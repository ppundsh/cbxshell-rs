#[path = "../../build_res.rs"]
mod build_res;

fn main() {
    build_res::compile_pe_resources(&build_res::PeMetadata {
        internal_name: "cbx-cli",
        original_filename: "cbx-cli.exe",
        file_description: "CBX Shell Command Line & Self-Test Utility",
        is_dll: false,
    });
    build_res::harden_exe_linker();
}
