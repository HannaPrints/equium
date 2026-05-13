//! Equium GPU miner — v0.
//!
//! Scope today: GPU-accelerated leaf generation (the BLAKE2b-heavy
//! ~70% of solver time), with Wagner rounds still on the CPU. This is
//! a hybrid path; pure-GPU Wagner is a separate effort.
//!
//! Subcommands:
//!   * `verify`  — generate leaves on both GPU and CPU for a fixed
//!                 (input, nonce) pair and assert they match. Run this
//!                 once on a new machine to confirm the shader is
//!                 correct for your driver/backend.
//!   * `bench`   — measure GPU leaf-generation throughput.
//!   * `mine`    — run the full hybrid mining loop. (Not yet wired up
//!                 in v0; falls back to a message until next session.)

use anchor_lang::prelude::AccountMeta;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use crossbeam_channel::bounded;
use equihash_core::challenge::{build_input, solution_hash, I_LEN};
use equihash_core::solver::{leaf_bytes, n_init_leaves, try_nonce_with_leaves};
use equihash_core::target::hash_under_target;
use equium::state::{EquiumConfig, CONFIG_SEED, VAULT_SEED};
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Keypair, Signer};
#[allow(deprecated)]
use solana_sdk::system_program;
use solana_sdk::sysvar;
use solana_sdk::transaction::Transaction;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod gpu;
mod shader_ref;
mod wagner;
#[cfg(feature = "cuda")]
mod cuda;
#[cfg(feature = "cuda")]
mod cuda_wagner;

#[derive(Parser, Debug)]
#[command(version, about = "Equium GPU miner")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Verify GPU leaf output matches the CPU reference for a fixed
    /// input/nonce. Use this to validate your driver before mining.
    Verify {
        /// How many leaves to verify. Default 2048 keeps the test fast
        /// (~ms) while still catching most byte-level shader bugs.
        #[arg(long, default_value_t = 2048u32)]
        leaves: u32,
    },
    /// Validate the shader *logic* without needing a GPU. Runs the
    /// pure-Rust port of leaves.wgsl against blake2b_simd byte-for-byte.
    /// If this fails, the WGSL kernel is wrong; if it passes, you can
    /// trust the GPU path to be correct barring a driver bug.
    VerifyCpu {
        #[arg(long, default_value_t = 8192u32)]
        leaves: u32,
    },
    /// Benchmark GPU leaf-generation throughput at full Equihash 96,5
    /// width (2^17 = 131,072 leaves).
    Bench {
        #[arg(long, default_value_t = 200u32)]
        iterations: u32,
    },
    /// v0.2 sanity check: run the full GPU pipeline (leaves + Wagner +
    /// solution scan) and the equivalent CPU reference on the same
    /// nonces, and assert they agree. If this passes, `--full-gpu`
    /// mining is safe on your driver.
    VerifyRounds {
        /// How many nonces to try. Each runs both GPU and CPU
        /// pipelines end-to-end. Default is small because the CPU
        /// reference is unoptimized (~3s/nonce in release).
        #[arg(long, default_value_t = 4u32)]
        nonces: u32,
    },
    /// Benchmark the full GPU Wagner pipeline (leaves + 5 rounds +
    /// solution scan) at full (96, 5) width.
    BenchWagner {
        #[arg(long, default_value_t = 50u32)]
        iterations: u32,
    },
    /// Internal: probe one wgpu backend and exit 0 on success. Spawned
    /// as a subprocess by the auto-pick logic so a driver SIGSEGV
    /// (e.g. NVIDIA 575's libnvidia-glvkspirv.so) doesn't take down
    /// the parent. Not intended for direct user invocation.
    #[command(hide = true)]
    Probe {
        /// Backend tag to probe (vulkan | gl | metal | dx12 | primary).
        #[arg(long, default_value = "primary")]
        backend: String,
    },
    /// Mine. Defaults to v0.1 hybrid (GPU leaves + CPU Wagner). Pass
    /// `--full-gpu` to use the v0.2 all-on-GPU pipeline.
    Mine {
        /// RPC endpoint. Bring your own — public mainnet rate-limits.
        #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
        rpc_url: String,

        /// Solana keypair file to mine with.
        #[arg(long)]
        keypair: PathBuf,

        /// Stop after N successful blocks (0 = run forever).
        #[arg(long, default_value_t = 0u64)]
        max_blocks: u64,

        /// Compute-unit limit per `mine` tx.
        #[arg(long, default_value_t = 1_400_000u32)]
        cu_limit: u32,

        /// CPU worker threads (v0.1 hybrid path only). Default =
        /// num_cpus. Each thread requests leaves from the GPU and runs
        /// its own Wagner pass.
        #[arg(long, short = 't', default_value_t = 0)]
        threads: usize,

        /// Use the v0.2 full-GPU pipeline. Leaves + Wagner + solution
        /// scan all run on the GPU; the CPU only manages tx submission.
        /// Run `verify-rounds` once on your machine before enabling.
        #[arg(long, default_value_t = false)]
        full_gpu: bool,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    // Two subcommands skip the auto-probe:
    //   - probe       — we're already inside a probe child; another
    //                   level would recurse.
    //   - verify-cpu  — pure CPU, never touches the GPU.
    // Everything else picks a backend now and writes it to
    // EQUIUM_BACKEND so all downstream `wgpu::Instance::new` calls
    // honor the choice.
    let needs_gpu = !matches!(args.cmd, Cmd::Probe { .. } | Cmd::VerifyCpu { .. });
    if needs_gpu {
        match auto_pick_backend() {
            Ok(backend) => std::env::set_var("EQUIUM_BACKEND", backend),
            Err(msg) => {
                eprintln!("\n✗ {msg}");
                std::process::exit(1);
            }
        }
    }

    match args.cmd {
        Cmd::Verify { leaves } => verify(leaves),
        Cmd::VerifyCpu { leaves } => verify_cpu(leaves),
        Cmd::Bench { iterations } => bench(iterations),
        Cmd::VerifyRounds { nonces } => verify_rounds(nonces),
        Cmd::BenchWagner { iterations } => bench_wagner(iterations),
        Cmd::Probe { backend } => probe(&backend),
        Cmd::Mine {
            rpc_url,
            keypair,
            max_blocks,
            cu_limit,
            threads,
            full_gpu,
        } => mine(rpc_url, keypair, max_blocks, cu_limit, threads, full_gpu),
    }
}

/// Internal probe: construct one backend's leaves pipeline and exit
/// zero on success. Always runs after `EQUIUM_BACKEND=<arg>` is set
/// so the wgpu constructor honors our request. The "cuda" backend
/// short-circuits to the cudarc path instead; "vulkan" / "gl" go
/// through wgpu. Stdout/stderr quiet on success (parent only cares
/// about the exit code).
fn probe(backend: &str) -> Result<()> {
    #[cfg(feature = "cuda")]
    if backend == "cuda" {
        let g = cuda::CudaLeafGen::new()?;
        eprintln!("{}", g.adapter_name);
        return Ok(());
    }
    std::env::set_var("EQUIUM_BACKEND", backend);
    // Constructing the leaves pipeline triggers the SPIR-V compile
    // path that crashes on bad NVIDIA drivers. If we reach the
    // println, the backend is healthy.
    let g = gpu::GpuLeafGen::new()?;
    eprintln!("{}", g.adapter_name);
    Ok(())
}

/// Auto-pick the best working GPU backend by probing each in
/// subprocesses (so a driver segfault doesn't take us down). Returns
/// the backend tag to use, or an error string for the operator if
/// nothing works.
///
/// Order:
///   1. If `EQUIUM_BACKEND` is set, respect it (escape hatch).
///   2. CUDA (when compiled in via --features cuda) — bypasses the
///      SPIR-V driver bug entirely. NVIDIA-only.
///   3. Pick the native wgpu backend for this OS: Metal on macOS,
///      DX12 on native Windows, Vulkan on Linux/WSL.
///   4. If Linux `nvidia-smi` reports a known-bad driver branch, skip
///      Vulkan entirely and start with GL. Saves the user from
///      waiting for the crash.
///   5. Probe each candidate via subprocess. If that fails, return
///      platform-specific troubleshooting steps.
fn auto_pick_backend() -> std::result::Result<&'static str, String> {
    if let Ok(forced) = std::env::var("EQUIUM_BACKEND") {
        if !forced.trim().is_empty() {
            eprintln!("backend: forced via EQUIUM_BACKEND={forced}");
            return Ok(Box::leak(forced.into_boxed_str()) as &'static str);
        }
    }

    let nvidia_driver = nvidia_driver_version();
    if std::env::consts::OS == "linux" {
        if let Some(drv) = nvidia_driver
            .as_deref()
            .filter(|drv| is_known_bad_spirv_driver(drv))
        {
            eprintln!(
                "⚠ Detected NVIDIA driver {drv} — known SPIR-V crash branch, \
                 skipping Vulkan probe."
            );
        }
    }
    let probe_order = probe_order_for_platform(std::env::consts::OS, nvidia_driver.as_deref());

    let mut errors: Vec<String> = Vec::new();
    for &backend in &probe_order {
        eprint!("backend: probing {backend}… ");
        match probe_subprocess(backend) {
            Ok(name) => {
                eprintln!("OK\n  ↳ {name}");
                return Ok(backend);
            }
            Err(reason) => {
                eprintln!("FAILED ({reason})");
                errors.push(format!("{backend}: {reason}"));
            }
        }
    }

    // If we saw "device is lost" anywhere, that's the unmistakable
    // signature of the NVIDIA 555/565/570/575 SPIR-V driver bug.
    // Tailor the message so the operator knows it's a driver issue,
    // not a config one.
    let device_lost = errors
        .iter()
        .any(|e| e.to_lowercase().contains("device is lost"));
    let driver_hint = if device_lost {
        "\n  ⚠ \"device is lost\" = NVIDIA SPIR-V driver bug (555.x / 565.x /\n     570.x / 575.x branches). Permanent fix: install the LTS\n     driver: sudo apt install -y nvidia-driver-535-server.\n     Inside a container (vast.ai / Runpod) you can't replace the\n     kernel module — stop the rental and pick another listing\n     with driver 535.x / 545.x / 550.x instead.\n"
    } else {
        ""
    };

    Err(format!(
        "No working GPU backend.\n\nProbes:\n  - {}\n{}\nTroubleshooting:\n{}",
        errors.join("\n  - "),
        driver_hint,
        troubleshooting_for_platform(std::env::consts::OS)
    ))
}

fn probe_order_for_platform(os: &str, nvidia_driver: Option<&str>) -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut order = match os {
        "macos" => vec!["metal"],
        "windows" => vec!["dx12", "vulkan", "gl"],
        "linux" if nvidia_driver.is_some_and(is_known_bad_spirv_driver) => vec!["gl"],
        "linux" => vec!["vulkan", "gl"],
        _ => vec!["primary"],
    };

    #[cfg(feature = "cuda")]
    {
        if nvidia_driver.is_some() {
            order.insert(0, "cuda");
        }
    }

    order
}

fn is_known_bad_spirv_driver(driver: &str) -> bool {
    driver.starts_with("555.")
        || driver.starts_with("565.")
        || driver.starts_with("570.")
        || driver.starts_with("575.")
}

fn troubleshooting_for_platform(os: &str) -> &'static str {
    match os {
        "macos" => {
            "  1. system_profiler SPDisplaysDataType    (macOS sees the GPU?)\n  \
        2. Run from a logged-in Terminal session, not CI/headless SSH\n  \
        3. softwareupdate --install --all     (Metal driver updates ship with macOS)\n  \
        4. Try EQUIUM_BACKEND=metal ./target/release/equium-gpu-miner verify\n  \
        5. File an issue: https://github.com/HannaPrints/equium/issues\n"
        }
        "windows" => {
            "  1. dxdiag                              (Windows sees the GPU?)\n  \
        2. Update your GPU driver from NVIDIA/AMD/Intel\n  \
        3. Try EQUIUM_BACKEND=dx12 ./target/release/equium-gpu-miner verify\n  \
        4. File an issue: https://github.com/HannaPrints/equium/issues\n"
        }
        _ => {
            "  1. nvidia-smi              (driver present + healthy?)\n  \
        2. vulkaninfo --summary    (Vulkan loader sees the GPU?)\n  \
        3. lspci | grep -i vga     (kernel sees the card?)\n  \
        4. On NVIDIA 555/565/570/575: sudo apt install -y nvidia-driver-535-server\n  \
        5. File an issue: https://github.com/HannaPrints/equium/issues\n"
        }
    }
}

/// Spawn `self probe --backend=<backend>` and read the result. The
/// child has its own process space, so a SIGSEGV in the driver
/// crashes the child cleanly without touching us.
fn probe_subprocess(backend: &str) -> std::result::Result<String, String> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()
        .map_err(|e| format!("locate self: {e}"))?;
    let out = Command::new(&exe)
        .arg("probe")
        .arg("--backend")
        .arg(backend)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Belt-and-suspenders: clear EQUIUM_BACKEND for the child so
        // we don't recurse into the same forced choice.
        .env_remove("EQUIUM_BACKEND")
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if out.status.success() {
        let name = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Ok(if name.is_empty() { backend.to_string() } else { name })
    } else {
        Err(probe_failure_reason(
            out.status.code(),
            &out.stdout,
            &out.stderr,
        ))
    }
}

fn probe_failure_reason(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> String {
    let detail = probe_output_detail(stdout, stderr);
    match (code, detail) {
        (Some(code), Some(detail)) => format!("exit {code}: {detail}"),
        (Some(code), None) => format!("exit {code}"),
        (None, Some(detail)) => format!("killed by signal: {detail}"),
        (None, None) => "killed by signal (likely SIGSEGV — driver crash)".to_string(),
    }
}

fn probe_output_detail(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    for bytes in [stderr, stdout] {
        let text = String::from_utf8_lossy(bytes);
        for line in text.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("Error:") {
                let rest = rest.trim();
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }

    for bytes in [stderr, stdout] {
        let text = String::from_utf8_lossy(bytes);
        if let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) {
            return Some(line.to_string());
        }
    }

    None
}

/// Read the NVIDIA driver version via `nvidia-smi`. Returns None when
/// nvidia-smi isn't present (AMD-only / Apple / etc.) or fails.
fn nvidia_driver_version() -> Option<String> {
    use std::process::Command;
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=driver_version", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_probe_order_uses_metal() {
        assert_eq!(probe_order_for_platform("macos", None), vec!["metal"]);
    }

    #[test]
    fn linux_probe_order_keeps_gl_fallback() {
        assert_eq!(
            probe_order_for_platform("linux", None),
            vec!["vulkan", "gl"]
        );
    }

    #[test]
    fn linux_nvidia_575_probe_order_skips_vulkan() {
        assert_eq!(
            probe_order_for_platform("linux", Some("575.64.03")),
            vec!["gl"]
        );
    }

    #[test]
    fn probe_failure_reason_includes_child_stderr() {
        assert_eq!(
            probe_failure_reason(Some(1), b"", b"Error: no compatible GPU adapter found\n"),
            "exit 1: no compatible GPU adapter found"
        );
    }
}

/// Build a deterministic (input, nonce) pair for verification. Doesn't
/// have to be a real chain challenge — we just want both code paths to
/// chew on identical bytes.
fn fixed_test_input() -> ([u8; I_LEN], [u8; 32]) {
    let mut input = [0u8; I_LEN];
    input[..9].copy_from_slice(b"Equium-v1");
    for i in 9..I_LEN {
        input[i] = (i as u8).wrapping_mul(7).wrapping_add(13);
    }
    let mut nonce = [0u8; 32];
    for i in 0..32 {
        nonce[i] = (i as u8).wrapping_mul(31).wrapping_add(2);
    }
    (input, nonce)
}

/// Construct a leaf-generation backend honoring `EQUIUM_BACKEND`.
/// When the `cuda` feature is on and EQUIUM_BACKEND=cuda we build a
/// `CudaLeafGen` (bypassing wgpu's Vulkan/SPIR-V path entirely);
/// otherwise we fall through to the wgpu `GpuLeafGen` which honors
/// the standard primary/vulkan/gl/metal/dx12 tags.
fn make_leaf_gen() -> Result<Box<dyn gpu::LeafGen>> {
    #[cfg(feature = "cuda")]
    if std::env::var("EQUIUM_BACKEND").as_deref() == Ok("cuda") {
        return Ok(Box::new(cuda::CudaLeafGen::new()?));
    }
    Ok(Box::new(gpu::GpuLeafGen::new()?))
}

fn verify(n_leaves: u32) -> Result<()> {
    let gpu = make_leaf_gen()?;
    println!("GPU backend: {}", gpu.backend_label());

    let (input, nonce) = fixed_test_input();

    let mut gpu_out = vec![0u8; (n_leaves as usize) * gpu::LEAF_BYTES];
    let t0 = Instant::now();
    gpu.generate(&input, &nonce, n_leaves, &mut gpu_out)?;
    let gpu_ms = t0.elapsed().as_millis();

    let t1 = Instant::now();
    let cpu_out = cpu_leaves_reference(&input, &nonce, n_leaves);
    let cpu_ms = t1.elapsed().as_millis();

    let mut mismatches = 0usize;
    let mut first_mismatch_at = None;
    for i in 0..(n_leaves as usize) {
        let g = &gpu_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
        let c = &cpu_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
        if g != c {
            mismatches += 1;
            if first_mismatch_at.is_none() {
                first_mismatch_at = Some(i);
            }
        }
    }

    println!(
        "leaves: {}   gpu: {:>5}ms   cpu: {:>5}ms   match: {}/{}",
        n_leaves,
        gpu_ms,
        cpu_ms,
        n_leaves as usize - mismatches,
        n_leaves
    );

    if mismatches == 0 {
        println!("✓ GPU output matches CPU reference byte-for-byte.");
        Ok(())
    } else {
        if let Some(i) = first_mismatch_at {
            let g = &gpu_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
            let c = &cpu_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
            println!("first mismatch at leaf {i}:");
            println!("  gpu: {}", hex::encode(g));
            println!("  cpu: {}", hex::encode(c));
        }
        bail!("{} leaves differ between GPU and CPU", mismatches);
    }
}

/// CPU-only validation. Compares the Rust port of the shader logic
/// (src/shader_ref.rs) against the canonical blake2b_simd output. Catches
/// shader-logic bugs (arithmetic, packing, control flow) without
/// needing a GPU at all.
fn verify_cpu(n_leaves: u32) -> Result<()> {
    let (input, nonce) = fixed_test_input();

    let t_ref = Instant::now();
    let cpu_out = cpu_leaves_reference(&input, &nonce, n_leaves);
    let cpu_ms = t_ref.elapsed().as_millis();

    let t_shader = Instant::now();
    let shader_out = shader_ref::leaves(&input, &nonce, n_leaves);
    let shader_ms = t_shader.elapsed().as_millis();

    let mut mismatches = 0usize;
    let mut first_mismatch_at = None;
    for i in 0..(n_leaves as usize) {
        let g = &shader_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
        let c = &cpu_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
        if g != c {
            mismatches += 1;
            if first_mismatch_at.is_none() {
                first_mismatch_at = Some(i);
            }
        }
    }

    println!(
        "leaves: {}   shader-port: {:>4}ms   blake2b_simd ref: {:>4}ms   match: {}/{}",
        n_leaves,
        shader_ms,
        cpu_ms,
        n_leaves as usize - mismatches,
        n_leaves
    );

    if mismatches == 0 {
        println!("✓ shader logic matches BLAKE2b reference byte-for-byte.");
        println!("  GPU path should be correct on real hardware (subject to driver).");
        Ok(())
    } else {
        if let Some(i) = first_mismatch_at {
            let g = &shader_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
            let c = &cpu_out[i * gpu::LEAF_BYTES..(i + 1) * gpu::LEAF_BYTES];
            println!("first mismatch at leaf {i}:");
            println!("  shader-port: {}", hex::encode(g));
            println!("  blake2b ref: {}", hex::encode(c));
        }
        bail!("{} leaves differ between shader logic and BLAKE2b reference", mismatches);
    }
}

fn bench(iterations: u32) -> Result<()> {
    let gpu = make_leaf_gen()?;
    println!("GPU backend: {}", gpu.backend_label());

    let (input, nonce) = fixed_test_input();
    let n_leaves: u32 = 1 << 17; // 131,072 — full Equihash 96,5 width
    let mut out = vec![0u8; (n_leaves as usize) * gpu::LEAF_BYTES];

    // Warm-up dispatch (shader compile, pipeline cache).
    gpu.generate(&input, &nonce, n_leaves, &mut out)?;

    let t0 = Instant::now();
    for _ in 0..iterations {
        gpu.generate(&input, &nonce, n_leaves, &mut out)?;
    }
    let elapsed = t0.elapsed();
    let total_leaves = (iterations as u64) * (n_leaves as u64);
    let leaves_per_sec = total_leaves as f64 / elapsed.as_secs_f64();
    let hashes_per_sec = leaves_per_sec / (gpu::LEAVES_PER_CALL as f64);

    println!(
        "{} iterations × {} leaves in {:.2}s",
        iterations,
        n_leaves,
        elapsed.as_secs_f64()
    );
    println!(
        "leaf throughput:    {:>12.0} leaves/sec  ({:.1} MLeaves/s)",
        leaves_per_sec,
        leaves_per_sec / 1_000_000.0
    );
    println!(
        "BLAKE2b throughput: {:>12.0} hashes/sec  ({:.1} MH/s)",
        hashes_per_sec,
        hashes_per_sec / 1_000_000.0
    );

    Ok(())
}

// ============================================================================
// v0.2 full-GPU verification + bench
// ============================================================================

/// Backend-agnostic wrapper for the full Wagner pipeline. Lets
/// verify-rounds / bench-wagner / mine --full-gpu pick wgpu or CUDA
/// at runtime based on EQUIUM_BACKEND without duplicating the loop
/// bodies. CudaWagner mutates internal device buffers on each
/// dispatch so run_nonce is &mut self everywhere.
enum AnyWagner {
    Wgpu(wagner::GpuWagner),
    #[cfg(feature = "cuda")]
    Cuda(cuda_wagner::CudaWagner),
}

impl AnyWagner {
    fn new() -> Result<Self> {
        #[cfg(feature = "cuda")]
        if std::env::var("EQUIUM_BACKEND").as_deref() == Ok("cuda") {
            return Ok(AnyWagner::Cuda(cuda_wagner::CudaWagner::new()?));
        }
        Ok(AnyWagner::Wgpu(wagner::GpuWagner::new()?))
    }
    fn adapter_name(&self) -> &str {
        match self {
            AnyWagner::Wgpu(w) => &w.adapter_name,
            #[cfg(feature = "cuda")]
            AnyWagner::Cuda(w) => &w.adapter_name,
        }
    }
    fn run_nonce(
        &mut self,
        input: &[u8; 81],
        nonce: &[u8; 32],
    ) -> Result<Vec<[u32; 32]>> {
        match self {
            AnyWagner::Wgpu(w) => w.run_nonce(input, nonce),
            #[cfg(feature = "cuda")]
            AnyWagner::Cuda(w) => w.run_nonce(input, nonce),
        }
    }
}

/// Sanity-check the full-GPU Wagner pipeline by running it alongside
/// the CPU reference (shader_ref::wagner_full) on the same nonces and
/// asserting they find the same solutions. Solutions are sparse, so
/// most nonces produce `[]` from both sides — what we're testing is
/// that *whenever* the CPU finds a solution, the GPU finds it too,
/// and vice versa.
fn verify_rounds(n_nonces: u32) -> Result<()> {
    let mut wagner = AnyWagner::new()?;
    println!("GPU backend: {}", wagner.adapter_name());
    println!(
        "verifying {} nonce(s) — CPU reference is unoptimized, expect ~few seconds each\n",
        n_nonces
    );

    let (input, base_nonce) = fixed_test_input();
    let mut disagreements = 0usize;

    for n in 0..n_nonces {
        // Tweak nonce per iteration so we cover different bucket
        // configurations. First byte is unique per iteration.
        let mut nonce = base_nonce;
        nonce[0] = (n & 0xFF) as u8;
        nonce[1] = ((n >> 8) & 0xFF) as u8;

        let t_cpu = Instant::now();
        let cpu_leaves = shader_ref::leaves(&input, &nonce, 1 << 17);
        let cpu_solutions = shader_ref::wagner_full(&cpu_leaves);
        let cpu_ms = t_cpu.elapsed().as_millis();

        let t_gpu = Instant::now();
        let gpu_solutions = wagner.run_nonce(&input, &nonce)?;
        let gpu_ms = t_gpu.elapsed().as_millis();

        // Canonicalize for comparison: sort each solution's indices
        // lexicographically *of its 32 u32s* (canonical concat means
        // both sides produce the same order, but sorting the *list of
        // solutions* is needed since GPU emits in non-deterministic
        // order).
        let mut cpu_sorted: Vec<[u32; 32]> = cpu_solutions
            .iter()
            .map(|r| r.indices)
            .collect();
        cpu_sorted.sort();
        let mut gpu_sorted = gpu_solutions.clone();
        gpu_sorted.sort();

        let agree = cpu_sorted == gpu_sorted;
        let tag = if agree { "✓" } else { "✗" };
        println!(
            "  nonce #{:<3}  cpu: {:>5}ms ({} sol)  gpu: {:>5}ms ({} sol)  {}",
            n,
            cpu_ms,
            cpu_sorted.len(),
            gpu_ms,
            gpu_sorted.len(),
            tag
        );

        if !agree {
            disagreements += 1;
            println!("    cpu solutions: {:?}", cpu_sorted);
            println!("    gpu solutions: {:?}", gpu_sorted);
        }
    }

    if disagreements == 0 {
        println!(
            "\n✓ GPU Wagner pipeline matches CPU reference across {} nonces.",
            n_nonces
        );
        println!("  Safe to mine with --full-gpu.");
        Ok(())
    } else {
        bail!(
            "{} nonce(s) disagreed between GPU and CPU — do NOT use --full-gpu",
            disagreements
        )
    }
}

fn bench_wagner(iterations: u32) -> Result<()> {
    let mut wagner = AnyWagner::new()?;
    println!("GPU backend: {}", wagner.adapter_name());

    let (input, nonce) = fixed_test_input();

    // Warm-up: triggers shader compile + pipeline cache + first-buffer
    // alloc cost.
    let _ = wagner.run_nonce(&input, &nonce)?;

    let t0 = Instant::now();
    for n in 0..iterations {
        let mut nn = nonce;
        nn[0] = (n & 0xFF) as u8;
        nn[1] = ((n >> 8) & 0xFF) as u8;
        let _ = wagner.run_nonce(&input, &nn)?;
    }
    let elapsed = t0.elapsed();
    let h_per_sec = iterations as f64 / elapsed.as_secs_f64();

    println!(
        "{} iterations in {:.2}s",
        iterations,
        elapsed.as_secs_f64()
    );
    println!(
        "wagner throughput:  {:>10.1} H/s   ({:.1} ms/nonce)",
        h_per_sec,
        elapsed.as_secs_f64() * 1000.0 / iterations as f64
    );

    Ok(())
}

// ============================================================================
// Mining loop
// ============================================================================

const EQUIHASH_N: u32 = 96;
const EQUIHASH_K: u32 = 5;

struct RaceWinner {
    nonce: [u8; 32],
    soln_indices: Vec<u8>,
}

fn mine(
    rpc_url: String,
    keypair_path: PathBuf,
    max_blocks: u64,
    cu_limit: u32,
    threads: usize,
    full_gpu: bool,
) -> Result<()> {
    // Two pipelines: v0.1 hybrid uses a LeafGen + CPU Wagner workers;
    // v0.2 full_gpu uses GpuWagner single-threaded. v0.2 is wgpu-only
    // for now (CUDA Wagner kernels TODO); --full-gpu with EQUIUM_BACKEND=cuda
    // falls back to the hybrid path automatically.
    let leaf_gen: Option<Arc<dyn gpu::LeafGen>> = if !full_gpu {
        Some(Arc::from(make_leaf_gen()?))
    } else {
        None
    };
    let mut wagner_gpu = if full_gpu {
        Some(AnyWagner::new()?)
    } else {
        None
    };
    let adapter_name = match (&leaf_gen, &wagner_gpu) {
        (Some(g), _) => g.backend_label().to_string(),
        (_, Some(w)) => w.adapter_name().to_string(),
        _ => unreachable!(),
    };
    println!("GPU backend: {}", adapter_name);
    if full_gpu {
        println!("mode: v0.2 full-GPU (leaves + Wagner + solution scan on GPU)");
    } else {
        println!("mode: v0.1 hybrid (GPU leaves + CPU Wagner)");
    }

    let miner_kp = read_keypair_file(&keypair_path)
        .map_err(|e| anyhow!("read keypair {}: {}", keypair_path.display(), e))?;
    let miner = miner_kp.pubkey();

    let program_id = equium::ID;
    let rpc = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());
    let (config_pda, _) = Pubkey::find_program_address(&[CONFIG_SEED], &program_id);
    let (vault_pda, _) = Pubkey::find_program_address(&[VAULT_SEED], &program_id);

    let token_program_id = {
        let cfg = fetch_config(&rpc, &config_pda)?;
        let mint = rpc.get_account(&cfg.mint).context("fetch mint")?;
        mint.owner
    };

    let thread_count = if threads == 0 { num_cpus::get().max(1) } else { threads };
    let header_threads = if full_gpu {
        "1 single-threaded GPU pipeline".to_string()
    } else {
        format!("{} CPU + 1 shared GPU", thread_count)
    };
    println!(
        "miner   {}\nprogram {}\nrpc     {}\nthreads {}\n",
        short_pk(&miner),
        short_pk(&program_id),
        truncate(&rpc_url, 60),
        header_threads
    );

    let mut current_height: u64 = u64::MAX;
    let mut try_in_round: u32 = 0;
    let mut blocks_mined: u64 = 0;
    let mut total_nonces: u64 = 0;
    let started_at = Instant::now();

    loop {
        let cfg = fetch_config(&rpc, &config_pda)?;
        if !cfg.mining_open {
            println!("mining not open yet — sleeping 5s");
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        let miner_ata = get_associated_token_address_with_program_id(&miner, &cfg.mint, &token_program_id);

        if cfg.block_height != current_height {
            current_height = cfg.block_height;
            try_in_round = 0;
            println!(
                "\nround #{}  prize {} EQM  target 0x{}…",
                cfg.block_height,
                format_reward(cfg.current_epoch_reward),
                hex::encode(&cfg.current_target[..4])
            );
        }

        let input = build_input(&cfg.current_challenge, &miner.to_bytes(), cfg.block_height);
        let race_started = Instant::now();
        let (race_result, race_nonces_attempted) = if full_gpu {
            // Full-GPU: 32 nonces per round attempt. Each costs one
            // GPU end-to-end pass; refetching config every ~32 keeps
            // us responsive to height changes.
            let max_attempts = 32u64;
            let r = race_for_solution_full_gpu(
                wagner_gpu.as_mut().unwrap(),
                &input,
                &cfg.current_target,
                max_attempts,
            )?;
            (r, max_attempts)
        } else {
            // v0.1 hybrid: many CPU threads, each running its own
            // Wagner pass against shared GPU leaves.
            let max_per_thread = 64u64;
            let r = race_for_solution_gpu(
                leaf_gen.as_ref().unwrap().clone(),
                &input,
                &cfg.current_target,
                thread_count,
                max_per_thread,
            );
            (r, (thread_count as u64) * max_per_thread)
        };

        let elapsed = race_started.elapsed();
        match race_result {
            Some((winner, nonces_tried)) => {
                total_nonces = total_nonces.saturating_add(nonces_tried);
                try_in_round += 1;
                let session_secs = started_at.elapsed().as_secs_f64().max(0.001);
                let hashrate = total_nonces as f64 / session_secs;

                // Re-verify off-chain.
                let cand_hash = solution_hash(&winner.soln_indices, &input);
                if !hash_under_target(&cand_hash, &cfg.current_target) {
                    // This shouldn't happen — the race already checked target.
                    eprintln!("internal: winner above target, skipping");
                    continue;
                }

                match submit_mine(
                    &rpc,
                    &miner_kp,
                    &program_id,
                    &config_pda,
                    &cfg,
                    &vault_pda,
                    &miner_ata,
                    &token_program_id,
                    &winner.nonce,
                    winner.soln_indices.clone(),
                    cu_limit,
                ) {
                    Ok(sig) => {
                        blocks_mined += 1;
                        println!(
                            "  ✓ MINED  +{} EQM  try #{}  {}ms  {}",
                            format_reward(cfg.current_epoch_reward),
                            try_in_round,
                            elapsed.as_millis(),
                            fmt_hashrate(hashrate)
                        );
                        println!("    sig {}", short_sig(&sig));

                        if max_blocks > 0 && blocks_mined >= max_blocks {
                            println!("\nsession complete · {} blocks · {}", blocks_mined, fmt_hashrate(hashrate));
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        let reason = classify_submit_err(&e.to_string());
                        println!("  · submit failed: {}  ({}ms)", reason, elapsed.as_millis());
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
            None => {
                total_nonces = total_nonces.saturating_add(race_nonces_attempted);
                try_in_round += 1;
                let session_secs = started_at.elapsed().as_secs_f64().max(0.001);
                let hashrate = total_nonces as f64 / session_secs;
                println!(
                    "  · try #{}  exhausted  {}ms  {}",
                    try_in_round,
                    elapsed.as_millis(),
                    fmt_hashrate(hashrate)
                );
            }
        }
    }
}

/// Spawn `threads` CPU workers; each grinds nonces, dispatching leaf
/// generation to the shared GPU and running Wagner CPU-side. First
/// thread to find a below-target solution wins.
fn race_for_solution_gpu(
    gpu: Arc<dyn gpu::LeafGen>,
    input: &[u8; I_LEN],
    target: &[u8; 32],
    threads: usize,
    max_per_thread: u64,
) -> Option<(RaceWinner, u64)> {
    let stop = Arc::new(AtomicBool::new(false));
    let total_nonces = Arc::new(AtomicU64::new(0));
    let (tx, rx) = bounded::<RaceWinner>(1);

    let n_leaves = n_init_leaves(EQUIHASH_N, EQUIHASH_K) as u32;
    let leaf_size = leaf_bytes(EQUIHASH_N);

    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let gpu = gpu.clone();
        let input = *input;
        let target = *target;
        let stop = stop.clone();
        let total = total_nonces.clone();
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            let mut leaves_buf = vec![0u8; (n_leaves as usize) * leaf_size];
            let mut tried = 0u64;
            while tried < max_per_thread && !stop.load(Ordering::Relaxed) {
                let mut nonce = [0u8; 32];
                rng.fill(&mut nonce);
                tried += 1;

                // GPU: generate leaves for this nonce. Errors here are
                // logged but don't crash the worker — usually transient
                // (device timeout, driver hiccup); we just try the next nonce.
                if gpu.generate(&input, &nonce, n_leaves, &mut leaves_buf).is_err() {
                    continue;
                }

                if let Some(soln) =
                    try_nonce_with_leaves(EQUIHASH_N, EQUIHASH_K, &input, &nonce, &leaves_buf)
                {
                    let h = solution_hash(&soln, &input);
                    if hash_under_target(&h, &target) {
                        let _ = tx.send(RaceWinner { nonce, soln_indices: soln });
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            total.fetch_add(tried, Ordering::Relaxed);
        }));
    }
    drop(tx);

    let winner = rx.recv_timeout(Duration::from_secs(600)).ok();
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
    let nonces_tried = total_nonces.load(Ordering::Relaxed);
    winner.map(|w| (w, nonces_tried))
}

/// Single-threaded full-GPU race: try nonces sequentially through
/// `GpuWagner::run_nonce`. Wagner is GPU-bound, so multi-threading
/// here just contends on the single device; one thread saturates it.
fn race_for_solution_full_gpu(
    wagner_gpu: &mut AnyWagner,
    input: &[u8; I_LEN],
    target: &[u8; 32],
    max_attempts: u64,
) -> Result<Option<(RaceWinner, u64)>> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut tried = 0u64;
    while tried < max_attempts {
        let mut nonce = [0u8; 32];
        rng.fill(&mut nonce);
        tried += 1;

        let solutions = wagner_gpu.run_nonce(input, &nonce)?;
        for indices in solutions {
            let compressed = compress_indices_96_5(&indices);
            // Defense in depth: re-validate via the upstream verifier
            // before paying for an RPC tx. If the GPU produced a stale
            // or malformed solution, skip silently.
            if equihash::is_valid_solution(96, 5, input, &nonce, &compressed).is_err() {
                continue;
            }
            let h = solution_hash(&compressed, input);
            if hash_under_target(&h, target) {
                return Ok(Some((
                    RaceWinner { nonce, soln_indices: compressed },
                    tried,
                )));
            }
        }
    }
    Ok(None)
}

/// Compress 32 u32 solution indices into the 68-byte SPL-side format
/// (17 bits per index, packed big-endian). Mirrors
/// `equihash_core::solver::compress_indices` for (96, 5).
fn compress_indices_96_5(indices: &[u32; 32]) -> Vec<u8> {
    const BITS_PER: usize = 17; // cbits(96, 5) + 1 = 16 + 1
    let total_bits = BITS_PER * indices.len();
    let total_bytes = total_bits.div_ceil(8);
    let mut out = vec![0u8; total_bytes];
    let mut pos = 0usize;
    for &idx in indices {
        for b in (0..BITS_PER).rev() {
            let bit = (idx >> b) & 1;
            let byte = pos / 8;
            let shift = 7 - (pos % 8);
            out[byte] |= (bit as u8) << shift;
            pos += 1;
        }
    }
    out
}

// ============================================================================
// Solana RPC + helpers (mostly mirroring cli-miner)
// ============================================================================

fn fetch_config(rpc: &RpcClient, config_pda: &Pubkey) -> Result<EquiumConfig> {
    let acct = rpc.get_account(config_pda)?;
    let mut data = acct.data.as_slice();
    let cfg = EquiumConfig::try_deserialize(&mut data)?;
    Ok(cfg)
}

#[allow(clippy::too_many_arguments)]
fn submit_mine(
    rpc: &RpcClient,
    miner_kp: &Keypair,
    program_id: &Pubkey,
    config_pda: &Pubkey,
    cfg: &EquiumConfig,
    vault_pda: &Pubkey,
    miner_ata: &Pubkey,
    token_program_id: &Pubkey,
    nonce: &[u8; 32],
    soln_indices: Vec<u8>,
    cu_limit: u32,
) -> Result<String> {
    let miner = miner_kp.pubkey();
    let accounts = equium::accounts::Mine {
        miner,
        config: *config_pda,
        mint: cfg.mint,
        mineable_vault: *vault_pda,
        miner_ata: *miner_ata,
        token_program: *token_program_id,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: system_program::ID,
        slot_hashes: sysvar::slot_hashes::ID,
    }
    .to_account_metas(None);
    let accounts: Vec<AccountMeta> = accounts
        .into_iter()
        .map(|m| AccountMeta {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    let data = equium::instruction::Mine { nonce: *nonce, soln_indices }.data();
    let ix = Instruction { program_id: *program_id, accounts, data };
    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(cu_limit);

    let recent = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&[cu_ix, ix], Some(&miner), &[miner_kp], recent);
    let sig = rpc.send_and_confirm_transaction(&tx)?;
    Ok(sig.to_string())
}

fn classify_submit_err(s: &str) -> &'static str {
    if s.contains("0x1773") || s.contains("AboveTarget") {
        "above target"
    } else if s.contains("0x1772") || s.contains("InvalidEquihash") {
        "invalid equihash"
    } else if s.contains("0x1774") || s.contains("StaleChallenge") {
        "stale challenge"
    } else if s.contains("BlockhashNotFound") {
        "blockhash expired"
    } else {
        "submit error"
    }
}

fn format_reward(base: u64) -> String {
    let whole = base / 1_000_000;
    let frac = base % 1_000_000;
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{}.{:06}", whole, frac).trim_end_matches('0').to_string()
    }
}

fn fmt_hashrate(h: f64) -> String {
    if h >= 1000.0 {
        format!("{:.1} kH/s", h / 1000.0)
    } else {
        format!("{:.1} H/s", h)
    }
}

fn short_pk(pk: &Pubkey) -> String {
    let s = pk.to_string();
    format!("{}…{}", &s[..4], &s[s.len() - 4..])
}

fn short_sig(s: &str) -> String {
    if s.len() <= 12 { s.to_string() } else { format!("{}…{}", &s[..6], &s[s.len() - 6..]) }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() > n { format!("{}…", &s[..n]) } else { s.to_string() }
}

// silence Pubkey::from_str unused warning if any
const _: fn() = || {
    let _ = Pubkey::from_str;
};

// ============================================================================
// CPU reference (for verify, verify-cpu, bench)
// ============================================================================

/// CPU reference: replicate exactly what `equihash_core::solver::solve`
/// does to generate the first `n_leaves` leaves. Uses the upstream
/// `blake2b_simd` directly so the comparison is at the byte level.
fn cpu_leaves_reference(input: &[u8; I_LEN], nonce: &[u8; 32], n_leaves: u32) -> Vec<u8> {
    use blake2b_simd::Params;

    let mut personal = [0u8; 16];
    personal[..8].copy_from_slice(b"ZcashPoW");
    personal[8..12].copy_from_slice(&96u32.to_le_bytes());
    personal[12..16].copy_from_slice(&5u32.to_le_bytes());

    let base_state = Params::new()
        .hash_length(60)
        .personal(&personal)
        .to_state();

    let mut state_with_input = base_state.clone();
    state_with_input.update(input);
    state_with_input.update(nonce);

    let mut out = Vec::with_capacity((n_leaves as usize) * gpu::LEAF_BYTES);
    // Same loop as the shader: one BLAKE2b call per index, yields
    // gpu::LEAVES_PER_CALL leaves of LEAF_BYTES each.
    let n_calls = (n_leaves + gpu::LEAVES_PER_CALL - 1) / gpu::LEAVES_PER_CALL;
    for call_idx in 0..n_calls {
        let mut s = state_with_input.clone();
        s.update(&call_idx.to_le_bytes());
        let h = s.finalize();
        let bytes = h.as_bytes(); // 60 bytes
        for k in 0..(gpu::LEAVES_PER_CALL as usize) {
            let leaf_idx = (call_idx * gpu::LEAVES_PER_CALL) as usize + k;
            if leaf_idx >= n_leaves as usize {
                break;
            }
            out.extend_from_slice(&bytes[k * gpu::LEAF_BYTES..(k + 1) * gpu::LEAF_BYTES]);
        }
    }
    out
}
