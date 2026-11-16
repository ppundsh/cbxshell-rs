use std::env;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

#[path = "../../build_res.rs"]
mod build_res;

fn main() {
    build_res::compile_pe_resources(&build_res::PeMetadata {
        internal_name: "CBXShell-Setup",
        original_filename: "CBXShell-Setup.exe",
        file_description: "CBX Shell Setup & Uninstallation Utility",
        is_dll: false,
    });
    build_res::harden_exe_linker();

    println!("cargo:rerun-if-env-changed=CBX_PAYLOAD_DIR");
    println!("cargo:rerun-if-env-changed=CBX_PACK_STAMP");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(&manifest_dir)
        .to_path_buf();

    let payload_dir = env::var("CBX_PAYLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root.join("target").join("release"));

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let zip_path = out_dir.join("payload.zip");

    let f = File::create(&zip_path).expect("Failed to create payload.zip");
    let mut zw = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let bin_files = ["cbxshell.dll", "cbx-manager.exe", "cbx-cli.exe"];
    for name in bin_files {
        let p = payload_dir.join(name);
        if p.exists() {
            if let Ok(bytes) = std::fs::read(&p) {
                zw.start_file(name, opt).unwrap();
                zw.write_all(&bytes).unwrap();
            }
        }
    }

    let doc_files = ["LICENSE", "README.md", "README.zh-TW.md"];
    for name in doc_files {
        let p = workspace_root.join(name);
        println!("cargo:rerun-if-changed={}", p.display());
        if p.exists() {
            if let Ok(bytes) = std::fs::read(&p) {
                zw.start_file(name, opt).unwrap();
                zw.write_all(&bytes).unwrap();
            }
        }
    }

    zw.finish().expect("Failed to finish payload.zip");

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "gnu" {
        println!("cargo:rustc-link-arg=-Wl,--dynamicbase");
        println!("cargo:rustc-link-arg=-Wl,--nxcompat");
        println!("cargo:rustc-link-arg=-Wl,--high-entropy-va");
    } else if Path::new("msvc").exists() || target_env == "msvc" {
        println!("cargo:rustc-link-arg=/DYNAMICBASE");
        println!("cargo:rustc-link-arg=/NXCOMPAT");
        println!("cargo:rustc-link-arg=/HIGHENTROPYVA");
    }
}
