#[path = "../../build_res.rs"]
mod build_res;

fn main() {
    build_res::compile_pe_resources(&build_res::PeMetadata {
        internal_name: "cbx-manager",
        original_filename: "cbx-manager.exe",
        file_description: "CBX Shell Manager - Thumbnail & Association Configuration",
        is_dll: false,
    });
    build_res::harden_exe_linker();
}
