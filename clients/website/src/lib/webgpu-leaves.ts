/**
 * WebGPU Equihash leaf-generation pipeline (v0.3 browser miner).
 *
 * Browser parallel of `clients/gpu-miner/src/gpu.rs::GpuLeafGen`. Loads
 * the same `leaves.wgsl` shader the native miner uses (served from
 * `/shaders/leaves.wgsl`) and produces 131,072 12-byte leaves per
 * `generate()` call.
 *
 * Pairs with the worker's new `solve-with-leaves` message: the main
 * thread generates leaves on the GPU, then ships them to a CPU worker
 * which runs Wagner + target check. Hybrid path = browser parallel of
 * native v0.1.
 *
 * If `navigator.gpu` is missing (Firefox stable, Safari < 18 in some
 * configurations) or `requestAdapter` returns null, the caller falls
 * back to the legacy pure-WASM path automatically.
 */

const N_INIT_LEAVES = 1 << 17; // 131,072 for Equihash 96,5
const LEAF_BYTES = 12;
const LEAVES_PER_BLAKE2B = 5; // 60-byte digest → 5 leaves

/** Pick a workgroup size that matches the GPU's warp/wavefront
 * width. Same heuristic as the native miner (clients/gpu-miner/
 * src/gpu.rs::pick_workgroup_size). */
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

/**
 * Uniform layout mirrors gpu.rs::Params exactly (160 bytes, all
 * vec4-aligned).
 *
 *   [0..16]    personal:  u32×4   "ZcashPoW" + n_le + k_le
 *   [16..32]   cfg:       u32×4   [digest_len, n_leaves, _, _]
 *   [32..128]  input:     u32×24  I-block (81 bytes padded to 96)
 *   [128..144] nonce_lo:  u32×4   nonce bytes 0..16
 *   [144..160] nonce_hi:  u32×4   nonce bytes 16..32
 */
const PARAMS_BYTES = 160;

const PERSONAL_BYTES = (() => {
  const b = new Uint8Array(16);
  const enc = new TextEncoder().encode("ZcashPoW");
  b.set(enc, 0);
  // n_le = 96 (u32 LE)
  b[8] = 96;
  b[9] = 0;
  b[10] = 0;
  b[11] = 0;
  // k_le = 5 (u32 LE)
  b[12] = 5;
  b[13] = 0;
  b[14] = 0;
  b[15] = 0;
  return b;
})();

export interface WebGPULeavesInfo {
  /** Adapter name (e.g. "Apple M2 Pro"). Best-effort — browsers
   * usually mask this for privacy and return an empty string. */
  adapterName: string;
  /** Whether the adapter was returned with a software fallback flag. */
  isFallback: boolean;
}

export class WebGPULeaves {
  private device: GPUDevice;
  private pipeline: GPUComputePipeline;
  private paramsBuf: GPUBuffer;
  private leavesBuf: GPUBuffer;
  private stagingBuf: GPUBuffer;
  private bindGroup: GPUBindGroup;
  /** Workgroup size baked into the compiled pipeline via the
   * `override WG_SIZE` constant. Used to compute dispatch grid. */
  private wgSize: number;
  readonly info: WebGPULeavesInfo;

  private constructor(
    device: GPUDevice,
    pipeline: GPUComputePipeline,
    paramsBuf: GPUBuffer,
    leavesBuf: GPUBuffer,
    stagingBuf: GPUBuffer,
    bindGroup: GPUBindGroup,
    wgSize: number,
    info: WebGPULeavesInfo
  ) {
    this.device = device;
    this.pipeline = pipeline;
    this.paramsBuf = paramsBuf;
    this.leavesBuf = leavesBuf;
    this.stagingBuf = stagingBuf;
    this.bindGroup = bindGroup;
    this.wgSize = wgSize;
    this.info = info;
  }

  /**
   * Probe + initialize a WebGPU device and compile the leaves shader.
   * Returns null when WebGPU is unavailable or adapter request fails —
   * caller should fall back to the WASM-only path.
   */
  static async init(): Promise<WebGPULeaves | null> {
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

    let device: GPUDevice;
    try {
      device = await adapter.requestDevice({ label: "equium-webgpu-leaves" });
    } catch {
      return null;
    }

    let shaderText: string;
    try {
      const res = await fetch("/shaders/leaves.wgsl", { cache: "force-cache" });
      if (!res.ok) throw new Error(`shader fetch ${res.status}`);
      shaderText = await res.text();
    } catch {
      device.destroy();
      return null;
    }

    const module = device.createShaderModule({
      label: "leaves.wgsl",
      code: shaderText,
    });

    const bindGroupLayout = device.createBindGroupLayout({
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

    // Pick workgroup size from adapter info before pipeline create —
    // override constants are resolved at compile time.
    const adapterInfo = (adapter as unknown as { info?: GPUAdapterInfo }).info;
    const wgSize = pickWorkgroupSize(adapterInfo);

    const pipeline = device.createComputePipeline({
      label: "leaves.pipeline",
      layout: device.createPipelineLayout({
        bindGroupLayouts: [bindGroupLayout],
      }),
      compute: {
        module,
        entryPoint: "main",
        constants: { WG_SIZE: wgSize },
      },
    });

    const paramsBuf = device.createBuffer({
      label: "leaves.params",
      size: PARAMS_BYTES,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    const leavesBytes = N_INIT_LEAVES * LEAF_BYTES;
    const leavesBuf = device.createBuffer({
      label: "leaves",
      size: leavesBytes,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC,
    });
    const stagingBuf = device.createBuffer({
      label: "leaves.staging",
      size: leavesBytes,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });

    const bindGroup = device.createBindGroup({
      label: "leaves.bg",
      layout: bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: paramsBuf } },
        { binding: 1, resource: { buffer: leavesBuf } },
      ],
    });

    // Browsers tend to mask adapter.info for fingerprinting reasons; we
    // surface what's available and otherwise label it generically.
    const name = adapterInfo?.description || adapterInfo?.vendor || "WebGPU";
    const isFallback =
      (adapter as unknown as { isFallbackAdapter?: boolean })
        .isFallbackAdapter === true;

    return new WebGPULeaves(
      device,
      pipeline,
      paramsBuf,
      leavesBuf,
      stagingBuf,
      bindGroup,
      wgSize,
      { adapterName: `${name} (wg=${wgSize})`, isFallback }
    );
  }

  /**
   * Generate all 131,072 leaves for one (input, nonce) pair. Returns a
   * fresh Uint8Array sized `N_INIT_LEAVES * 12` ready to ship to a
   * Wagner worker.
   *
   * @throws if the GPU dispatch fails or the device is lost.
   */
  async generate(input: Uint8Array, nonce: Uint8Array): Promise<Uint8Array> {
    if (input.length !== 81) {
      throw new Error(`leaves: input must be 81 bytes, got ${input.length}`);
    }
    if (nonce.length !== 32) {
      throw new Error(`leaves: nonce must be 32 bytes, got ${nonce.length}`);
    }

    // Pack params: personal (16) + cfg (16) + input padded to 96 +
    // nonce_lo (16) + nonce_hi (16) = 160 bytes.
    const params = new Uint8Array(PARAMS_BYTES);
    params.set(PERSONAL_BYTES, 0);
    // cfg: digest_len=60, n_leaves=N_INIT_LEAVES
    const cfgView = new DataView(params.buffer, 16, 16);
    cfgView.setUint32(0, 60, true);
    cfgView.setUint32(4, N_INIT_LEAVES, true);
    // input padded to 96 bytes starting at offset 32
    params.set(input, 32);
    // nonce_lo + nonce_hi starting at offset 128
    params.set(nonce, 128);

    this.device.queue.writeBuffer(this.paramsBuf, 0, params);

    const encoder = this.device.createCommandEncoder({ label: "leaves.enc" });
    {
      const pass = encoder.beginComputePass({ label: "leaves.pass" });
      pass.setPipeline(this.pipeline);
      pass.setBindGroup(0, this.bindGroup);
      const nCalls = Math.ceil(N_INIT_LEAVES / LEAVES_PER_BLAKE2B);
      const workgroups = Math.ceil(nCalls / this.wgSize);
      pass.dispatchWorkgroups(workgroups, 1, 1);
      pass.end();
    }
    const bytes = N_INIT_LEAVES * LEAF_BYTES;
    encoder.copyBufferToBuffer(this.leavesBuf, 0, this.stagingBuf, 0, bytes);
    this.device.queue.submit([encoder.finish()]);

    await this.stagingBuf.mapAsync(GPUMapMode.READ, 0, bytes);
    // Copy out of the mapped range — once we unmap, the view becomes
    // invalid. The fresh Uint8Array can be safely transferred to a
    // worker afterwards.
    const mapped = new Uint8Array(this.stagingBuf.getMappedRange(0, bytes));
    const out = new Uint8Array(bytes);
    out.set(mapped);
    this.stagingBuf.unmap();
    return out;
  }

  /**
   * Tear down the GPU device. Call when the user stops the miner so
   * the browser can reclaim VRAM.
   */
  destroy(): void {
    this.paramsBuf.destroy();
    this.leavesBuf.destroy();
    this.stagingBuf.destroy();
    this.device.destroy();
  }
}
