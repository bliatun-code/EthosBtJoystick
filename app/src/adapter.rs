//! Facts about the PC's Bluetooth adapter, for the diagnostics view: which
//! adapter, which Bluetooth version it speaks and how old its driver is.
//! (An adapter from the Bluetooth 4.x days with a years-old driver is the
//! first suspect when a connection keeps dropping.)

use std::mem::size_of;

use windows::core::{GUID, PCWSTR};
use windows::Devices::Bluetooth::BluetoothAdapter;
use windows::Devices::Radios::RadioState;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_DriverDate, DEVPKEY_Device_DriverProvider, DEVPKEY_Device_DriverVersion,
    DEVPKEY_Device_Manufacturer, DEVPKEY_NAME, DEVPROPKEY, DEVPROPTYPE,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::System::IO::DeviceIoControl;

use crate::util::{from_wide_ptr, wide};

/// Device interface of the Bluetooth radio (bthport).
const GUID_BTHPORT_DEVICE_INTERFACE: GUID = GUID::from_u128(0x0850302a_b344_4fda_9be9_90576b8d46f0);
/// Returns a BTH_LOCAL_RADIO_INFO for the radio.
const IOCTL_BTH_GET_LOCAL_INFO: u32 = 0x0041_0000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdapterInfo {
    /// Device name, e.g. "Intel(R) Wireless Bluetooth(R)". Empty if no adapter was found.
    pub name: String,
    /// Chip maker according to the adapter itself (Bluetooth company id).
    pub chip_maker: String,
    /// "4.2", "5.3" ... from the adapter's HCI version.
    pub bluetooth_version: String,
    pub driver_provider: String,
    pub driver_version: String,
    /// yyyy-mm-dd
    pub driver_date: String,
    pub low_energy: Option<bool>,
    pub central_role: Option<bool>,
    /// The radio is switched on in Windows.
    pub radio_on: Option<bool>,
}

impl AdapterInfo {
    pub fn found(&self) -> bool {
        !self.name.is_empty() || self.low_energy.is_some()
    }

    /// Year of the driver, if known.
    pub fn driver_year(&self) -> Option<u32> {
        self.driver_date.get(..4)?.parse().ok()
    }
}

/// Queries Windows. Takes a moment (device enumeration and two WinRT calls),
/// so call it from a background thread.
pub fn query() -> AdapterInfo {
    let mut info = AdapterInfo::default();
    unsafe { read_radio_device(&mut info) };
    read_capabilities(&mut info);
    info
}

/// Windows build, e.g. "26100.2314 (24H2)".
pub fn windows_build() -> String {
    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = registry_string(key, "CurrentBuild").unwrap_or_default();
    let revision = registry_dword(key, "UBR");
    let display = registry_string(key, "DisplayVersion").unwrap_or_default();

    let mut text = build;
    if let Some(revision) = revision {
        text.push_str(&format!(".{revision}"));
    }
    if !display.is_empty() {
        text.push_str(&format!(" ({display})"));
    }
    text
}

fn read_capabilities(info: &mut AdapterInfo) {
    let Ok(adapter) = BluetoothAdapter::GetDefaultAsync().and_then(|operation| operation.get()) else { return };
    info.low_energy = adapter.IsLowEnergySupported().ok();
    info.central_role = adapter.IsCentralRoleSupported().ok();
    if let Ok(radio) = adapter.GetRadioAsync().and_then(|operation| operation.get()) {
        info.radio_on = radio.State().ok().map(|state| state == RadioState::On);
        if info.name.is_empty() {
            info.name = radio.Name().map(|name| name.to_string()).unwrap_or_default();
        }
    }
}

unsafe fn read_radio_device(info: &mut AdapterInfo) {
    let guid = GUID_BTHPORT_DEVICE_INTERFACE;
    let Ok(set) =
        SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), HWND::default(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
    else {
        return;
    };

    let mut interface =
        SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
    if SetupDiEnumDeviceInterfaces(set, None, &guid, 0, &mut interface).is_ok() {
        let mut device = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let mut required = 0u32;
        let _ = SetupDiGetDeviceInterfaceDetailW(set, &interface, None, 0, Some(&mut required), None);
        if required > 0 {
            let mut buffer = vec![0u32; (required as usize).div_ceil(4) + 1];
            let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &interface, Some(detail), required, None, Some(&mut device))
                .is_ok()
            {
                let path = from_wide_ptr(std::ptr::addr_of!((*detail).DevicePath) as *const u16);
                read_radio_version(&path, info);

                info.name = device_string(set, &device, &DEVPKEY_NAME).unwrap_or_default();
                info.driver_provider = device_string(set, &device, &DEVPKEY_Device_DriverProvider).unwrap_or_default();
                info.driver_version = device_string(set, &device, &DEVPKEY_Device_DriverVersion).unwrap_or_default();
                info.driver_date = device_date(set, &device, &DEVPKEY_Device_DriverDate).unwrap_or_default();
                if info.chip_maker.is_empty() {
                    info.chip_maker = device_string(set, &device, &DEVPKEY_Device_Manufacturer).unwrap_or_default();
                }
            }
        }
    }
    let _ = SetupDiDestroyDeviceInfoList(set);
}

/// Asks the radio for its HCI version and maker.
unsafe fn read_radio_version(path: &str, info: &mut AdapterInfo) {
    let wide_path = wide(path);
    let Ok(handle) = CreateFileW(
        PCWSTR(wide_path.as_ptr()),
        0, // the query needs no read or write access
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        HANDLE::default(),
    ) else {
        return;
    };

    // BTH_LOCAL_RADIO_INFO: BTH_DEVICE_INFO (272 bytes), flags u32,
    // hciRevision u16, hciVersion u8, then BTH_RADIO_INFO { features u64,
    // mfg u16, lmpSubversion u16, lmpVersion u8 }. The header packs the
    // structure to 1 byte (292 bytes); cope with a padded layout (296) too.
    let mut buffer = [0u8; 320];
    let mut returned = 0u32;
    let ok = DeviceIoControl(
        handle,
        IOCTL_BTH_GET_LOCAL_INFO,
        None,
        0,
        Some(buffer.as_mut_ptr() as _),
        buffer.len() as u32,
        Some(&mut returned),
        None,
    )
    .is_ok();
    let _ = CloseHandle(handle);
    if !ok || returned < 292 {
        return;
    }

    let hci_version = buffer[278];
    let maker_offset = if returned >= 296 { 288 } else { 287 };
    let maker = u16::from_le_bytes([buffer[maker_offset], buffer[maker_offset + 1]]);
    info.bluetooth_version = bluetooth_version(hci_version).to_string();
    info.chip_maker = company_name(maker);
}

/// HCI / LMP version number -> Bluetooth core specification version.
pub fn bluetooth_version(hci_version: u8) -> &'static str {
    match hci_version {
        0 => "1.0b",
        1 => "1.1",
        2 => "1.2",
        3 => "2.0",
        4 => "2.1",
        5 => "3.0",
        6 => "4.0",
        7 => "4.1",
        8 => "4.2",
        9 => "5.0",
        10 => "5.1",
        11 => "5.2",
        12 => "5.3",
        13 => "5.4",
        14 => "6.0",
        _ => "",
    }
}

/// The handful of Bluetooth company ids found in PC adapters.
fn company_name(id: u16) -> String {
    match id {
        2 => "Intel".into(),
        10 => "Qualcomm (CSR)".into(),
        13 => "Texas Instruments".into(),
        15 => "Broadcom".into(),
        29 => "Qualcomm".into(),
        70 => "MediaTek".into(),
        72 => "Marvell".into(),
        93 => "Realtek".into(),
        other => format!("company id {other}"),
    }
}

unsafe fn device_property(set: HDEVINFO, device: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<Vec<u8>> {
    let mut property_type = DEVPROPTYPE(0);
    let mut buffer = vec![0u8; 512];
    let mut required = 0u32;
    SetupDiGetDevicePropertyW(set, device, key, &mut property_type, Some(&mut buffer), Some(&mut required), 0).ok()?;
    buffer.truncate(required as usize);
    Some(buffer)
}

unsafe fn device_string(set: HDEVINFO, device: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let bytes = device_property(set, device, key)?;
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = units.iter().position(|c| *c == 0).unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..end]))
}

/// A FILETIME property as yyyy-mm-dd.
unsafe fn device_date(set: HDEVINFO, device: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let bytes = device_property(set, device, key)?;
    let filetime = u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
    Some(filetime_to_date(filetime))
}

/// FILETIME (100 ns ticks since 1601-01-01) -> "yyyy-mm-dd".
pub fn filetime_to_date(filetime: u64) -> String {
    const TICKS_PER_DAY: u64 = 864_000_000_000;
    const DAYS_1601_TO_1970: i64 = 134_774;
    let days = (filetime / TICKS_PER_DAY) as i64 - DAYS_1601_TO_1970;

    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn registry_string(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buffer = [0u16; 128];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as _),
            Some(&mut size),
        )
    };
    result.is_ok().then(|| {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    })
}

fn registry_dword(key: &str, name: &str) -> Option<u32> {
    let (key, name) = (wide(key), wide(name));
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as _),
            Some(&mut size),
        )
    };
    result.is_ok().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_dates() {
        assert_eq!(filetime_to_date(0), "1601-01-01");
        assert_eq!(filetime_to_date(116_444_736_000_000_000), "1970-01-01");
        // 2021-06-15 and 2024-02-29 (leap day) at midnight UTC
        assert_eq!(filetime_to_date(132_681_888_000_000_000), "2021-06-15");
        assert_eq!(filetime_to_date(133_536_384_000_000_000), "2024-02-29");
    }

    #[test]
    fn version_names() {
        assert_eq!(bluetooth_version(8), "4.2");
        assert_eq!(bluetooth_version(12), "5.3");
        assert_eq!(bluetooth_version(200), "");
        assert_eq!(company_name(93), "Realtek");
        assert_eq!(company_name(2), "Intel");
        assert_eq!(company_name(9999), "company id 9999");
    }

    #[test]
    fn driver_year() {
        let info = AdapterInfo { driver_date: "2021-06-15".into(), ..Default::default() };
        assert_eq!(info.driver_year(), Some(2021));
        assert_eq!(AdapterInfo::default().driver_year(), None);
    }

    /// Smoke test against this PC: must not crash, whatever hardware is present.
    #[test]
    fn query_runs() {
        let info = query();
        if info.found() {
            assert!(info.driver_date.is_empty() || info.driver_date.len() == 10);
        }
        assert!(!windows_build().is_empty());
    }
}
