# Equium GPU miner

Hybrid GPU/CPU miner for Equium ($EQM). GPU does the BLAKE2b-heavy
leaf-generation pass (≈70% of solver time in the CPU profile); CPU
handles Wagner rounds.

Cross-platform via `wgpu`:

| OS | Backend | Status |
|----|---------|--------|
| macOS (Apple Silicon + Intel) | Metal | should work |
| Linux | Vulkan | should work with mesa or proprietary drivers |
| Windows | DX12 or Vulkan | should work |
| Web | WebGPU | planned, same shader |

## Status: v0.1 — hybrid GPU/CPU miner

What's here today:

- `equium-gpu-miner verify-cpu` — validates shader logic against
  blake2b_simd without needing a GPU. Run this first; it should
  always pass.
- `equium-gpu-miner verify` — runs BLAKE2b leaf generation on the
  actual GPU and compares to CPU reference. Requires GPU hardware.
- `equium-gpu-miner bench` — measures leaf throughput on your GPU.
- `equium-gpu-miner mine` — **full mining loop**. GPU does BLAKE2b
  leaf generation, CPU runs Wagner rounds, races across `--threads`
  workers for below-target nonces, submits to chain.

What's not here yet:

- Wagner rounds on GPU. Out of scope for v0.x — that's a bigger
  compute-shader effort (sort, XOR-and-pair, ×5 rounds). Even
  hybrid v0.1 is meaningfully faster than pure-CPU because BLAKE2b
  is ~70% of the per-attempt work.

## Mine

```bash
cargo build --release -p equium-gpu-miner

./target/release/equium-gpu-miner mine \
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \
  --keypair ~/.config/solana/id.json
```

The mining loop spawns `--threads N` CPU workers (default = num_cpus).
Each worker grinds random nonces, dispatches a leaf-generation kernel
to the shared GPU, runs Wagner rounds on its own CPU thread, and
races to find a below-target solution. First worker wins the round;
the others abort and the next round starts.

## Roadmap

- **v0** ✓ GPU BLAKE2b leaf generation + CPU verification harness.
- **v0.1** ✓ Hybrid mining loop (this version). GPU leaves → CPU
  Wagner → race-for-below-target → submit.
- **v0.2**: First Wagner round on GPU (sort + XOR-and-pair).
- **v0.3**: All 5 Wagner rounds on GPU.
- **v0.4**: WebGPU integration into the browser miner.

## Build

Standard Cargo:

```bash
cargo build --release -p equium-gpu-miner
```

GPU drivers are loaded at runtime via `wgpu`. No CUDA toolchain or
admin install needed — anything that supports Vulkan, Metal, or DX12
works.

## Verify

After building on a new machine, run the verification step. This
exists because subtle shader bugs (alignment, endianness, off-by-one)
can pass `cargo check` and still produce wrong bytes on real
hardware. The check is fast (<1s for the default 2048 leaves) and
compares GPU output to `blake2b_simd` byte-for-byte.

```bash
./target/release/equium-gpu-miner verify
```

Backend selection is automatic: CUDA first when built with
`--features cuda` on an NVIDIA host, then Metal on macOS, DX12 on
native Windows, and Vulkan → GL on Linux/WSL. Override with
`EQUIUM_BACKEND=cuda|metal|dx12|vulkan|gl` only when you need to force
a specific path.

On macOS, run the GPU check from a logged-in Terminal session. Headless
CI or SSH-only sessions can report `Metal: Supported` in
`system_profiler` while `MTLCreateSystemDefaultDevice()` still returns
no usable device to the process.

If it prints `✓ GPU output matches CPU reference byte-for-byte`,
you're good. If it prints a mismatch, please open a GitHub issue
with the first-mismatch hex output and your `--info` (we'll add a
flag for that) so we can chase the driver-specific case.

## Bench

```bash
./target/release/equium-gpu-miner bench
```

Reports BLAKE2b throughput at full Equihash width. Useful for
comparing GPUs and for confirming the GPU is actually being used
(software fallback through `lavapipe` runs orders of magnitude
slower than a real adapter).

## Why hybrid v0 instead of going full GPU first

Equihash 96,5 Wagner on GPU is genuinely hard — bucket sort + XOR-and-
pair at 2^17 leaves with proper memory bandwidth is its own multi-week
project. The leaf-generation pass alone is ~70% of CPU solver time
according to profiling, so even before Wagner gets ported the GPU
should give a meaningful speedup over the multi-threaded CPU CLI.

Verifying byte-level correctness of the BLAKE2b shader first means
that when we add the Wagner kernels we already know the leaves they
consume match the CPU reference; debugging the Wagner code can assume
the input layer is sound.

## Source

Shader: `src/shaders/leaves.wgsl`
Host: `src/gpu.rs`, `src/main.rs`
