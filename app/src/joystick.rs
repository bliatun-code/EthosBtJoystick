//! Client side of the virtual joystick driver (`driver/ethosjoy.c`).
//!
//! The driver exposes a vendor defined HID collection next to the joystick.
//! We open that collection and write output report 2 (16 axis bytes + 4 button
//! bytes); the driver turns it into the joystick's input report.

use std::mem::size_of;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

use crate::util::wide;

// Keep in sync with driver/ethosjoy.c
pub const VENDOR_ID: u16 = 0x1209;
pub const PRODUCT_ID: u16 = 0xE7B0;
pub const HARDWARE_ID: &str = "root\\EthosBtJoystick";
pub const AXIS_COUNT: usize = 8;
pub const BUTTON_COUNT: usize = 32;
pub const AXIS_MAX: u16 = 0x7FFF;
pub const AXIS_CENTER: u16 = 0x4000;

const REPORT_ID_CONTROL: u8 = 0x02;
const REPORT_SIZE: usize = 1 + AXIS_COUNT * 2 + BUTTON_COUNT / 8;
const CONTROL_USAGE_PAGE: u16 = 0xFF00;

/// Names of the HID axes, in report order.
pub const AXIS_NAMES: [&str; AXIS_COUNT] = ["X", "Y", "Z", "Rx", "Ry", "Rz", "Slider", "Dial"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoyState {
    /// 0..=AXIS_MAX, centre = AXIS_CENTER
    pub axes: [u16; AXIS_COUNT],
    /// bit 0 = button 1
    pub buttons: u32,
}

impl JoyState {
    pub const NEUTRAL: JoyState = JoyState { axes: [AXIS_CENTER; AXIS_COUNT], buttons: 0 };

    fn to_report(self) -> [u8; REPORT_SIZE] {
        let mut report = [0u8; REPORT_SIZE];
        report[0] = REPORT_ID_CONTROL;
        for (i, axis) in self.axes.iter().enumerate() {
            let value = (*axis).min(AXIS_MAX).to_le_bytes();
            report[1 + i * 2] = value[0];
            report[2 + i * 2] = value[1];
        }
        report[1 + AXIS_COUNT * 2..].copy_from_slice(&self.buttons.to_le_bytes());
        report
    }
}

/// An open handle to the driver's control collection.
pub struct VirtualJoystick {
    handle: HANDLE,
}

// The handle is only used for WriteFile, which is safe from any thread.
unsafe impl Send for VirtualJoystick {}

impl VirtualJoystick {
    /// Finds and opens the control collection. Fails if the driver is not
    /// installed (or not started).
    pub fn open() -> Result<Self, String> {
        // The HID collections are the children of our root-enumerated device
        // node. (Their paths do not contain the VID/PID - for virtual devices
        // hidclass derives them from the parent's id, e.g. "HID\HIDCLASS&COL02".)
        let mut candidates = Vec::new();
        unsafe {
            for_each_device(|_, data| candidates.extend(hid_interfaces_below(data.DevInst)));
        }
        if candidates.is_empty() {
            return Err(String::from("the virtual joystick device was not found"));
        }

        let mut last_error = String::from("the control collection of the virtual joystick was not found");
        for path in candidates {
            let wide_path = wide(&path);
            let handle = unsafe {
                CreateFileW(
                    PCWSTR(wide_path.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    HANDLE::default(),
                )
            };
            let handle = match handle {
                Ok(handle) => handle,
                Err(error) => {
                    last_error = format!("could not open {path}: {error}");
                    continue;
                }
            };
            if unsafe { is_control_collection(handle) } {
                return Ok(Self { handle });
            }
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
        Err(last_error)
    }

    pub fn write(&self, state: &JoyState) -> Result<(), String> {
        let report = state.to_report();
        let mut written = 0u32;
        unsafe { WriteFile(self.handle, Some(&report), Some(&mut written), None) }
            .map_err(|error| error.to_string())
    }
}

impl Drop for VirtualJoystick {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Reads the joystick's current input report straight from the joystick
/// collection (the one simulators read) - i.e. what the driver hands to Windows.
/// Used for diagnostics.
pub fn read_joystick_state() -> Result<JoyState, String> {
    let mut candidates = Vec::new();
    unsafe {
        for_each_device(|_, data| candidates.extend(hid_interfaces_below(data.DevInst)));
    }

    for path in candidates {
        let wide_path = wide(&path);
        let Ok(handle) = (unsafe {
            CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                HANDLE::default(),
            )
        }) else {
            continue;
        };

        let result = unsafe { read_input_report(handle) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        if let Some(result) = result {
            return result;
        }
    }
    Err(String::from("the joystick collection was not found"))
}

/// `None` if `handle` is not the joystick collection.
unsafe fn read_input_report(handle: HANDLE) -> Option<Result<JoyState, String>> {
    const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
    const USAGE_JOYSTICK: u16 = 0x04;
    const REPORT_ID_JOYSTICK: u8 = 0x01;

    let mut preparsed = PHIDP_PREPARSED_DATA::default();
    if !HidD_GetPreparsedData(handle, &mut preparsed).as_bool() {
        return None;
    }
    let mut caps = HIDP_CAPS::default();
    let status = HidP_GetCaps(preparsed, &mut caps);
    let _ = HidD_FreePreparsedData(preparsed);
    if status != HIDP_STATUS_SUCCESS || caps.UsagePage != USAGE_PAGE_GENERIC_DESKTOP || caps.Usage != USAGE_JOYSTICK {
        return None;
    }
    if caps.InputReportByteLength as usize != REPORT_SIZE {
        return Some(Err(format!("unexpected input report length {}", caps.InputReportByteLength)));
    }

    let mut report = [0u8; REPORT_SIZE];
    report[0] = REPORT_ID_JOYSTICK;
    if !HidD_GetInputReport(handle, report.as_mut_ptr() as _, report.len() as u32).as_bool() {
        return Some(Err(format!("HidD_GetInputReport failed: {}", windows::core::Error::from_win32())));
    }

    let mut state = JoyState::NEUTRAL;
    for (i, axis) in state.axes.iter_mut().enumerate() {
        *axis = u16::from_le_bytes([report[1 + i * 2], report[2 + i * 2]]);
    }
    let b = &report[1 + AXIS_COUNT * 2..];
    state.buttons = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    Some(Ok(state))
}

unsafe fn is_control_collection(handle: HANDLE) -> bool {
    let mut attributes = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    if !HidD_GetAttributes(handle, &mut attributes).as_bool()
        || attributes.VendorID != VENDOR_ID
        || attributes.ProductID != PRODUCT_ID
    {
        return false;
    }

    let mut preparsed = PHIDP_PREPARSED_DATA::default();
    if !HidD_GetPreparsedData(handle, &mut preparsed).as_bool() {
        return false;
    }
    let mut caps = HIDP_CAPS::default();
    let status = HidP_GetCaps(preparsed, &mut caps);
    let _ = HidD_FreePreparsedData(preparsed);
    status == HIDP_STATUS_SUCCESS
        && caps.UsagePage == CONTROL_USAGE_PAGE
        && caps.OutputReportByteLength as usize == REPORT_SIZE
}

/// HID device interface paths of the direct children of device node `devinst`.
pub fn hid_interfaces_below(devinst: u32) -> Vec<String> {
    let mut paths = Vec::new();
    unsafe {
        let guid = HidD_GetHidGuid();
        let mut child = 0u32;
        if CM_Get_Child(&mut child, devinst, 0) != CR_SUCCESS {
            return paths;
        }
        loop {
            let mut id = [0u16; 256]; // MAX_DEVICE_ID_LEN is 200
            if CM_Get_Device_IDW(child, &mut id, 0) == CR_SUCCESS {
                paths.extend(device_interfaces(&guid, PCWSTR(id.as_ptr())));
            }
            let mut next = 0u32;
            if CM_Get_Sibling(&mut next, child, 0) != CR_SUCCESS {
                break;
            }
            child = next;
        }
    }
    paths
}

/// Device interface paths of all present HID collections (developer tools only).
pub fn all_hid_interfaces() -> Vec<String> {
    unsafe { device_interfaces(&HidD_GetHidGuid(), PCWSTR::null()) }
}

/// Present interfaces of class `guid`, optionally limited to one device instance.
unsafe fn device_interfaces(guid: &GUID, device_id: PCWSTR) -> Vec<String> {
    let mut length = 0u32;
    if CM_Get_Device_Interface_List_SizeW(&mut length, guid, device_id, CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
        != CR_SUCCESS
        || length <= 1
    {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize];
    if CM_Get_Device_Interface_ListW(guid, device_id, &mut buffer, CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
        != CR_SUCCESS
    {
        return Vec::new();
    }
    // REG_MULTI_SZ style list
    buffer.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

/// State of the driver's device node, as far as an unelevated process can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverStatus {
    /// No device with our hardware id exists.
    NotInstalled,
    /// The device exists but is not running; Device Manager problem code.
    Problem(u32),
    /// The device is started.
    Running,
}

pub fn driver_status() -> DriverStatus {
    let mut result = DriverStatus::NotInstalled;
    unsafe {
        for_each_device(|set, data| {
            let mut status = CM_DEVNODE_STATUS_FLAGS(0);
            let mut problem = CM_PROB(0);
            let cr = CM_Get_DevNode_Status(&mut status, &mut problem, data.DevInst, 0);
            if cr == CR_SUCCESS && (status.0 & DN_STARTED.0) != 0 {
                result = DriverStatus::Running;
            } else if result == DriverStatus::NotInstalled {
                result = DriverStatus::Problem(problem.0);
            }
            let _ = set;
        });
    }
    result
}

/// Calls `callback` for every device node (present or not) whose hardware id
/// is `HARDWARE_ID`.
///
/// # Safety
/// The callback receives raw SetupAPI handles that are only valid during the call.
pub unsafe fn for_each_device(mut callback: impl FnMut(HDEVINFO, &SP_DEVINFO_DATA)) {
    let enumerator = wide("ROOT");
    let Ok(set) = SetupDiGetClassDevsW(None, PCWSTR(enumerator.as_ptr()), HWND::default(), DIGCF_ALLCLASSES)
    else {
        return;
    };

    for index in 0.. {
        let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        if SetupDiEnumDeviceInfo(set, index, &mut data).is_err() {
            break;
        }

        let mut buffer = [0u8; 1024];
        let mut required = 0u32;
        if SetupDiGetDeviceRegistryPropertyW(
            set,
            &data,
            SPDRP_HARDWAREID,
            None,
            Some(&mut buffer),
            Some(&mut required),
        )
        .is_err()
        {
            continue;
        }

        // REG_MULTI_SZ: compare every string
        let len = (required as usize).min(buffer.len()) / 2;
        let chars: Vec<u16> = (0..len).map(|i| u16::from_le_bytes([buffer[i * 2], buffer[i * 2 + 1]])).collect();
        let is_ours = chars
            .split(|c| *c == 0)
            .any(|id| String::from_utf16_lossy(id).eq_ignore_ascii_case(HARDWARE_ID));
        if is_ours {
            callback(set, &data);
        }
    }

    let _ = SetupDiDestroyDeviceInfoList(set);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_layout() {
        let mut state = JoyState::NEUTRAL;
        state.axes[0] = 0x1234;
        state.axes[7] = 0x7FFF;
        state.buttons = 0x8000_0001;
        let report = state.to_report();
        assert_eq!(report.len(), 21);
        assert_eq!(report[0], 2);
        assert_eq!(&report[1..3], &[0x34, 0x12]);
        assert_eq!(&report[3..5], &[0x00, 0x40]);
        assert_eq!(&report[15..17], &[0xFF, 0x7F]);
        assert_eq!(&report[17..21], &[0x01, 0x00, 0x00, 0x80]);
    }
}
