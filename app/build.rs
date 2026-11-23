use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    check_driver_package(&manifest);
    embed_resources(&manifest);
}

/// The driver package is embedded into the executable (see `src/installer.rs`),
/// so it has to be built before the app. Give a clear message if it is missing.
fn check_driver_package(manifest: &Path) {
    let package = manifest.join("..").join("driver").join("out").join("package");

    for file in ["ethosjoy.dll", "ethosjoy.inf", "ethosjoy.cat"] {
        let path = package.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.exists() {
            panic!(
                "\n\nDriver package file missing: {}\nBuild the driver first:  powershell -ExecutionPolicy Bypass -File driver\\build.ps1\n(or run build.ps1 in the repository root, which builds everything)\n\n",
                path.display()
            );
        }
    }
}

/// Gives the exe its icon and version information - what Explorer, shortcuts
/// and Task Manager show. The resource script is written here so that the
/// version always follows Cargo.toml, compiled with the Windows SDK's rc.exe
/// and linked into the program (not into the tests and examples).
fn embed_resources(manifest: &Path) {
    let icon = manifest.join("assets").join("EthosBtJoystick.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-env-changed=RC");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let number = |part: &str| std::env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap();
    let numbers = format!("{},{},{},0", number("MAJOR"), number("MINOR"), number("PATCH"));

    let script = format!(
        r#"#pragma code_page(65001)

1 ICON "{icon}"

1 VERSIONINFO
 FILEVERSION {numbers}
 PRODUCTVERSION {numbers}
 FILEFLAGSMASK 0x3fL
 FILEFLAGS 0x0L
 FILEOS 0x40004L
 FILETYPE 0x1L
 FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Ethos BT Joystick"
            VALUE "FileDescription", "Ethos BT Joystick"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "EthosBtJoystick.exe"
            VALUE "LegalCopyright", "Copyright (c) 2026 bliatun-code. MIT License."
            VALUE "OriginalFilename", "EthosBtJoystick.exe"
            VALUE "ProductName", "Ethos BT Joystick"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let (rc_file, res_file) = (out.join("app.rc"), out.join("app.res"));
    std::fs::write(&rc_file, script).unwrap();

    let rc = find_rc().unwrap_or_else(|| {
        panic!(
            "\n\nrc.exe (the resource compiler of the Windows SDK) was not found.\nInstall the Windows SDK with the Visual Studio Build Tools, or set the environment variable RC to the full path of rc.exe.\n\n"
        )
    });
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_file)
        .arg(&rc_file)
        .status()
        .unwrap_or_else(|error| panic!("cannot start {}: {error}", rc.display()));
    if !status.success() {
        panic!("{} failed on {}", rc.display(), rc_file.display());
    }

    println!("cargo:rustc-link-arg-bins={}", res_file.display());
}

/// rc.exe: the one named in `RC`, else one on PATH (a Visual Studio developer
/// prompt has it), else the newest Windows SDK in the default location.
fn find_rc() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RC").map(PathBuf::from) {
        return path.exists().then_some(path);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        if let Some(found) = std::env::split_paths(&paths).map(|dir| dir.join("rc.exe")).find(|path| path.exists()) {
            return Some(found);
        }
    }

    let kits = PathBuf::from(std::env::var_os("ProgramFiles(x86)")?).join("Windows Kits").join("10").join("bin");
    let mut found: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(kits)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let rc = entry.path().join("x64").join("rc.exe");
            // the folder is the SDK version, for example 10.0.26100.0
            let version: Vec<u32> = entry.file_name().to_string_lossy().split('.').map(|n| n.parse().ok()).collect::<Option<_>>()?;
            rc.exists().then_some((version, rc))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, rc)| rc)
}
