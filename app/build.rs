use std::path::Path;

/// The driver package is embedded into the executable (see `src/installer.rs`),
/// so it has to be built before the app. Give a clear message if it is missing.
fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let package = Path::new(&manifest).join("..").join("driver").join("out").join("package");

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
