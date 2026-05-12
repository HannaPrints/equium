//! Full-GPU Wagner pipeline on CUDA — the perf path that bypasses
//! wgpu/SPIR-V entirely. Same control flow as `wagner.rs::GpuWagner`
//! (which uses wgpu), wired to the CUDA C++ kernels in
//! `clients/gpu-miner/cuda/{leaves,rounds}.cu`.
//!
//! Per nonce:
//!   1. write leaves params (160 B) + nonce
//!   2. launch leaves kernel → leaves_buf
//!   3. launch init_rows → rows_a
//!   4. 5× round:
//!        - clear bucket_counts + out_count
//!        - launch count_buckets (rows_X → bucket_counts/slots)
//!        - launch pair_emit (rows_X → rows_Y, atomic out_count)
//!        - readback out_count to drive next round's grid + indices_count
//!   5. clear out_count, launch solution_scan → rows_A
//!   6. readback solution_count + first N solution rows
//!
//! Correctness: each kernel matches its rounds.wgsl counterpart
//! byte-for-byte; `verify-rounds` does an on-device check against
//! `shader_ref::round_kernel` at full (96, 5) width.

use crate::cuda::{LEAF_BYTES, LEAVES_PER_CALL};
use anyhow::{anyhow, Result};
use cudarc::driver::{
    CudaContext, CudaFunction, CudaModule, CudaSlice, LaunchConfig, PushKernelArg,
};
use std::sync::Arc;

pub const N_INIT_LEAVES: u32 = 1 << 17; // 131,072
const HASH_WORDS: u32 = 3;
const INDICES_MAX: u32 = 32;
const ROW_WORDS: u32 = 35;
const NUM_BUCKETS: u32 = 65_536;
const MAX_PER_BUCKET: u32 = 16;
/// Ping-pong row buffer capacity. Same value as wagner.rs (2.5× input;
/// well above the Poisson(2) tail for full-width (96, 5)).
const MAX_ROWS: u32 = 320_000;
const MAX_SOLUTIONS: u32 = 64;
const ROW_BYTES: u32 = ROW_WORDS * 4;
const LEAVES_PARAMS_BYTES: usize = 160;
const ROUNDS_PARAMS_BYTES: usize = 16;
const DEFAULT_BLOCK: u32 = 256;

const PERSONAL: [u8; 16] = {
    let mut p = [0u8; 16];
    p[0] = b'Z'; p[1] = b'c'; p[2] = b'a'; p[3] = b's';
    p[4] = b'h'; p[5] = b'P'; p[6] = b'o'; p[7] = b'W';
    p[8] = 96;   // n_le u32
    p[12] = 5;   // k_le u32
    p
};

pub struct CudaWagner {
    ctx: Arc<CudaContext>,

    // Module handles kept alive so CUDA's refcount holds the loaded
    // PTX in memory for the lifetime of the kernel functions.
    _leaves_mod: Arc<CudaModule>,
    _rounds_mod: Arc<CudaModule>,

    leaves_fn: CudaFunction,
    init_rows_fn: CudaFunction,
    count_fn: CudaFunction,
    pair_fn: CudaFunction,
    solution_fn: CudaFunction,

    // Persistent device buffers.
    leaves_params_d: CudaSlice<u8>,
    leaves_buf_d: CudaSlice<u8>,
    rounds_params_d: CudaSlice<u8>,
    rows_a_d: CudaSlice<u8>,
    rows_b_d: CudaSlice<u8>,
    bucket_counts_d: CudaSlice<u8>,
    bucket_slots_d: CudaSlice<u8>,
    out_count_d: CudaSlice<u8>,

    // Host-side helpers — small fixed buffers we reuse to zero
    // bucket_counts + out_count between rounds.
    zero_counts: Vec<u8>,
    zero4: [u8; 4],

    pub adapter_name: String,
}

impl CudaWagner {
    pub fn new() -> Result<Self> {
        let ctx = CudaContext::new(0).map_err(|e| anyhow!("CUDA init: {e:?}"))?;
        let device_name = ctx.name().map_err(|e| anyhow!("CUDA device name: {e:?}"))?;

        let leaves_ptx = include_str!(env!("EQUIUM_PTX_LEAVES"));
        let rounds_ptx = include_str!(env!("EQUIUM_PTX_ROUNDS"));
        let leaves_mod = ctx
            .load_module(leaves_ptx.into())
            .map_err(|e| anyhow!("CUDA load leaves PTX: {e:?}"))?;
        let rounds_mod = ctx
            .load_module(rounds_ptx.into())
            .map_err(|e| anyhow!("CUDA load rounds PTX: {e:?}"))?;

        let leaves_fn = leaves_mod
            .load_function("leaves_kernel")
            .map_err(|e| anyhow!("load leaves_kernel: {e:?}"))?;
        let init_rows_fn = rounds_mod
            .load_function("init_rows")
            .map_err(|e| anyhow!("load init_rows: {e:?}"))?;
        let count_fn = rounds_mod
            .load_function("count_buckets")
            .map_err(|e| anyhow!("load count_buckets: {e:?}"))?;
        let pair_fn = rounds_mod
            .load_function("pair_emit")
            .map_err(|e| anyhow!("load pair_emit: {e:?}"))?;
        let solution_fn = rounds_mod
            .load_function("solution_scan")
            .map_err(|e| anyhow!("load solution_scan: {e:?}"))?;

        let stream = ctx.default_stream();
        let leaves_params_d = stream
            .alloc_zeros::<u8>(LEAVES_PARAMS_BYTES)
            .map_err(|e| anyhow!("alloc leaves_params: {e:?}"))?;
        let leaves_buf_d = stream
            .alloc_zeros::<u8>((N_INIT_LEAVES as usize) * LEAF_BYTES)
            .map_err(|e| anyhow!("alloc leaves: {e:?}"))?;
        let rounds_params_d = stream
            .alloc_zeros::<u8>(ROUNDS_PARAMS_BYTES)
            .map_err(|e| anyhow!("alloc rounds_params: {e:?}"))?;
        let rows_bytes = (MAX_ROWS as usize) * (ROW_BYTES as usize);
        let rows_a_d = stream
            .alloc_zeros::<u8>(rows_bytes)
            .map_err(|e| anyhow!("alloc rows_a: {e:?}"))?;
        let rows_b_d = stream
            .alloc_zeros::<u8>(rows_bytes)
            .map_err(|e| anyhow!("alloc rows_b: {e:?}"))?;
        let bucket_counts_d = stream
            .alloc_zeros::<u8>((NUM_BUCKETS as usize) * 4)
            .map_err(|e| anyhow!("alloc bucket_counts: {e:?}"))?;
        let bucket_slots_d = stream
            .alloc_zeros::<u8>((NUM_BUCKETS as usize) * (MAX_PER_BUCKET as usize) * 4)
            .map_err(|e| anyhow!("alloc bucket_slots: {e:?}"))?;
        let out_count_d = stream
            .alloc_zeros::<u8>(4)
            .map_err(|e| anyhow!("alloc out_count: {e:?}"))?;

        Ok(Self {
            ctx,
            _leaves_mod: leaves_mod,
            _rounds_mod: rounds_mod,
            leaves_fn,
            init_rows_fn,
            count_fn,
            pair_fn,
            solution_fn,
            leaves_params_d,
            leaves_buf_d,
            rounds_params_d,
            rows_a_d,
            rows_b_d,
            bucket_counts_d,
            bucket_slots_d,
            out_count_d,
            zero_counts: vec![0u8; (NUM_BUCKETS as usize) * 4],
            zero4: [0u8; 4],
            adapter_name: format!("{device_name} (CUDA)"),
        })
    }

    fn write_leaves_params(&mut self, input: &[u8; 81], nonce: &[u8; 32]) -> Result<()> {
        let mut p = [0u8; LEAVES_PARAMS_BYTES];
        p[..16].copy_from_slice(&PERSONAL);
        p[16..20].copy_from_slice(&60u32.to_le_bytes()); // digest_len
        p[20..24].copy_from_slice(&N_INIT_LEAVES.to_le_bytes());
        p[32..32 + 81].copy_from_slice(input);
        p[128..128 + 32].copy_from_slice(nonce);
        let stream = self.ctx.default_stream();
        stream
            .memcpy_htod(&p, &mut self.leaves_params_d)
            .map_err(|e| anyhow!("htod leaves params: {e:?}"))
    }

    fn write_rounds_params(&mut self, n_rows: u32, indices_count_in: u32) -> Result<()> {
        let mut p = [0u8; ROUNDS_PARAMS_BYTES];
        p[0..4].copy_from_slice(&n_rows.to_le_bytes());
        p[4..8].copy_from_slice(&MAX_ROWS.to_le_bytes());
        p[8..12].copy_from_slice(&indices_count_in.to_le_bytes());
        let stream = self.ctx.default_stream();
        stream
            .memcpy_htod(&p, &mut self.rounds_params_d)
            .map_err(|e| anyhow!("htod rounds params: {e:?}"))
    }

    fn readback_out_count(&self) -> Result<u32> {
        let stream = self.ctx.default_stream();
        let mut h = [0u8; 4];
        stream
            .memcpy_dtoh(&self.out_count_d, &mut h)
            .map_err(|e| anyhow!("dtoh out_count: {e:?}"))?;
        stream.synchronize().map_err(|e| anyhow!("sync: {e:?}"))?;
        Ok(u32::from_le_bytes(h))
    }

    /// Run leaves + init_rows + 5 rounds + solution_scan for one
    /// (input, nonce). Returns raw 32-index candidate solutions —
    /// caller compresses + re-validates via
    /// `equium::is_valid_solution` before submitting tx.
    pub fn run_nonce(
        &mut self,
        input: &[u8; 81],
        nonce: &[u8; 32],
    ) -> Result<Vec<[u32; 32]>> {
        self.write_leaves_params(input, nonce)?;
        let stream = self.ctx.default_stream();

        // 1. leaves
        let n_calls = (N_INIT_LEAVES + LEAVES_PER_CALL - 1) / LEAVES_PER_CALL;
        let grid = (n_calls + DEFAULT_BLOCK - 1) / DEFAULT_BLOCK;
        let cfg = LaunchConfig {
            grid_dim: (grid, 1, 1),
            block_dim: (DEFAULT_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut lb = stream.launch_builder(&self.leaves_fn);
        lb.arg(&self.leaves_params_d)
            .arg(&mut self.leaves_buf_d)
            .arg(&N_INIT_LEAVES);
        unsafe { lb.launch(cfg) }.map_err(|e| anyhow!("launch leaves: {e:?}"))?;

        // 2. init_rows → rows_a
        self.write_rounds_params(N_INIT_LEAVES, 1)?;
        let cfg_rows = launch_cfg(N_INIT_LEAVES);
        let mut lb = stream.launch_builder(&self.init_rows_fn);
        lb.arg(&self.rounds_params_d)
            .arg(&self.leaves_buf_d)
            .arg(&mut self.rows_a_d);
        unsafe { lb.launch(cfg_rows) }.map_err(|e| anyhow!("launch init_rows: {e:?}"))?;

        // 3. 5 Wagner rounds, ping-ponging rows_a ↔ rows_b
        let mut n_rows_current = N_INIT_LEAVES;
        for round in 0..5u32 {
            let indices_count_in = 1u32 << round;
            self.write_rounds_params(n_rows_current, indices_count_in)?;

            // Clear bucket_counts + out_count.
            stream
                .memcpy_htod(&self.zero_counts, &mut self.bucket_counts_d)
                .map_err(|e| anyhow!("clear bucket_counts: {e:?}"))?;
            stream
                .memcpy_htod(&self.zero4, &mut self.out_count_d)
                .map_err(|e| anyhow!("clear out_count: {e:?}"))?;

            let cfg_r = launch_cfg(n_rows_current);

            // count_buckets — read from the active "input" buffer.
            if round % 2 == 0 {
                let mut lb = stream.launch_builder(&self.count_fn);
                lb.arg(&self.rounds_params_d)
                    .arg(&self.rows_a_d)
                    .arg(&mut self.bucket_counts_d)
                    .arg(&mut self.bucket_slots_d);
                unsafe { lb.launch(cfg_r) }
                    .map_err(|e| anyhow!("launch count r{round}: {e:?}"))?;
            } else {
                let mut lb = stream.launch_builder(&self.count_fn);
                lb.arg(&self.rounds_params_d)
                    .arg(&self.rows_b_d)
                    .arg(&mut self.bucket_counts_d)
                    .arg(&mut self.bucket_slots_d);
                unsafe { lb.launch(cfg_r) }
                    .map_err(|e| anyhow!("launch count r{round}: {e:?}"))?;
            }

            // pair_emit — same input, opposite output.
            if round % 2 == 0 {
                let mut lb = stream.launch_builder(&self.pair_fn);
                lb.arg(&self.rounds_params_d)
                    .arg(&self.rows_a_d)
                    .arg(&mut self.rows_b_d)
                    .arg(&self.bucket_counts_d)
                    .arg(&self.bucket_slots_d)
                    .arg(&mut self.out_count_d);
                unsafe { lb.launch(cfg_r) }
                    .map_err(|e| anyhow!("launch pair r{round}: {e:?}"))?;
            } else {
                let mut lb = stream.launch_builder(&self.pair_fn);
                lb.arg(&self.rounds_params_d)
                    .arg(&self.rows_b_d)
                    .arg(&mut self.rows_a_d)
                    .arg(&self.bucket_counts_d)
                    .arg(&self.bucket_slots_d)
                    .arg(&mut self.out_count_d);
                unsafe { lb.launch(cfg_r) }
                    .map_err(|e| anyhow!("launch pair r{round}: {e:?}"))?;
            }

            // Readback out_count to drive next round's grid.
            let next_n = self.readback_out_count()?;
            if next_n == 0 {
                return Ok(Vec::new());
            }
            if next_n > MAX_ROWS {
                return Err(anyhow!(
                    "round {round} out_count overflow: {next_n} > {MAX_ROWS}"
                ));
            }
            n_rows_current = next_n;
        }

        // After 5 rounds (rounds 0,2,4 wrote to rows_b), survivors
        // live in rows_b. Solution scan reads rows_b, writes rows_a.
        self.write_rounds_params(n_rows_current, 32)?;
        stream
            .memcpy_htod(&self.zero4, &mut self.out_count_d)
            .map_err(|e| anyhow!("clear out_count solution: {e:?}"))?;
        let cfg_sol = launch_cfg(n_rows_current);
        let mut lb = stream.launch_builder(&self.solution_fn);
        lb.arg(&self.rounds_params_d)
            .arg(&self.rows_b_d)
            .arg(&mut self.rows_a_d)
            .arg(&mut self.out_count_d);
        unsafe { lb.launch(cfg_sol) }.map_err(|e| anyhow!("launch solution: {e:?}"))?;

        let n_sol = self.readback_out_count()?.min(MAX_SOLUTIONS);
        if n_sol == 0 {
            return Ok(Vec::new());
        }

        // Read solution rows (first n_sol slots of rows_a).
        let bytes = (n_sol as usize) * (ROW_BYTES as usize);
        let mut host = vec![0u8; bytes];
        stream
            .memcpy_dtoh(&self.rows_a_d.slice(0..bytes), &mut host)
            .map_err(|e| anyhow!("dtoh solutions: {e:?}"))?;
        stream.synchronize().map_err(|e| anyhow!("final sync: {e:?}"))?;

        let mut out = Vec::with_capacity(n_sol as usize);
        for i in 0..n_sol as usize {
            let base = i * (ROW_BYTES as usize) + (HASH_WORDS as usize) * 4;
            let mut idx = [0u32; 32];
            for k in 0..32 {
                let off = base + k * 4;
                idx[k] = u32::from_le_bytes([
                    host[off], host[off + 1], host[off + 2], host[off + 3],
                ]);
            }
            out.push(idx);
        }
        Ok(out)
    }
}

fn launch_cfg(n_threads: u32) -> LaunchConfig {
    let grid = (n_threads + DEFAULT_BLOCK - 1) / DEFAULT_BLOCK;
    LaunchConfig {
        grid_dim: (grid, 1, 1),
        block_dim: (DEFAULT_BLOCK, 1, 1),
        shared_mem_bytes: 0,
    }
}
