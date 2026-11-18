// No console window for the GUI.
#![windows_subsystem = "windows"]

use ethos_bt_joystick::installer::{self, Action};
use ethos_bt_joystick::ui;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    // Elevated helper modes (started through UAC by the GUI, see installer.rs).
    if has(installer::ARG_INSTALL) {
        std::process::exit(installer::run_elevated(Action::Install));
    }
    if has(installer::ARG_UNINSTALL) {
        std::process::exit(installer::run_elevated(Action::Uninstall));
    }

    // Two instances would fight over the radio and the joystick.
    let _instance = unsafe { CreateMutexW(None, false, w!("Local\\EthosBtJoystick.SingleInstance")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            MessageBoxW(
                None,
                w!("Ethos BT Joystick is already running."),
                w!("Ethos BT Joystick"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return;
    }

    if let Err(error) = ui::run(has("--demo")) {
        let text: Vec<u16> = format!("Could not start the user interface:\n{error}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(None, windows::core::PCWSTR(text.as_ptr()), w!("Ethos BT Joystick"), MB_OK);
        }
        std::process::exit(1);
    }
}
