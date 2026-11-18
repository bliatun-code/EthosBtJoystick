//! Installs / removes the virtual joystick driver.
//!
//! The GUI is not elevated. When the user clicks "Install driver" it starts
//! itself again with `--install-driver` through the UAC prompt
//! ([`launch_elevated`]). The elevated instance ([`run_elevated`]) then
//!
//! 1. unpacks the embedded driver package to `%ProgramFiles%\EthosBtJoystick\driver`,
//! 2. creates a one-off self-signed code signing certificate on this PC, signs
//!    the driver catalog with it, trusts the *public* certificate
//!    (LocalMachine Root + TrustedPublisher) and deletes the private key,
//! 3. creates the root-enumerated device `root\EthosBtJoystick` and installs
//!    the driver on it.
//!
//! Everything is written to `%ProgramFiles%\EthosBtJoystick\install.log`.

use std::fmt::Write as _;
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{
    FOLDERID_ProgramFiles, SHGetKnownFolderPath, ShellExecuteExW, KF_FLAG_DEFAULT, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::joystick::{for_each_device, HARDWARE_ID};
use crate::util::{base64, from_wide_ptr, wide};

const DRIVER_DLL: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../driver/out/package/ethosjoy.dll"));
const DRIVER_INF: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../driver/out/package/ethosjoy.inf"));
const DRIVER_CAT: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../driver/out/package/ethosjoy.cat"));

/// Subject of the local signing certificate (also used to find it again).
const CERT_SUBJECT: &str = "CN=Ethos BT Joystick local driver signing";

/// {745a17a0-74d3-11d0-b6fe-00a0c90f57da}
const GUID_DEVCLASS_HIDCLASS: GUID = GUID::from_u128(0x745a17a0_74d3_11d0_b6fe_00a0c90f57da);

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ERROR_CANCELLED_HRESULT: i32 = 0x800704C7u32 as i32;

pub const ARG_INSTALL: &str = "--install-driver";
pub const ARG_UNINSTALL: &str = "--uninstall-driver";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    Uninstall,
}

// ---------------------------------------------------------------------------
// Unelevated side (GUI)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Outcome {
    pub action: Action,
    pub success: bool,
    /// Contents of the install log (or the error that prevented the launch).
    pub log: String,
}

#[derive(Debug)]
pub enum LaunchError {
    /// The user answered "No" in the UAC prompt.
    Cancelled,
    Other(String),
}

/// A running elevated instance of this program.
pub struct ElevatedProcess {
    action: Action,
    handle: isize,
}

// The process handle may be waited on from any thread.
unsafe impl Send for ElevatedProcess {}

impl ElevatedProcess {
    /// Blocks until the elevated instance has finished.
    pub fn wait(self) -> Outcome {
        let handle = HANDLE(self.handle as _);
        let mut exit_code = 1u32;
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            let _ = GetExitCodeProcess(handle, &mut exit_code);
            let _ = CloseHandle(handle);
        }
        let log = std::fs::read_to_string(log_path()).unwrap_or_default();
        Outcome { action: self.action, success: exit_code == 0, log }
    }
}

/// Starts this executable elevated (shows the UAC prompt).
pub fn launch_elevated(action: Action) -> Result<ElevatedProcess, LaunchError> {
    let exe = std::env::current_exe().map_err(|e| LaunchError::Other(e.to_string()))?;
    let exe = wide(&exe.to_string_lossy());
    let args = wide(match action {
        Action::Install => ARG_INSTALL,
        Action::Uninstall => ARG_UNINSTALL,
    });

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) if !info.hProcess.is_invalid() => {
            Ok(ElevatedProcess { action, handle: info.hProcess.0 as isize })
        }
        Ok(()) => Err(LaunchError::Other("no process handle".into())),
        Err(error) if error.code().0 == ERROR_CANCELLED_HRESULT => Err(LaunchError::Cancelled),
        Err(error) => Err(LaunchError::Other(error.message().to_string())),
    }
}

pub fn install_dir() -> PathBuf {
    let program_files = unsafe {
        match SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, HANDLE::default()) {
            Ok(path) => {
                let text = from_wide_ptr(path.0);
                CoTaskMemFree(Some(path.0 as _));
                PathBuf::from(text)
            }
            Err(_) => PathBuf::from(r"C:\Program Files"),
        }
    };
    program_files.join("EthosBtJoystick")
}

pub fn log_path() -> PathBuf {
    install_dir().join("install.log")
}

// ---------------------------------------------------------------------------
// Elevated side
// ---------------------------------------------------------------------------

struct Log {
    text: String,
}

impl Log {
    fn line(&mut self, message: impl AsRef<str>) {
        let _ = writeln!(self.text, "{}", message.as_ref());
    }

    fn save(&self) {
        let path = log_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, &self.text);
    }
}

/// Entry point of the elevated instance. Returns the process exit code.
pub fn run_elevated(action: Action) -> i32 {
    let mut log = Log { text: String::new() };
    log.line(format!("Ethos BT Joystick {} - {:?} driver", env!("CARGO_PKG_VERSION"), action));

    let result = match action {
        Action::Install => install(&mut log),
        Action::Uninstall => uninstall(&mut log),
    };

    let code = match result {
        Ok(()) => {
            log.line("RESULT: OK");
            0
        }
        Err(error) => {
            log.line(format!("RESULT: FAILED - {error}"));
            1
        }
    };
    log.save();
    code
}

fn install(log: &mut Log) -> Result<(), String> {
    // Start from a clean slate so that re-installing / upgrading always works.
    remove_devices(log);
    remove_driver_packages(log);

    let dir = install_dir().join("driver");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for (name, data) in [("ethosjoy.dll", DRIVER_DLL), ("ethosjoy.inf", DRIVER_INF), ("ethosjoy.cat", DRIVER_CAT)] {
        std::fs::write(dir.join(name), data).map_err(|e| format!("cannot write {name}: {e}"))?;
    }
    log.line(format!("Driver package unpacked to {}", dir.display()));

    log.line("Creating local signing certificate and signing the driver catalog ...");
    run_powershell(&sign_script(&dir), log)?;

    log.line("Installing the device ...");
    let inf = dir.join("ethosjoy.inf");
    if let Err(error) = unsafe { create_device_and_install(&inf, log) } {
        // do not leave a half installed device behind
        remove_devices(log);
        return Err(error);
    }
    Ok(())
}

fn uninstall(log: &mut Log) -> Result<(), String> {
    remove_devices(log);
    remove_driver_packages(log);

    log.line("Removing the local signing certificate ...");
    let certificates = run_powershell(&remove_cert_script(), log);

    let _ = std::fs::remove_dir_all(install_dir().join("driver"));
    certificates
}

/// PowerShell: create the certificate, sign `<dir>\ethosjoy.cat`, trust it.
pub fn sign_script(dir: &Path) -> String {
    SIGN_SCRIPT
        .replace("__DIR__", &ps_quote(&dir.to_string_lossy()))
        .replace("__SUBJECT__", &ps_quote(CERT_SUBJECT))
}

/// PowerShell: remove our certificate from the machine stores.
pub fn remove_cert_script() -> String {
    REMOVE_CERT_SCRIPT.replace("__SUBJECT__", &ps_quote(CERT_SUBJECT))
}

/// Creates the root-enumerated device node and installs the driver on it
/// (the same thing `devcon install ethosjoy.inf root\EthosBtJoystick` does).
unsafe fn create_device_and_install(inf: &Path, log: &mut Log) -> Result<(), String> {
    let hardware_id = wide(HARDWARE_ID);

    let set = SetupDiCreateDeviceInfoList(Some(&GUID_DEVCLASS_HIDCLASS), HWND::default())
        .map_err(|e| format!("SetupDiCreateDeviceInfoList: {e}"))?;

    let result = (|| -> Result<(), String> {
        let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        SetupDiCreateDeviceInfoW(
            set,
            w!("HIDClass"),
            &GUID_DEVCLASS_HIDCLASS,
            PCWSTR::null(),
            HWND::default(),
            DICD_GENERATE_ID,
            Some(&mut data),
        )
        .map_err(|e| format!("SetupDiCreateDeviceInfo: {e}"))?;

        // REG_MULTI_SZ: the id followed by an extra NUL
        let mut multi_sz: Vec<u8> = hardware_id.iter().flat_map(|c| c.to_le_bytes()).collect();
        multi_sz.extend_from_slice(&[0, 0]);
        SetupDiSetDeviceRegistryPropertyW(set, &mut data, SPDRP_HARDWAREID, Some(&multi_sz))
            .map_err(|e| format!("SetupDiSetDeviceRegistryProperty: {e}"))?;

        SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, Some(&data))
            .map_err(|e| format!("SetupDiCallClassInstaller(DIF_REGISTERDEVICE): {e}"))?;
        Ok(())
    })();
    let _ = SetupDiDestroyDeviceInfoList(set);
    result?;
    log.line(format!("Device node {HARDWARE_ID} created"));

    let inf_path = wide(&inf.to_string_lossy());
    let mut reboot = BOOL(0);
    UpdateDriverForPlugAndPlayDevicesW(
        HWND::default(),
        PCWSTR(hardware_id.as_ptr()),
        PCWSTR(inf_path.as_ptr()),
        INSTALLFLAG_FORCE,
        Some(&mut reboot),
    )
    .map_err(|e| format!("UpdateDriverForPlugAndPlayDevices: {e}"))?;

    log.line("Driver installed");
    if reboot.as_bool() {
        log.line("NOTE: Windows asks for a restart to finish the installation");
    }
    Ok(())
}

/// Removes every device node with our hardware id.
fn remove_devices(log: &mut Log) {
    let mut removed = 0;
    let mut failed = Vec::new();
    unsafe {
        for_each_device(|set, data| match SetupDiCallClassInstaller(DIF_REMOVE, set, Some(data)) {
            Ok(()) => removed += 1,
            Err(error) => failed.push(error.to_string()),
        });
    }
    if removed > 0 {
        log.line(format!("Removed {removed} device node(s)"));
    }
    for error in failed {
        log.line(format!("WARNING: could not remove a device node: {error}"));
    }
}

/// Deletes our driver package(s) from the driver store. Published packages are
/// copies of the INF named `%SystemRoot%\INF\oemNN.inf`.
fn remove_driver_packages(log: &mut Log) {
    let Some(windows) = std::env::var_os("SystemRoot").map(PathBuf::from) else { return };
    let Ok(entries) = std::fs::read_dir(windows.join("INF")) else { return };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !(name.starts_with("oem") && name.ends_with(".inf")) {
            continue;
        }
        let Ok(content) = std::fs::read(entry.path()) else { continue };
        if !inf_is_ours(&content) {
            continue;
        }
        let wide_name = wide(&name);
        if unsafe { SetupUninstallOEMInfW(PCWSTR(wide_name.as_ptr()), SUOI_FORCEDELETE, None) }.as_bool() {
            log.line(format!("Removed driver package {name}"));
        } else {
            let error = windows::core::Error::from_win32();
            log.line(format!("WARNING: could not remove driver package {name}: {error}"));
        }
    }
}

/// The INF may be stored as ANSI/UTF-8 or UTF-16.
fn inf_is_ours(content: &[u8]) -> bool {
    let needle = HARDWARE_ID.to_lowercase();
    let ansi = String::from_utf8_lossy(content).to_lowercase();
    if ansi.contains(&needle) {
        return true;
    }
    let utf16: Vec<u16> = content.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&utf16).to_lowercase().contains(&needle)
}

fn system_directory() -> PathBuf {
    let mut buffer = [0u16; 260];
    let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if len == 0 || len >= buffer.len() {
        PathBuf::from(r"C:\Windows\System32")
    } else {
        PathBuf::from(String::from_utf16_lossy(&buffer[..len]))
    }
}

/// Single-quoted PowerShell string literal.
fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Runs a script with Windows PowerShell (absolute path - we are elevated).
/// The script is passed as -EncodedCommand, so no script file policy applies.
fn run_powershell(script: &str, log: &mut Log) -> Result<(), String> {
    let exe = system_directory().join(r"WindowsPowerShell\v1.0\powershell.exe");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();

    let output = Command::new(&exe)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
        .arg(base64(&utf16))
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;

    for stream in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(stream);
        for line in text.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
            // -EncodedCommand reports errors as CLIXML on stderr; keep it readable
            if !line.starts_with("#< CLIXML") {
                log.line(format!("  {line}"));
            }
        }
    }

    if output.status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell step failed (exit code {:?})", output.status.code()))
    }
}

/// `__DIR__` / `__SUBJECT__` are replaced by quoted PowerShell strings (see `sign_script`).
const SIGN_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try {
    $dir = __DIR__
    $subject = __SUBJECT__
    $cat = Join-Path $dir 'ethosjoy.cat'

    # certificates from an earlier installation are no longer needed
    foreach ($store in 'Root', 'TrustedPublisher', 'CA', 'My') {
        Get-ChildItem "Cert:\LocalMachine\$store" | Where-Object { $_.Subject -eq $subject } | Remove-Item -Force
    }

    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject `
        -CertStoreLocation 'Cert:\LocalMachine\My' -KeyAlgorithm RSA -KeyLength 2048 `
        -HashAlgorithm SHA256 -KeyExportPolicy NonExportable -NotAfter (Get-Date).AddYears(25)
    try {
        $signature = Set-AuthenticodeSignature -FilePath $cat -Certificate $cert -HashAlgorithm SHA256
        if ($null -eq $signature.SignerCertificate) {
            throw "Signing the driver catalog failed: $($signature.StatusMessage)"
        }

        # trust the public certificate only
        $cer = Join-Path $dir 'ethosjoy.cer'
        Export-Certificate -Cert $cert -FilePath $cer -Type CERT | Out-Null
        Import-Certificate -FilePath $cer -CertStoreLocation 'Cert:\LocalMachine\Root' | Out-Null
        Import-Certificate -FilePath $cer -CertStoreLocation 'Cert:\LocalMachine\TrustedPublisher' | Out-Null
    }
    finally {
        # throw the private key away: nothing else can ever be signed with this certificate
        Remove-Item -Path ("Cert:\LocalMachine\My\" + $cert.Thumbprint) -DeleteKey -Force
        Get-ChildItem 'Cert:\LocalMachine\CA' | Where-Object { $_.Thumbprint -eq $cert.Thumbprint } | Remove-Item -Force
    }

    $check = Get-AuthenticodeSignature -FilePath $cat
    Write-Output ("Certificate " + $cert.Thumbprint + ", catalog signature: " + $check.Status)
    if ($check.Status -ne 'Valid') {
        throw "The driver catalog signature is not trusted: $($check.StatusMessage)"
    }
}
catch {
    Write-Output ("ERROR: " + $_.Exception.Message)
    exit 1
}
"#;

const REMOVE_CERT_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try {
    $subject = __SUBJECT__
    foreach ($store in 'Root', 'TrustedPublisher', 'CA', 'My') {
        Get-ChildItem "Cert:\LocalMachine\$store" | Where-Object { $_.Subject -eq $subject } | ForEach-Object {
            Remove-Item -Path $_.PSPath -Force
            Write-Output ("Removed certificate " + $_.Thumbprint + " from " + $store)
        }
    }
}
catch {
    Write-Output ("ERROR: " + $_.Exception.Message)
    exit 1
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_have_no_placeholders_left() {
        let sign = sign_script(Path::new(r"C:\Program Files\EthosBtJoystick\driver"));
        let remove = remove_cert_script();
        for script in [&sign, &remove] {
            assert!(!script.contains("__"), "unreplaced placeholder");
            assert!(script.contains(CERT_SUBJECT));
        }
        assert!(sign.contains(r"$dir = 'C:\Program Files\EthosBtJoystick\driver'"));
    }

    #[test]
    fn quoting() {
        assert_eq!(ps_quote(r"C:\Program Files\X"), r"'C:\Program Files\X'");
        assert_eq!(ps_quote("a'b"), "'a''b'");
    }

    #[test]
    fn embedded_inf_is_recognised() {
        assert!(inf_is_ours(DRIVER_INF));
        let utf16: Vec<u8> = String::from_utf8_lossy(DRIVER_INF).encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        assert!(inf_is_ours(&utf16));
        assert!(!inf_is_ours(b"[Version]\nSignature=\"$WINDOWS NT$\"\n"));
    }

    /// The -EncodedCommand plumbing: output is captured, failures are reported.
    #[test]
    fn powershell_runner() {
        let mut log = Log { text: String::new() };
        run_powershell("Write-Output ('sum=' + (20 + 22) + ' æøå')", &mut log).unwrap();
        assert!(log.text.contains("sum=42"), "log: {}", log.text);

        let mut log = Log { text: String::new() };
        assert!(run_powershell("Write-Output 'ERROR: nope'; exit 1", &mut log).is_err());
        assert!(log.text.contains("ERROR: nope"), "log: {}", log.text);
    }

    /// Parses (does not run) the embedded scripts with the real PowerShell
    /// parser, so a typo cannot slip through to the elevated installer.
    #[test]
    fn embedded_scripts_are_valid_powershell() {
        let dir = std::env::temp_dir().join(format!("ethosjoy-script-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let scripts = [
            ("sign.ps1", sign_script(Path::new(r"C:\Program Files\EthosBtJoystick\driver"))),
            ("remove.ps1", remove_cert_script()),
        ];
        for (name, script) in scripts {
            let path = dir.join(name);
            // UTF-8 with BOM so Windows PowerShell reads it as UTF-8
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(script.as_bytes());
            std::fs::write(&path, bytes).unwrap();

            let check = format!(
                "$errors = $null; \
                 [void][System.Management.Automation.Language.Parser]::ParseFile({}, [ref]$null, [ref]$errors); \
                 if ($errors.Count -gt 0) {{ $errors | ForEach-Object {{ Write-Output $_.ToString() }}; exit 1 }}",
                ps_quote(&path.to_string_lossy())
            );
            let mut log = Log { text: String::new() };
            let result = run_powershell(&check, &mut log);
            assert!(result.is_ok(), "{name} has syntax errors:\n{}", log.text);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
