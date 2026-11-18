use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct PeMetadata<'a> {
    pub internal_name: &'a str,
    pub original_filename: &'a str,
    pub file_description: &'a str,
    pub is_dll: bool,
}

/// Emits linker hardening for executables (not the shell-extension DLL).
///
/// `/DEPENDENTLOADFLAG:0x800` (= `LOAD_LIBRARY_SEARCH_SYSTEM32`) makes the Windows loader resolve
/// the executable's *static* DLL imports from System32 only, so a planted `version.dll`,
/// `dwmapi.dll`, `uxtheme.dll`, ... sitting next to `CBXShell-Setup.exe` in a Downloads/Temp folder
/// is never loaded (binary-planting / DLL search-order hijacking).
#[allow(dead_code)]
pub fn harden_exe_linker() {
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
        // Record only the PDB file name (not the build machine's absolute path) in the PE.
        println!("cargo:rustc-link-arg-bins=/PDBALTPATH:%_PDB%");
    }
}

pub fn compile_pe_resources(meta: &PeMetadata) {
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(&manifest_dir);
    let assets_dir = workspace_root.join("assets");
    let ico_path = assets_dir.join("app.ico");
    let manifest_path = assets_dir.join("app.manifest");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc_path = out_dir.join("version.rc");
    let res_path = out_dir.join("version.res");

    let file_type = if meta.is_dll { "0x2L" } else { "0x1L" };

    let manifest_and_icon = if !meta.is_dll {
        format!(
            "1 RT_MANIFEST \"{manifest}\"\r\n\
             1 ICON \"{icon}\"\r\n",
            manifest = manifest_path.to_string_lossy().replace('\\', "/"),
            icon = ico_path.to_string_lossy().replace('\\', "/")
        )
    } else {
        String::new()
    };

    // Version resource is derived from the crate version (workspace `version` in Cargo.toml),
    // so `bump-version.ps1` is the only place a release number ever has to change.
    let ver = |k: &str| env::var(k).ok().and_then(|v| v.parse::<u16>().ok()).unwrap_or(0);
    let (v_major, v_minor, v_patch) = (
        ver("CARGO_PKG_VERSION_MAJOR"),
        ver("CARGO_PKG_VERSION_MINOR"),
        ver("CARGO_PKG_VERSION_PATCH"),
    );
    let rc_content = format!(
        "{header}\
         1 VERSIONINFO\r\n\
         FILEVERSION {v_major},{v_minor},{v_patch},0\r\n\
         PRODUCTVERSION {v_major},{v_minor},{v_patch},0\r\n\
         FILEFLAGSMASK 0x3fL\r\n\
         FILEFLAGS 0x0L\r\n\
         FILEOS 0x40004L\r\n\
         FILETYPE {file_type}\r\n\
         FILESUBTYPE 0x0L\r\n\
         BEGIN\r\n\
             BLOCK \"StringFileInfo\"\r\n\
             BEGIN\r\n\
                 BLOCK \"040904b0\"\r\n\
                 BEGIN\r\n\
                     VALUE \"CompanyName\", \"CBXShell Open Source Project\\0\"\r\n\
                     VALUE \"FileDescription\", \"{desc}\\0\"\r\n\
                     VALUE \"FileVersion\", \"{v_major}.{v_minor}.{v_patch}.0\\0\"\r\n\
                     VALUE \"InternalName\", \"{internal}\\0\"\r\n\
                     VALUE \"LegalCopyright\", \"GNU General Public License v3.0\\0\"\r\n\
                     VALUE \"OriginalFilename\", \"{orig_file}\\0\"\r\n\
                     VALUE \"ProductName\", \"CBX Shell\\0\"\r\n\
                     VALUE \"ProductVersion\", \"{v_major}.{v_minor}.{v_patch}.0\\0\"\r\n\
                 END\r\n\
             END\r\n\
             BLOCK \"VarFileInfo\"\r\n\
             BEGIN\r\n\
                 VALUE \"Translation\", 0x409, 1200\r\n\
             END\r\n\
         END\r\n",
        header = manifest_and_icon,
        file_type = file_type,
        desc = meta.file_description,
        internal = meta.internal_name,
        orig_file = meta.original_filename,
        v_major = v_major,
        v_minor = v_minor,
        v_patch = v_patch,
    );

    fs::write(&rc_path, rc_content).expect("Failed to write version.rc");

    let Some(rc_exe) = find_rc_exe() else {
        println!("cargo:warning=rc.exe not found; skipping Windows PE resource embedding");
        return;
    };

    let status = Command::new(&rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .expect("Failed to execute rc.exe");

    if !status.success() {
        panic!("rc.exe failed with exit code {:?}", status.code());
    }

    if meta.is_dll {
        println!("cargo:rustc-cdylib-link-arg={}", res_path.display());
    } else {
        println!("cargo:rustc-link-arg={}", res_path.display());
    }
    println!("cargo:rerun-if-changed={}", ico_path.display());
    println!("cargo:rerun-if-changed={}", manifest_path.display());
}

fn find_rc_exe() -> Option<PathBuf> {
    if let Ok(output) = Command::new("where.exe").arg("rc.exe").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = s.lines().next() {
                let p = PathBuf::from(line.trim());
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    // Derive SDK roots from the build machine's environment instead of hard-coding paths.
    let sdk_roots: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(|v| std::env::var_os(v))
        .map(|p| PathBuf::from(p).join("Windows Kits").join("10").join("bin"))
        .collect();
    for root in &sdk_roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            let mut dirs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            for d in dirs.iter().rev() {
                for arch in ["x64", "x86", "arm64"] {
                    let p = d.join(arch).join("rc.exe");
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}
