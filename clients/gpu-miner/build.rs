//! Build-time CUDA kernel compilation.
//!
//! When the `cuda` cargo feature is enabled, this invokes `nvcc` to
//! compile each `cuda/*.cu` source into PTX. The PTX text is then
//! `include_bytes!`'d at compile time into the Rust binary so the
//! runtime has no separate file dependency on the user's box.
//!
//! Without the `cuda` feature this does nothing — `wgpu` is the only
//! backend in that build, and we don't want to require nvcc on Macs
//! or other non-CUDA hosts.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Re-run if the .cu files or this script change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cuda");

    // Cargo sets CARGO_FEATURE_<NAME> for each enabled feature.
    if env::var("CARGO_FEATURE_CUDA").is_err() {
        return;
    }

    let cuda_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cuda");
    if !cuda_dir.exists() {
        // Feature enabled but no kernels yet — silent ok, the runtime
        // code will fail at link time if it tries to include the PTX.
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let nvcc = env::var("NVCC").unwrap_or_else(|_| "nvcc".to_string());

    // Confirm nvcc exists. If not, emit a clear error pointing the
    // user at the apt-install line instead of letting the spawn
    // fail later with a cryptic "No such file or directory".
    let nvcc_present = Command::new(&nvcc)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !nvcc_present {
        panic!(
            "feature `cuda` is enabled but `{nvcc}` is not on PATH.\n\
             \n\
             On Ubuntu / Debian:  sudo apt install -y nvidia-cuda-toolkit\n\
             Or use a CUDA *devel* container image (e.g.\n\
             nvidia/cuda:12.4.1-devel-ubuntu22.04) instead of -runtime.\n"
        );
    }

    // Compile every .cu in cuda/ to a sibling .ptx. We target sm_70
    // (Volta) as a baseline — PTX is forward-compatible up to JIT
    // recompilation, so a single binary runs from V100 / T4 / A100 /
    // RTX 30xx / RTX 40xx all the way through Blackwell.
    let sources: Vec<_> = fs::read_dir(&cuda_dir)
        .expect("read cuda/")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("cu"))
        .map(|e| e.path())
        .collect();

    if sources.is_empty() {
        return;
    }

    for src in &sources {
        let stem = src.file_stem().unwrap().to_string_lossy().to_string();
        let dst = out_dir.join(format!("{stem}.ptx"));
        compile_cu(&nvcc, src, &dst);
        // Export the path so cuda.rs can `include_bytes!` it via the
        // OUT_DIR env var.
        println!("cargo:rustc-env=EQUIUM_PTX_{}={}", stem.to_uppercase(), dst.display());
        println!("cargo:rerun-if-changed={}", src.display());
    }
}

/// Detect the compute capability of the first available NVIDIA GPU.
/// Falls back to "compute_70" (Volta) if detection fails — PTX is
/// forward-compatible so the binary still runs on newer cards via JIT.
fn detect_compute_arch(nvcc: &str) -> String {
    // Honour an explicit override, e.g. CUDA_ARCH=sm_120 for Blackwell.
    if let Ok(arch) = env::var("CUDA_ARCH") {
        let arch = arch.trim().to_string();
        // Accept both "sm_120" and "compute_120" spellings.
        return if arch.starts_with("sm_") {
            arch.replacen("sm_", "compute_", 1)
        } else {
            arch
        };
    }

    // Ask nvidia-smi for the compute capability of device 0.
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=compute_cap", "--format=csv,noheader", "--id=0"])
        .output();

    if let Ok(out) = out {
        if out.status.success() {
            let cap = String::from_utf8_lossy(&out.stdout);
            let cap = cap.trim().replace('.', "");
            if !cap.is_empty() {
                return format!("compute_{cap}");
            }
        }
    }

    // Safe baseline: Volta (sm_70). PTX is forward-compatible.
    "compute_70".to_string()
}

fn compile_cu(nvcc: &str, src: &Path, dst: &Path) {
    let arch = detect_compute_arch(nvcc);
    eprintln!("equium-gpu-miner build.rs: compiling {} with --gpu-architecture={arch}", src.display());

    let status = Command::new(nvcc)
        .arg("--ptx")
        .arg(format!("--gpu-architecture={arch}"))
        .arg("-O3")
        .arg("--use_fast_math")
        .arg("-o")
        .arg(dst)
        .arg(src)
        .status()
        .unwrap_or_else(|e| panic!("nvcc spawn failed for {}: {e}", src.display()));
    if !status.success() {
        panic!(
            "nvcc failed for {} (exit {:?})\n\
             If you see \"Unsupported gpu architecture\", try:\n\
             CUDA_ARCH=sm_XX cargo build --features cuda\n\
             (replace XX with your GPU's compute capability, e.g. 89 for RTX 4090, 120 for RTX 5090)",
            src.display(), status.code()
        );
    }
}
