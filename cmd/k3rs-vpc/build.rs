fn main() {
    // Only compile eBPF programs on Linux when the ebpf feature is enabled
    #[cfg(all(target_os = "linux", feature = "ebpf"))]
    {
        let ebpf_crate = std::path::PathBuf::from("../k3rs-vpc-ebpf");
        if ebpf_crate.exists() {
            aya_build::build_ebpf([ebpf_crate])
                .expect("failed to build eBPF programs");
        } else {
            println!("cargo:warning=k3rs-vpc-ebpf crate not found, skipping eBPF build");
        }
    }
}
