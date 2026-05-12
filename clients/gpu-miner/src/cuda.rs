//! CUDA backend — bypasses the Vulkan/SPIR-V stack entirely so we
//! don't trip the libnvidia-glvkspirv.so crash on driver branches
//! 555–575.
//!
//! Same shape as `gpu.rs::GpuLeafGen` (init + generate) so it slots
//! into the probe ladder without the call sites caring which backend
//! actually ran the kernel. The CUDA C++ source lives in
//! `clients/gpu-miner/cuda/leaves.cu`; `build.rs` compiles it to
//! PTX via nvcc and exports the path through
//! `EQUIUM_PTX_LEAVES`. cudarc loads that PTX at runtime via the
//! Driver API.
//!
//! Algorithm parity: byte-for-byte identical to
//! `shader_ref::leaves` (which the `cargo test` round-by-round
//! suite validates against blake2b_simd). The WGSL and CUDA
//! kernels are independent implementations of the same spec; if
//! one drifts the divergence shows up immediately in
//! `verify-cpu` and `verify`.

use anyhow::{anyhow, Context, Result};
use cudarc::driver::{CudaContext, CudaFunction, CudaModule, LaunchConfig, PushKernelArg};
use std::sync::Arc;

pub const LEAF_BYTES: usize = 12;
pub const LEAVES_PER_CALL: u32 = 5;
const PARAMS_BYTES: usize = 160;
const DEFAULT_BLOCK: u32 = 256; // CUDA's standard for memory-bound kernels

const PERSONAL: [u8; 16] = {
    let mut p = [0u8; 16];
    p[0] = b'Z';
    p[1] = b'c';
    p[2] = b'a';
    p[3] = b's';
    p[4] = b'h';
    p[5] = b'P';
    p[6] = b'o';
    p[7] = b'W';
    // n_le = 96 (u32 LE)
    p[8] = 96;
    // k_le = 5 (u32 LE)
    p[12] = 5;
    p
};

pub struct CudaLeafGen {
    ctx: Arc<CudaContext>,
    _module: Arc<CudaModule>,
    kernel: CudaFunction,
    pub adapter_name: String,
}

impl CudaLeafGen {
    /// Probe + initialize a CUDA device 0 and load the BLAKE2b PTX.
    /// Returns Err on any failure — the auto-probe ladder treats
    /// that as "this backend not available" and falls through to
    /// Vulkan / GL.
    pub fn new() -> Result<Self> {
        let ctx = CudaContext::new(0).map_err(|e| anyhow!("CUDA init: {e:?}"))?;
        let device_name = ctx
            .name()
            .map_err(|e| anyhow!("CUDA device name: {e:?}"))?;

        // The PTX is compiled by build.rs and the path is shoved
        // into a cargo env var. When the `cuda` feature is off,
        // this file isn't compiled and the env var isn't set —
        // see the cfg-gating in main.rs.
        let ptx = include_str!(env!("EQUIUM_PTX_LEAVES"));
        let module = ctx
            .load_module(ptx.into())
            .map_err(|e| anyhow!("CUDA load_module: {e:?}"))?;
        let kernel = module
            .load_function("leaves_kernel")
            .map_err(|e| anyhow!("CUDA load_function leaves_kernel: {e:?}"))?;

        Ok(Self {
            ctx,
            _module: module,
            kernel,
            adapter_name: format!("{device_name} (CUDA)"),
        })
    }

    /// Generate `n_leaves` 12-byte leaves into `out`. `out.len()` must
    /// be at least `n_leaves * LEAF_BYTES`. Mirrors `gpu::GpuLeafGen::generate`.
    pub fn generate(
        &self,
        input: &[u8; 81],
        nonce: &[u8; 32],
        n_leaves: u32,
        out: &mut [u8],
    ) -> Result<()> {
        let leaves_bytes = (n_leaves as usize) * LEAF_BYTES;
        if out.len() < leaves_bytes {
            return Err(anyhow!(
                "leaves out buf too small: {} < {leaves_bytes}",
                out.len()
            ));
        }

        // Pack the uniform block: same 160-byte layout the WGSL
        // shader uses, since both kernels read it byte-by-byte
        // through `pack_le` and friends.
        let mut params = [0u8; PARAMS_BYTES];
        params[..16].copy_from_slice(&PERSONAL);
        params[16..20].copy_from_slice(&60u32.to_le_bytes()); // digest_len
        params[20..24].copy_from_slice(&n_leaves.to_le_bytes());
        params[32..32 + 81].copy_from_slice(input);
        params[128..128 + 32].copy_from_slice(nonce);

        let stream = self.ctx.default_stream();

        // Allocate + upload params.
        let params_d = stream
            .memcpy_stod(&params)
            .map_err(|e| anyhow!("htod params: {e:?}"))?;
        let mut leaves_d = stream
            .alloc_zeros::<u8>(leaves_bytes)
            .map_err(|e| anyhow!("alloc leaves: {e:?}"))?;

        // Launch: one thread per BLAKE2b call (= 5 leaves).
        let n_calls = (n_leaves + LEAVES_PER_CALL - 1) / LEAVES_PER_CALL;
        let grid_dim = (n_calls + DEFAULT_BLOCK - 1) / DEFAULT_BLOCK;
        let cfg = LaunchConfig {
            grid_dim: (grid_dim, 1, 1),
            block_dim: (DEFAULT_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut launcher = stream.launch_builder(&self.kernel);
        launcher
            .arg(&params_d)
            .arg(&mut leaves_d)
            .arg(&n_leaves);
        unsafe { launcher.launch(cfg) }
            .map_err(|e| anyhow!("launch leaves_kernel: {e:?}"))?;
        stream
            .synchronize()
            .map_err(|e| anyhow!("stream sync: {e:?}"))?;

        // Read leaves back to host.
        stream
            .memcpy_dtoh(&leaves_d, &mut out[..leaves_bytes])
            .map_err(|e| anyhow!("dtoh leaves: {e:?}"))?;
        Ok(())
    }
}
