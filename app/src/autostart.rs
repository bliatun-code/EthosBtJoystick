//! "Start with Windows": an entry under the current user's Run key that starts
//! the program minimised to the notification area at sign-in.

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use crate::util::wide;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "EthosBtJoystick";

/// Command line switch: start hidden, with only the tray icon.
pub const ARG_MINIMIZED: &str = "--minimized";

/// The command Windows should run at sign-in for *this* executable.
fn command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {ARG_MINIMIZED}", exe.display()))
}

/// True if the entry exists and points at this executable.
pub fn is_enabled() -> bool {
    match (read(RUN_KEY, VALUE_NAME), command_line()) {
        (Some(current), Some(expected)) => current.eq_ignore_ascii_case(&expected),
        _ => false,
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let command = command_line().ok_or("the path of the program is unknown")?;
        write(RUN_KEY, VALUE_NAME, &command)
    } else {
        delete(RUN_KEY, VALUE_NAME)
    }
}

fn read(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buffer = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as _),
            Some(&mut size),
        )
    };
    if result != ERROR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

fn write(key: &str, name: &str, value: &str) -> Result<(), String> {
    let (key, name, value) = (wide(key), wide(name), wide(value));
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(value.as_ptr() as _),
            (value.len() * 2) as u32,
        )
    };
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(windows::core::Error::from(result.to_hresult()).message().to_string())
    }
}

fn delete(key: &str, name: &str) -> Result<(), String> {
    let (key, name) = (wide(key), wide(name));
    let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr())) };
    // "not found" is fine: the entry is gone either way
    if result == ERROR_SUCCESS || read_exists(&key, &name).is_none() {
        Ok(())
    } else {
        Err(windows::core::Error::from(result.to_hresult()).message().to_string())
    }
}

fn read_exists(key: &[u16], name: &[u16]) -> Option<()> {
    let mut size = 0u32;
    let result = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut size))
    };
    (result == ERROR_SUCCESS).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{RegDeleteKeyW, RegDeleteTreeW};

    /// Exercises the registry helpers in a private test key, never in the
    /// real Run key.
    #[test]
    fn registry_round_trip() {
        let key = format!(r"Software\EthosBtJoystick\Test-{}", std::process::id());
        assert_eq!(read(&key, "Value"), None);

        write(&key, "Value", r#""C:\Program Files\X\App.exe" --minimized"#).unwrap();
        assert_eq!(read(&key, "Value").as_deref(), Some(r#""C:\Program Files\X\App.exe" --minimized"#));

        delete(&key, "Value").unwrap();
        assert_eq!(read(&key, "Value"), None);
        delete(&key, "Value").unwrap(); // deleting a missing value is not an error

        // remove the test key, and its parent if nothing else is in it
        let (wide_key, parent) = (wide(&key), wide(r"Software\EthosBtJoystick"));
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wide_key.as_ptr()));
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(wide_key.as_ptr()));
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(parent.as_ptr()));
        }
    }

    #[test]
    fn command_line_is_quoted_and_minimized() {
        let command = command_line().unwrap();
        assert!(command.starts_with('"'));
        assert!(command.ends_with("\" --minimized"));
    }
}
