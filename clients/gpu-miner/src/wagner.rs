//! Full-GPU Wagner pipeline for Equihash 96,5 (v0.2).
//!
//! Pipeline: leaves → init_rows → 5×(count_buckets + pair_emit) →
//! solution_scan → readback.
//!
//! All BLAKE2b + Wagner work stays on the GPU; the only CPU touch
//! points per nonce are:
//!   * writing the (input, nonce) into the uniform buffer,
//!   * uploading a fresh `indices_count_in` between rounds, and
//!   * reading back at most a few solution slots at the end.
//!
//! Correctness is validated at the algorithm level by
//! `shader_ref::round_kernel` against `equihash_core::solver::round`;
//! WGSL syntax + type checking is validated by `naga` in `cargo test`.
//! See `verify-rounds` for an end-to-end on-device check.
//!
//! This module is intentionally separate from `gpu.rs` so the v0.1
//! `GpuLeafGen` path is unchanged — that one is the production miner
//! today and we don't want to perturb it while v0.2 stabilizes.

// Several struct fields here hold wgpu resource handles that need to
// outlive each per-nonce dispatch (the device keeps internal refcounts
// only while they're owned). They look "unread" to rustc but are
// load-bearing.
#![allow(dead_code)]

use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use std::borrow::Cow;

use crate::gpu::{backends_from_env, pick_workgroup_size, pipeline_constants};

pub const LEAF_BYTES: usize = 12;
pub const N_INIT_LEAVES: u32 = 1 << 17; // 131,072 for Equihash 96,5
pub const LEAVES_PER_BLAKE2B: u32 = 5;

// Default workgroup size — overridden per pipeline via WGSL
// override constants based on adapter vendor (see
// `pick_workgroup_size` in gpu.rs). The dispatch math uses
// `self.wg_size` so this constant is only the WGSL fallback.

// Match rounds.wgsl constants.
const HASH_WORDS: u32 = 3;
const INDICES_MAX: u32 = 32;
const ROW_WORDS: u32 = 35;
const NUM_BUCKETS: u32 = 65_536;
const MAX_PER_BUCKET: u32 = 16;

/// Capacity for the ping-pong row buffers. Expected output per round
/// is ~N (mean 1 pair per bucket × 65,536 buckets). Tail bound with
/// MAX_PER_BUCKET=16 is well under 2× input. Allocate 2.5× for safety.
const MAX_ROWS: u32 = 320_000;

/// Tiny upper bound on candidate solutions per nonce — for (96,5) the
/// expected count per nonce is well under 1 and the absolute max we
/// ever need to surface to the CPU is a handful.
const MAX_SOLUTIONS: u32 = 64;

// Re-exported leaves uniform layout. Matches gpu.rs::Params one-to-one
// so we can share leaves.wgsl unchanged.
#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
struct LeavesParams {
    personal: [u32; 4],
    cfg: [u32; 4],
    input: [[u32; 4]; 6],
    nonce: [u32; 4],
    nonce_hi: [u32; 4],
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
struct RoundsParams {
    n_rows: u32,
    max_out_rows: u32,
    indices_count_in: u32,
    _pad: u32,
}

pub struct GpuWagner {
    device: wgpu::Device,
    queue: wgpu::Queue,

    // Pipelines
    leaves_pipeline: wgpu::ComputePipeline,
    init_rows_pipeline: wgpu::ComputePipeline,
    count_pipeline: wgpu::ComputePipeline,
    pair_pipeline: wgpu::ComputePipeline,
    solution_pipeline: wgpu::ComputePipeline,

    // Bind group layouts
    leaves_bgl: wgpu::BindGroupLayout,
    rounds_bgl: wgpu::BindGroupLayout,

    // Persistent buffers
    leaves_params_buf: wgpu::Buffer,
    leaves_buf: wgpu::Buffer,
    rounds_params_buf: wgpu::Buffer,
    rows_a: wgpu::Buffer,
    rows_b: wgpu::Buffer,
    bucket_counts: wgpu::Buffer,
    bucket_slots: wgpu::Buffer,
    out_count: wgpu::Buffer,
    staging_out_count: wgpu::Buffer,
    staging_solutions: wgpu::Buffer,

    // Two bind groups for the rounds pipeline — one per ping-pong
    // direction. Pre-built so per-nonce dispatch has no allocator
    // pressure.
    bg_in_a_out_b: wgpu::BindGroup,
    bg_in_b_out_a: wgpu::BindGroup,
    bg_leaves: wgpu::BindGroup,

    pub backend: wgpu::Backend,
    pub adapter_name: String,
    /// Workgroup size baked into every pipeline in this struct. Both
    /// shaders share the same `override WG_SIZE` constant; the host
    /// picks one value per adapter and reuses it.
    wg_size: u32,
}

impl GpuWagner {
    pub fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: backends_from_env(),
            ..Default::default()
        });

        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
            .ok_or_else(|| anyhow!("no compatible GPU adapter found"))?;

        let info = adapter.get_info();
        let backend = info.backend;
        let wg_size = pick_workgroup_size(&info);
        let adapter_name = format!("{} ({:?}, wg={})", info.name, info.backend, wg_size);

        // Only bump what we strictly need. Downlevel_defaults gives a
        // conservative-but-portable baseline (OpenGL ES 3.0 era); we
        // need more storage buffers per stage (7) and a slightly
        // larger max_storage_buffer_binding_size for the 45 MB row
        // buffer. Both are within the WebGPU baseline so any real GPU
        // accepts them.
        let mut limits = wgpu::Limits::downlevel_defaults();
        // rounds.wgsl declares 7 storage bindings (rows_in, rows_out,
        // bucket_counts, bucket_slots, out_count, leaves + 1 uniform
        // doesn't count). WebGPU baseline = 8.
        limits.max_storage_buffers_per_shader_stage = 7;
        // 320,000 rows × 35 × 4 = 44.8 MB per row buffer; round up.
        limits.max_storage_buffer_binding_size = 64 << 20; // 64 MB

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("equium-gpu-wagner"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| anyhow!("device init failed: {e:?}"))?;

        // Shaders
        let leaves_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("leaves.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/leaves.wgsl"))),
        });
        let rounds_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rounds.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/rounds.wgsl"))),
        });

        // ---------------- leaves pipeline (same shape as gpu.rs) ----
        let leaves_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("leaves.bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let leaves_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("leaves.pl"),
            bind_group_layouts: &[&leaves_bgl],
            push_constant_ranges: &[],
        });
        let constants = pipeline_constants(wg_size);
        let leaves_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("leaves.pipeline"),
            layout: Some(&leaves_pl),
            module: &leaves_module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                zero_initialize_workgroup_memory: false,
            },
            cache: None,
        });

        // ---------------- rounds bind group layout ------------------
        // Bindings 0..6 match rounds.wgsl.
        let rounds_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rounds.bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let rounds_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rounds.pl"),
            bind_group_layouts: &[&rounds_bgl],
            push_constant_ranges: &[],
        });

        // Capture `constants` by reference so the closure can hand the
        // same override map to every round-pipeline build.
        let round_constants = &constants;
        let make_round_pipeline = |entry: &str, label: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&rounds_pl),
                module: &rounds_module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: round_constants,
                    zero_initialize_workgroup_memory: false,
                },
                cache: None,
            })
        };
        let init_rows_pipeline = make_round_pipeline("init_rows", "init_rows.pipeline");
        let count_pipeline = make_round_pipeline("count_buckets", "count.pipeline");
        let pair_pipeline = make_round_pipeline("pair_emit", "pair.pipeline");
        let solution_pipeline = make_round_pipeline("solution_scan", "solution.pipeline");

        // ---------------- Buffers --------------------------------------
        let leaves_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("leaves_params"),
            size: std::mem::size_of::<LeavesParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let leaves_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("leaves"),
            size: (N_INIT_LEAVES as u64) * (LEAF_BYTES as u64),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let rounds_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rounds_params"),
            size: std::mem::size_of::<RoundsParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let row_buf_bytes = (MAX_ROWS as u64) * (ROW_WORDS as u64) * 4;
        let rows_a = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rows_a"),
            size: row_buf_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let rows_b = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rows_b"),
            size: row_buf_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let bucket_counts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bucket_counts"),
            size: (NUM_BUCKETS as u64) * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bucket_slots = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bucket_slots"),
            size: (NUM_BUCKETS as u64) * (MAX_PER_BUCKET as u64) * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let out_count = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("out_count"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging_out_count = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging_out_count"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let staging_solutions_bytes = (MAX_SOLUTIONS as u64) * (ROW_WORDS as u64) * 4;
        let staging_solutions = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging_solutions"),
            size: staging_solutions_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------------- Bind groups ---------------------------------
        let bg_leaves = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("leaves.bg"),
            layout: &leaves_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: leaves_params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: leaves_buf.as_entire_binding(),
                },
            ],
        });

        let make_round_bg = |in_buf: &wgpu::Buffer, out_buf: &wgpu::Buffer, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &rounds_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: rounds_params_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: in_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: out_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: bucket_counts.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: bucket_slots.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: out_count.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: leaves_buf.as_entire_binding(),
                    },
                ],
            })
        };
        let bg_in_a_out_b = make_round_bg(&rows_a, &rows_b, "rounds.bg.a_to_b");
        let bg_in_b_out_a = make_round_bg(&rows_b, &rows_a, "rounds.bg.b_to_a");

        Ok(Self {
            device,
            queue,
            leaves_pipeline,
            init_rows_pipeline,
            count_pipeline,
            pair_pipeline,
            solution_pipeline,
            leaves_bgl,
            rounds_bgl,
            leaves_params_buf,
            leaves_buf,
            rounds_params_buf,
            rows_a,
            rows_b,
            bucket_counts,
            bucket_slots,
            out_count,
            staging_out_count,
            staging_solutions,
            bg_in_a_out_b,
            bg_in_b_out_a,
            bg_leaves,
            backend,
            adapter_name,
            wg_size,
        })
    }

    fn write_leaves_params(&self, input: &[u8; 81], nonce: &[u8; 32]) {
        let mut p = [0u8; 16];
        p[..8].copy_from_slice(b"ZcashPoW");
        p[8..12].copy_from_slice(&96u32.to_le_bytes());
        p[12..16].copy_from_slice(&5u32.to_le_bytes());
        let personal = [
            u32::from_le_bytes(p[0..4].try_into().unwrap()),
            u32::from_le_bytes(p[4..8].try_into().unwrap()),
            u32::from_le_bytes(p[8..12].try_into().unwrap()),
            u32::from_le_bytes(p[12..16].try_into().unwrap()),
        ];
        let mut input_padded = [0u8; 96];
        input_padded[..81].copy_from_slice(input);
        let mut input_vec = [[0u32; 4]; 6];
        for i in 0..6 {
            let mut chunk = [0u8; 16];
            chunk.copy_from_slice(&input_padded[i * 16..(i + 1) * 16]);
            input_vec[i] = [
                u32::from_le_bytes(chunk[0..4].try_into().unwrap()),
                u32::from_le_bytes(chunk[4..8].try_into().unwrap()),
                u32::from_le_bytes(chunk[8..12].try_into().unwrap()),
                u32::from_le_bytes(chunk[12..16].try_into().unwrap()),
            ];
        }
        let mut nonce_lo = [0u32; 4];
        let mut nonce_hi = [0u32; 4];
        for i in 0..4 {
            nonce_lo[i] = u32::from_le_bytes(nonce[i * 4..(i + 1) * 4].try_into().unwrap());
            nonce_hi[i] =
                u32::from_le_bytes(nonce[16 + i * 4..16 + (i + 1) * 4].try_into().unwrap());
        }
        let params = LeavesParams {
            personal,
            cfg: [60, N_INIT_LEAVES, 0, 0],
            input: input_vec,
            nonce: nonce_lo,
            nonce_hi,
        };
        self.queue
            .write_buffer(&self.leaves_params_buf, 0, bytemuck::bytes_of(&params));
    }

    fn write_rounds_params(&self, n_rows: u32, indices_count_in: u32) {
        let params = RoundsParams {
            n_rows,
            max_out_rows: MAX_ROWS,
            indices_count_in,
            _pad: 0,
        };
        self.queue
            .write_buffer(&self.rounds_params_buf, 0, bytemuck::bytes_of(&params));
    }

    fn dispatch_with_grid(&self, encoder: &mut wgpu::CommandEncoder, pipeline: &wgpu::ComputePipeline, bg: &wgpu::BindGroup, n_threads: u32, label: &str) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bg, &[]);
        let workgroups = (n_threads + self.wg_size - 1) / self.wg_size;
        pass.dispatch_workgroups(workgroups, 1, 1);
    }

    fn readback_out_count(&self) -> Result<u32> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback_count.enc"),
            });
        encoder.copy_buffer_to_buffer(&self.out_count, 0, &self.staging_out_count, 0, 4);
        self.queue.submit(Some(encoder.finish()));

        let slice = self.staging_out_count.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .context("readback channel closed")?
            .map_err(|e| anyhow!("map staging_out_count failed: {e:?}"))?;
        let data = slice.get_mapped_range();
        let count = u32::from_le_bytes(data[..4].try_into().unwrap());
        drop(data);
        self.staging_out_count.unmap();
        Ok(count)
    }

    /// Run leaves + 5 rounds + solution_scan for one (input, nonce). On
    /// success, returns the list of raw 32-index candidate solutions
    /// (caller verifies via `equihash::is_valid_solution`). Returns an
    /// error only on GPU failures — "no solution this nonce" is the
    /// common path and surfaces as an empty Vec.
    pub fn run_nonce(&self, input: &[u8; 81], nonce: &[u8; 32]) -> Result<Vec<[u32; 32]>> {
        self.write_leaves_params(input, nonce);

        // === Step 1: leaves ===
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("step1_leaves.enc"),
            });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("leaves.pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.leaves_pipeline);
            pass.set_bind_group(0, &self.bg_leaves, &[]);
            let n_calls = (N_INIT_LEAVES + LEAVES_PER_BLAKE2B - 1) / LEAVES_PER_BLAKE2B;
            let workgroups = (n_calls + self.wg_size - 1) / self.wg_size;
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(Some(enc.finish()));

        // === Step 2: init_rows  — writes into rows_a via bg_in_b_out_a
        // (input is `leaves`, output is rows_a). The rows_in binding
        // is unused by init_rows, but the layout requires it bound —
        // we use rows_b as a harmless dummy.
        self.write_rounds_params(N_INIT_LEAVES, 1);
        {
            let mut enc = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("step2_init.enc"),
                });
            self.dispatch_with_grid(
                &mut enc,
                &self.init_rows_pipeline,
                &self.bg_in_b_out_a,
                N_INIT_LEAVES,
                "init_rows.pass",
            );
            self.queue.submit(Some(enc.finish()));
        }

        // === Step 3: 5 Wagner rounds, ping-pong rows_a ↔ rows_b ===
        // Round 0: in=A, out=B → bg_in_a_out_b
        // Round 1: in=B, out=A → bg_in_b_out_a
        // ... alternates.
        let mut n_rows_current = N_INIT_LEAVES;
        for round in 0..5u32 {
            let indices_count_in = 1u32 << round;
            self.write_rounds_params(n_rows_current, indices_count_in);

            let bg = if round % 2 == 0 {
                &self.bg_in_a_out_b
            } else {
                &self.bg_in_b_out_a
            };

            // Clear bucket_counts and out_count before each round.
            let mut enc = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("round.enc"),
                });
            enc.clear_buffer(&self.bucket_counts, 0, None);
            enc.clear_buffer(&self.out_count, 0, None);

            self.dispatch_with_grid(
                &mut enc,
                &self.count_pipeline,
                bg,
                n_rows_current,
                "count.pass",
            );
            self.dispatch_with_grid(
                &mut enc,
                &self.pair_pipeline,
                bg,
                n_rows_current,
                "pair.pass",
            );
            self.queue.submit(Some(enc.finish()));

            // Read back out_count so we know the next round's n_rows.
            // (~10–50 μs per stall; 5 stalls/nonce ≈ 0.25 ms overhead.)
            let next_n = self.readback_out_count()?;
            if next_n == 0 {
                // No surviving rows — this nonce has no solution.
                return Ok(Vec::new());
            }
            if next_n > MAX_ROWS {
                return Err(anyhow!(
                    "round {round} output overflow: {next_n} > MAX_ROWS={MAX_ROWS}"
                ));
            }
            n_rows_current = next_n;
        }

        // After 5 rounds, the final rows live in:
        //   round 0 → B, 1 → A, 2 → B, 3 → A, 4 → B  ⇒ rows_b.
        // For solution_scan we read rows_b and write into rows_a.
        self.write_rounds_params(n_rows_current, 32);
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("step4_solution.enc"),
            });
        enc.clear_buffer(&self.out_count, 0, None);
        self.dispatch_with_grid(
            &mut enc,
            &self.solution_pipeline,
            &self.bg_in_b_out_a,
            n_rows_current,
            "solution.pass",
        );
        // Copy solutions out of rows_a into the staging buffer.
        let copy_bytes = (MAX_SOLUTIONS as u64) * (ROW_WORDS as u64) * 4;
        enc.copy_buffer_to_buffer(&self.rows_a, 0, &self.staging_solutions, 0, copy_bytes);
        self.queue.submit(Some(enc.finish()));

        let n_sol = self.readback_out_count()?.min(MAX_SOLUTIONS);
        if n_sol == 0 {
            return Ok(Vec::new());
        }

        // Map + read the solution slots.
        let slice = self.staging_solutions.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .context("readback channel closed")?
            .map_err(|e| anyhow!("map staging_solutions failed: {e:?}"))?;
        let data = slice.get_mapped_range();

        let mut out = Vec::with_capacity(n_sol as usize);
        for i in 0..n_sol as usize {
            let base = i * (ROW_WORDS as usize) * 4;
            let mut indices = [0u32; 32];
            for k in 0..32 {
                let off = base + ((HASH_WORDS as usize) + k) * 4;
                indices[k] = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            }
            out.push(indices);
        }
        drop(data);
        self.staging_solutions.unmap();

        Ok(out)
    }
}
