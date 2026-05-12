/**
 * Full-GPU Wagner pipeline for the browser miner (v0.4).
 *
 * Browser parallel of `clients/gpu-miner/src/wagner.rs` — runs all
 * five Equihash 96,5 Wagner rounds plus the solution scan in
 * compute shaders, so the only CPU work per nonce is one params
 * write + a tiny readback at the end.
 *
 * The pipeline:
 *
 *   1. leaves.wgsl              → 131,072 12-byte leaves
 *   2. rounds.wgsl::init_rows   → expand into row layout (3 hash + 32 indices)
 *   3. rounds.wgsl::count_buckets } × 5 rounds, ping-ponging rows_a ↔ rows_b
 *   4. rounds.wgsl::pair_emit   }
 *   5. rounds.wgsl::solution_scan → emit rows whose final hash is all-zero
 *   6. readback ≤MAX_SOLUTIONS row records
 *
 * Both WGSL files are served from `/shaders/` (already in use by the
 * v0.3 hybrid path for leaves.wgsl). Validated end-to-end via the
 * same shader_ref tests in clients/gpu-miner — when the WGSL matches
 * the validated Rust port byte-for-byte, the browser path is correct
 * subject to driver bugs.
 *
 * Memory footprint: ~95 MB GPU resident (2× row buffers @ ~45 MB
 * each, plus leaves and bucket slot tables). Modest discrete + iGPUs
 * handle this easily; on adapters where the device limit denies the
 * allocation, `init()` returns null and miner-engine falls back to
 * the v0.3 hybrid path automatically.
 */

const N_INIT_LEAVES = 1 << 17; // 131,072
const LEAF_BYTES = 12;
const LEAVES_PER_BLAKE2B = 5;

/** Pick a workgroup size that matches the GPU's warp/wavefront
 * width. Matches the native miner's `pick_workgroup_size`. */
function pickWorkgroupSize(
  info: { vendor?: string; description?: string } | undefined
): number {
  const tag = `${info?.vendor ?? ""} ${info?.description ?? ""}`.toLowerCase();
  if (tag.includes("nvidia")) return 128;
  if (tag.includes("amd") || tag.includes("radeon")) return 64;
  if (tag.includes("apple")) return 64;
  if (tag.includes("intel")) return 32;
  return 64;
}

// Row layout constants (mirror rounds.wgsl)
const HASH_WORDS = 3;
const INDICES_MAX = 32;
const ROW_WORDS = 35;
const NUM_BUCKETS = 65_536;
const MAX_PER_BUCKET = 16;

/** Ping-pong row buffer capacity. Same value as the native miner
 * (2.5× input rows; well above the Poisson(2) tail). */
const MAX_ROWS = 320_000;

/** Max candidate solutions per nonce — for (96, 5) the expected
 * count is < 1, and we only ever surface a handful to the CPU. */
const MAX_SOLUTIONS = 64;

/** Bytes per row in the output buffer. */
const ROW_BYTES = ROW_WORDS * 4;

const LEAVES_PARAMS_BYTES = 160;
const ROUNDS_PARAMS_BYTES = 16;

const PERSONAL_BYTES = (() => {
  const b = new Uint8Array(16);
  b.set(new TextEncoder().encode("ZcashPoW"), 0);
  // n_le = 96, k_le = 5 as u32 LE
  b[8] = 96;
  b[12] = 5;
  return b;
})();

export interface WebGPUWagnerInfo {
  adapterName: string;
  isFallback: boolean;
}

export class WebGPUWagner {
  private device: GPUDevice;

  private leavesPipeline: GPUComputePipeline;
  private initRowsPipeline: GPUComputePipeline;
  private countPipeline: GPUComputePipeline;
  private pairPipeline: GPUComputePipeline;
  private solutionPipeline: GPUComputePipeline;

  // Persistent buffers
  private leavesParamsBuf: GPUBuffer;
  private leavesBuf: GPUBuffer;
  private roundsParamsBuf: GPUBuffer;
  private rowsA: GPUBuffer;
  private rowsB: GPUBuffer;
  private bucketCounts: GPUBuffer;
  private bucketSlots: GPUBuffer;
  private outCount: GPUBuffer;
  private stagingOutCount: GPUBuffer;
  private stagingSolutions: GPUBuffer;

  // Pre-built bind groups for each ping-pong direction.
  private bgInAOutB: GPUBindGroup;
  private bgInBOutA: GPUBindGroup;
  private bgLeaves: GPUBindGroup;

  /** Workgroup size baked into every pipeline via the
   * `override WG_SIZE` constant. */
  private wgSize: number;

  readonly info: WebGPUWagnerInfo;

  private constructor(
    device: GPUDevice,
    pipelines: {
      leaves: GPUComputePipeline;
      initRows: GPUComputePipeline;
      count: GPUComputePipeline;
      pair: GPUComputePipeline;
      solution: GPUComputePipeline;
    },
    buffers: {
      leavesParams: GPUBuffer;
      leaves: GPUBuffer;
      roundsParams: GPUBuffer;
      rowsA: GPUBuffer;
      rowsB: GPUBuffer;
      bucketCounts: GPUBuffer;
      bucketSlots: GPUBuffer;
      outCount: GPUBuffer;
      stagingOutCount: GPUBuffer;
      stagingSolutions: GPUBuffer;
    },
    bindGroups: {
      inAOutB: GPUBindGroup;
      inBOutA: GPUBindGroup;
      leaves: GPUBindGroup;
    },
    wgSize: number,
    info: WebGPUWagnerInfo
  ) {
    this.device = device;
    this.leavesPipeline = pipelines.leaves;
    this.initRowsPipeline = pipelines.initRows;
    this.countPipeline = pipelines.count;
    this.pairPipeline = pipelines.pair;
    this.solutionPipeline = pipelines.solution;
    this.leavesParamsBuf = buffers.leavesParams;
    this.leavesBuf = buffers.leaves;
    this.roundsParamsBuf = buffers.roundsParams;
    this.rowsA = buffers.rowsA;
    this.rowsB = buffers.rowsB;
    this.bucketCounts = buffers.bucketCounts;
    this.bucketSlots = buffers.bucketSlots;
    this.outCount = buffers.outCount;
    this.stagingOutCount = buffers.stagingOutCount;
    this.stagingSolutions = buffers.stagingSolutions;
    this.bgInAOutB = bindGroups.inAOutB;
    this.bgInBOutA = bindGroups.inBOutA;
    this.bgLeaves = bindGroups.leaves;
    this.wgSize = wgSize;
    this.info = info;
  }

  /**
   * Probe + initialize a WebGPU device and compile both shaders.
   * Returns null when WebGPU is unavailable, the adapter rejects our
   * limit requirements (~95 MB GPU memory + 7 storage bindings), or
   * any pipeline fails to compile. Caller should fall back to the
   * v0.3 hybrid path on null.
   */
  static async init(): Promise<WebGPUWagner | null> {
    if (typeof navigator === "undefined" || !navigator.gpu) {
      return null;
    }
    let adapter: GPUAdapter | null;
    try {
      adapter = await navigator.gpu.requestAdapter({
        powerPreference: "high-performance",
      });
    } catch {
      return null;
    }
    if (!adapter) return null;

    // We need 7 storage buffers per stage (rounds.wgsl) and a buffer
    // size that fits MAX_ROWS rows. Both are within the WebGPU
    // baseline; ask for them explicitly so we fail fast on hardware
    // that can't satisfy them.
    const requiredLimits: Record<string, number> = {
      maxStorageBuffersPerShaderStage: 7,
      maxStorageBufferBindingSize: 64 << 20, // 64 MB
    };
    let device: GPUDevice;
    try {
      device = await adapter.requestDevice({
        label: "equium-webgpu-wagner",
        requiredLimits,
      });
    } catch {
      return null;
    }

    const [leavesText, roundsText] = await Promise.all([
      fetch("/shaders/leaves.wgsl", { cache: "force-cache" }).then((r) =>
        r.ok ? r.text() : Promise.reject(new Error("leaves.wgsl"))
      ),
      fetch("/shaders/rounds.wgsl", { cache: "force-cache" }).then((r) =>
        r.ok ? r.text() : Promise.reject(new Error("rounds.wgsl"))
      ),
    ]).catch(() => [null, null] as const);
    if (!leavesText || !roundsText) {
      device.destroy();
      return null;
    }

    const leavesModule = device.createShaderModule({
      label: "leaves.wgsl",
      code: leavesText,
    });
    const roundsModule = device.createShaderModule({
      label: "rounds.wgsl",
      code: roundsText,
    });

    // Leaves binding layout: 1 uniform + 1 storage. Same as v0.3.
    const leavesBgl = device.createBindGroupLayout({
      label: "leaves.bgl",
      entries: [
        {
          binding: 0,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "uniform" },
        },
        {
          binding: 1,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "storage" },
        },
      ],
    });

    // Rounds binding layout: 1 uniform + 6 storage (rows_in,
    // rows_out, bucket_counts, bucket_slots, out_count, leaves).
    // Matches rounds.wgsl bindings 0..6.
    const roundsBgl = device.createBindGroupLayout({
      label: "rounds.bgl",
      entries: [
        {
          binding: 0,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "uniform" },
        },
        {
          binding: 1,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "read-only-storage" },
        },
        {
          binding: 2,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "storage" },
        },
        {
          binding: 3,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "storage" },
        },
        {
          binding: 4,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "storage" },
        },
        {
          binding: 5,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "storage" },
        },
        {
          binding: 6,
          visibility: GPUShaderStage.COMPUTE,
          buffer: { type: "read-only-storage" },
        },
      ],
    });

    // Pick workgroup size per adapter vendor and feed it through both
    // shaders' `override WG_SIZE`.
    const adapterInfo = (adapter as unknown as { info?: GPUAdapterInfo }).info;
    const wgSize = pickWorkgroupSize(adapterInfo);
    const constants = { WG_SIZE: wgSize };

    const leavesPipeline = device.createComputePipeline({
      label: "leaves.pipeline",
      layout: device.createPipelineLayout({ bindGroupLayouts: [leavesBgl] }),
      compute: { module: leavesModule, entryPoint: "main", constants },
    });

    const roundsLayout = device.createPipelineLayout({
      bindGroupLayouts: [roundsBgl],
    });
    const makeRoundPipeline = (entry: string, label: string) =>
      device.createComputePipeline({
        label,
        layout: roundsLayout,
        compute: { module: roundsModule, entryPoint: entry, constants },
      });
    const initRowsPipeline = makeRoundPipeline(
      "init_rows",
      "init_rows.pipeline"
    );
    const countPipeline = makeRoundPipeline("count_buckets", "count.pipeline");
    const pairPipeline = makeRoundPipeline("pair_emit", "pair.pipeline");
    const solutionPipeline = makeRoundPipeline(
      "solution_scan",
      "solution.pipeline"
    );

    // Allocate persistent buffers.
    const leavesParamsBuf = device.createBuffer({
      label: "leaves_params",
      size: LEAVES_PARAMS_BYTES,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    const leavesBuf = device.createBuffer({
      label: "leaves",
      size: N_INIT_LEAVES * LEAF_BYTES,
      usage: GPUBufferUsage.STORAGE,
    });
    const roundsParamsBuf = device.createBuffer({
      label: "rounds_params",
      size: ROUNDS_PARAMS_BYTES,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    const rowsByteLen = MAX_ROWS * ROW_BYTES;
    const rowsA = device.createBuffer({
      label: "rows_a",
      size: rowsByteLen,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC,
    });
    const rowsB = device.createBuffer({
      label: "rows_b",
      size: rowsByteLen,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC,
    });
    const bucketCounts = device.createBuffer({
      label: "bucket_counts",
      size: NUM_BUCKETS * 4,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
    });
    const bucketSlots = device.createBuffer({
      label: "bucket_slots",
      size: NUM_BUCKETS * MAX_PER_BUCKET * 4,
      usage: GPUBufferUsage.STORAGE,
    });
    const outCount = device.createBuffer({
      label: "out_count",
      size: 4,
      usage:
        GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST | GPUBufferUsage.COPY_SRC,
    });
    const stagingOutCount = device.createBuffer({
      label: "staging_out_count",
      size: 4,
      usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST,
    });
    const stagingSolutions = device.createBuffer({
      label: "staging_solutions",
      size: MAX_SOLUTIONS * ROW_BYTES,
      usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST,
    });

    const bgLeaves = device.createBindGroup({
      label: "leaves.bg",
      layout: leavesBgl,
      entries: [
        { binding: 0, resource: { buffer: leavesParamsBuf } },
        { binding: 1, resource: { buffer: leavesBuf } },
      ],
    });

    const makeRoundBg = (
      inBuf: GPUBuffer,
      outBuf: GPUBuffer,
      label: string
    ): GPUBindGroup =>
      device.createBindGroup({
        label,
        layout: roundsBgl,
        entries: [
          { binding: 0, resource: { buffer: roundsParamsBuf } },
          { binding: 1, resource: { buffer: inBuf } },
          { binding: 2, resource: { buffer: outBuf } },
          { binding: 3, resource: { buffer: bucketCounts } },
          { binding: 4, resource: { buffer: bucketSlots } },
          { binding: 5, resource: { buffer: outCount } },
          { binding: 6, resource: { buffer: leavesBuf } },
        ],
      });
    const bgInAOutB = makeRoundBg(rowsA, rowsB, "rounds.bg.a_to_b");
    const bgInBOutA = makeRoundBg(rowsB, rowsA, "rounds.bg.b_to_a");

    const name = adapterInfo?.description || adapterInfo?.vendor || "WebGPU";
    const isFallback =
      (adapter as unknown as { isFallbackAdapter?: boolean })
        .isFallbackAdapter === true;

    return new WebGPUWagner(
      device,
      {
        leaves: leavesPipeline,
        initRows: initRowsPipeline,
        count: countPipeline,
        pair: pairPipeline,
        solution: solutionPipeline,
      },
      {
        leavesParams: leavesParamsBuf,
        leaves: leavesBuf,
        roundsParams: roundsParamsBuf,
        rowsA,
        rowsB,
        bucketCounts,
        bucketSlots,
        outCount,
        stagingOutCount,
        stagingSolutions,
      },
      {
        inAOutB: bgInAOutB,
        inBOutA: bgInBOutA,
        leaves: bgLeaves,
      },
      wgSize,
      { adapterName: `${name} (wg=${wgSize})`, isFallback }
    );
  }

  private writeLeavesParams(input: Uint8Array, nonce: Uint8Array): void {
    if (input.length !== 81)
      throw new Error(`leaves: input must be 81 bytes, got ${input.length}`);
    if (nonce.length !== 32)
      throw new Error(`leaves: nonce must be 32 bytes, got ${nonce.length}`);
    const params = new Uint8Array(LEAVES_PARAMS_BYTES);
    params.set(PERSONAL_BYTES, 0);
    const cfg = new DataView(params.buffer, 16, 16);
    cfg.setUint32(0, 60, true); // digest_len
    cfg.setUint32(4, N_INIT_LEAVES, true);
    params.set(input, 32); // input @ +32 padded to 96
    params.set(nonce, 128); // nonce_lo + nonce_hi @ +128
    this.device.queue.writeBuffer(this.leavesParamsBuf, 0, params);
  }

  private writeRoundsParams(nRows: number, indicesCountIn: number): void {
    const buf = new Uint8Array(ROUNDS_PARAMS_BYTES);
    const dv = new DataView(buf.buffer);
    dv.setUint32(0, nRows, true);
    dv.setUint32(4, MAX_ROWS, true);
    dv.setUint32(8, indicesCountIn, true);
    dv.setUint32(12, 0, true);
    this.device.queue.writeBuffer(this.roundsParamsBuf, 0, buf);
  }

  /** Read out_count via the staging buffer. Costs one submit + one
   * mapAsync stall; called once per Wagner round to gate the next
   * dispatch's grid size. */
  private async readbackOutCount(): Promise<number> {
    const enc = this.device.createCommandEncoder({
      label: "readback_count.enc",
    });
    enc.copyBufferToBuffer(this.outCount, 0, this.stagingOutCount, 0, 4);
    this.device.queue.submit([enc.finish()]);
    await this.stagingOutCount.mapAsync(GPUMapMode.READ, 0, 4);
    const view = new Uint32Array(this.stagingOutCount.getMappedRange(0, 4).slice(0));
    const count = view[0];
    this.stagingOutCount.unmap();
    return count;
  }

  /**
   * Run leaves + init_rows + 5 rounds + solution_scan for one
   * (input, nonce). Returns the raw 32-index candidate solutions —
   * caller must compress + re-validate via `is_valid_solution`
   * before submitting.
   */
  async runNonce(
    input: Uint8Array,
    nonce: Uint8Array
  ): Promise<Uint32Array[]> {
    this.writeLeavesParams(input, nonce);

    // Step 1: leaves
    {
      const enc = this.device.createCommandEncoder({ label: "step1_leaves.enc" });
      const pass = enc.beginComputePass({ label: "leaves.pass" });
      pass.setPipeline(this.leavesPipeline);
      pass.setBindGroup(0, this.bgLeaves);
      const nCalls = Math.ceil(N_INIT_LEAVES / LEAVES_PER_BLAKE2B);
      pass.dispatchWorkgroups(Math.ceil(nCalls / this.wgSize), 1, 1);
      pass.end();
      this.device.queue.submit([enc.finish()]);
    }

    // Step 2: init_rows — writes into rows_a using bg_in_b_out_a
    // (rows_in binding is ignored by init_rows, rows_out=A).
    this.writeRoundsParams(N_INIT_LEAVES, 1);
    {
      const enc = this.device.createCommandEncoder({ label: "step2_init.enc" });
      const pass = enc.beginComputePass({ label: "init_rows.pass" });
      pass.setPipeline(this.initRowsPipeline);
      pass.setBindGroup(0, this.bgInBOutA);
      pass.dispatchWorkgroups(
        Math.ceil(N_INIT_LEAVES / this.wgSize),
        1,
        1
      );
      pass.end();
      this.device.queue.submit([enc.finish()]);
    }

    // Step 3: 5 Wagner rounds, ping-ponging rows_a ↔ rows_b.
    let nRowsCurrent = N_INIT_LEAVES;
    for (let round = 0; round < 5; round++) {
      const indicesCountIn = 1 << round;
      this.writeRoundsParams(nRowsCurrent, indicesCountIn);
      const bg = round % 2 === 0 ? this.bgInAOutB : this.bgInBOutA;

      const enc = this.device.createCommandEncoder({
        label: `round_${round}.enc`,
      });
      enc.clearBuffer(this.bucketCounts);
      enc.clearBuffer(this.outCount);

      {
        const pass = enc.beginComputePass({ label: `round_${round}.count` });
        pass.setPipeline(this.countPipeline);
        pass.setBindGroup(0, bg);
        pass.dispatchWorkgroups(
          Math.ceil(nRowsCurrent / this.wgSize),
          1,
          1
        );
        pass.end();
      }
      {
        const pass = enc.beginComputePass({ label: `round_${round}.pair` });
        pass.setPipeline(this.pairPipeline);
        pass.setBindGroup(0, bg);
        pass.dispatchWorkgroups(
          Math.ceil(nRowsCurrent / this.wgSize),
          1,
          1
        );
        pass.end();
      }
      this.device.queue.submit([enc.finish()]);

      const nextN = await this.readbackOutCount();
      if (nextN === 0) return [];
      if (nextN > MAX_ROWS) {
        throw new Error(`round ${round} overflow: ${nextN} > ${MAX_ROWS}`);
      }
      nRowsCurrent = nextN;
    }

    // After 5 rounds the survivors live in rows_b (round 4 wrote to
    // B). Solution_scan reads B → writes A (treating rows_a as the
    // solutions output).
    this.writeRoundsParams(nRowsCurrent, 32);
    {
      const enc = this.device.createCommandEncoder({ label: "solution.enc" });
      enc.clearBuffer(this.outCount);
      const pass = enc.beginComputePass({ label: "solution.pass" });
      pass.setPipeline(this.solutionPipeline);
      pass.setBindGroup(0, this.bgInBOutA);
      pass.dispatchWorkgroups(
        Math.ceil(nRowsCurrent / this.wgSize),
        1,
        1
      );
      pass.end();
      enc.copyBufferToBuffer(
        this.rowsA,
        0,
        this.stagingSolutions,
        0,
        MAX_SOLUTIONS * ROW_BYTES
      );
      this.device.queue.submit([enc.finish()]);
    }

    const nSol = Math.min(await this.readbackOutCount(), MAX_SOLUTIONS);
    if (nSol === 0) return [];

    await this.stagingSolutions.mapAsync(
      GPUMapMode.READ,
      0,
      nSol * ROW_BYTES
    );
    const data = new Uint8Array(
      this.stagingSolutions.getMappedRange(0, nSol * ROW_BYTES).slice(0)
    );
    this.stagingSolutions.unmap();

    const out: Uint32Array[] = [];
    for (let i = 0; i < nSol; i++) {
      const base = i * ROW_BYTES + HASH_WORDS * 4;
      const indices = new Uint32Array(INDICES_MAX);
      for (let k = 0; k < INDICES_MAX; k++) {
        const off = base + k * 4;
        indices[k] =
          data[off] |
          (data[off + 1] << 8) |
          (data[off + 2] << 16) |
          (data[off + 3] << 24);
      }
      out.push(indices);
    }
    return out;
  }

  /** Release GPU resources. Call when the miner stops. */
  destroy(): void {
    try {
      this.leavesParamsBuf.destroy();
      this.leavesBuf.destroy();
      this.roundsParamsBuf.destroy();
      this.rowsA.destroy();
      this.rowsB.destroy();
      this.bucketCounts.destroy();
      this.bucketSlots.destroy();
      this.outCount.destroy();
      this.stagingOutCount.destroy();
      this.stagingSolutions.destroy();
    } catch {}
    this.device.destroy();
  }
}
