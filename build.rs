fn main() {
    #[cfg(feature = "rv32-sim")]
    {
        let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
        std::fs::copy("memory.x", out_dir.join("memory.x")).expect("copy memory.x");
        println!("cargo:rustc-link-search={}", out_dir.display());
        println!("cargo:rerun-if-changed=memory.x");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
