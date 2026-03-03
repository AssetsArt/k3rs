fn main() {
    // Only compile eBPF programs on Linux when the ebpf feature is enabled
    #[cfg(all(target_os = "linux", feature = "ebpf"))]
    {
        let ebpf_crate = std::path::PathBuf::from("../k3rs-vpc-ebpf");
        if ebpf_crate.exists() {
            build_ebpf(&ebpf_crate);
        } else {
            println!("cargo:warning=k3rs-vpc-ebpf crate not found, skipping eBPF build");
        }
    }
}

/// Build the eBPF programs using cargo +nightly directly.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn build_ebpf(ebpf_crate: &std::path::Path) {
    use std::process::Command;

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_dir = std::path::PathBuf::from(&out_dir);
    let target_dir = out_dir.join("ebpf-target");

    let manifest_path = ebpf_crate.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", ebpf_crate.display());

    // Determine target endianness
    let endian = std::env::var("CARGO_CFG_TARGET_ENDIAN").unwrap_or_else(|_| "little".to_string());
    let bpf_prefix = if endian == "big" { "bpfeb" } else { "bpfel" };
    let target = format!("{bpf_prefix}-unknown-none");

    // Determine BPF target arch
    let target_arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "aarch64".to_string());

    let mut rustflags = String::new();
    rustflags.push_str(&format!("--cfg=bpf_target_arch=\"{target_arch}\""));
    rustflags.push('\x1f');
    rustflags.push_str("-Cdebuginfo=2");
    rustflags.push('\x1f');
    rustflags.push_str("-Clink-arg=--btf");

    let status = Command::new("rustup")
        .args([
            "run",
            "nightly",
            "cargo",
            "build",
            "--manifest-path",
            &manifest_path.to_string_lossy(),
            "-Z",
            "build-std=core",
            "--bins",
            "--release",
            "--target",
            &target,
            "--target-dir",
            &target_dir.to_string_lossy(),
        ])
        .env_remove("RUSTC")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env("CARGO_ENCODED_RUSTFLAGS", &rustflags)
        .status()
        .expect("failed to run cargo build for eBPF programs");

    if !status.success() {
        panic!("eBPF build failed");
    }

    // Find the compiled binary in the target directory
    let bin_path = target_dir
        .join(&target)
        .join("release")
        .join("k3rs-vpc-ebpf");
    if !bin_path.exists() {
        panic!(
            "eBPF binary not found at {} after successful build",
            bin_path.display()
        );
    }

    // Copy to OUT_DIR for include_bytes!
    let dest = out_dir.join("k3rs-vpc-ebpf");
    std::fs::copy(&bin_path, &dest).unwrap_or_else(|e| {
        panic!(
            "failed to copy eBPF binary {} → {}: {}",
            bin_path.display(),
            dest.display(),
            e
        )
    });
}
