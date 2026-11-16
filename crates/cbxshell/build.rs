use std::env;
use std::path::PathBuf;

#[path = "../../build_res.rs"]
mod build_res;

fn main() {
    build_res::compile_pe_resources(&build_res::PeMetadata {
        internal_name: "cbxshell",
        original_filename: "cbxshell.dll",
        file_description: "CBX Shell - Windows Explorer Comic & E-Book Thumbnail Handler",
        is_dll: true,
    });

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let def_file = manifest_dir.join("cbxshell.def");
    println!("cargo:rerun-if-changed={}", def_file.display());

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if target_env == "msvc" {
        println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def_file.display());
        println!("cargo:rustc-cdylib-link-arg=/DYNAMICBASE");
        println!("cargo:rustc-cdylib-link-arg=/NXCOMPAT");
        // Record only the PDB file name (not the build machine's absolute path) in the PE.
        println!("cargo:rustc-cdylib-link-arg=/PDBALTPATH:%_PDB%");
        if arch == "x86_64" {
            println!("cargo:rustc-cdylib-link-arg=/HIGHENTROPYVA");
            println!("cargo:rustc-cdylib-link-arg=/CETCOMPAT");
        }
    } else if target_env == "gnu" {
        // Pure-Rust GNU toolchain (requires zero Visual Studio Build Tools!)
        println!("cargo:rustc-cdylib-link-arg={}", def_file.display());
        println!("cargo:rustc-cdylib-link-arg=-Wl,--dynamicbase");
        println!("cargo:rustc-cdylib-link-arg=-Wl,--nxcompat");
        if arch == "x86_64" {
            println!("cargo:rustc-cdylib-link-arg=-Wl,--high-entropy-va");
        }
    }
}
