//! Developer tool: checks the "find the HID collections below a device node"
//! logic (used to locate the virtual joystick) against the real HID devices
//! of this PC. For every present HID interface it looks up the device's
//! parent node and verifies that walking down from that parent finds the
//! interface again.
//!
//!     cargo run --example hidtree

use ethos_bt_joystick::joystick::{all_hid_interfaces, hid_interfaces_below, VirtualJoystick};
use ethos_bt_joystick::util::wide;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Properties::{DEVPKEY_Device_InstanceId, DEVPROPTYPE};

fn main() {
    let interfaces = all_hid_interfaces();
    let mut found = 0;
    let mut virtual_style = 0;

    for path in &interfaces {
        let Some(parent) = parent_of_interface(path) else {
            println!("no parent: {path}");
            continue;
        };
        let below = hid_interfaces_below(parent);
        if below.iter().any(|p| p.eq_ignore_ascii_case(path)) {
            found += 1;
        } else {
            println!("NOT found via tree walk: {path}");
        }
        if !path.to_lowercase().contains("vid_") && !path.to_lowercase().contains("vid&") {
            virtual_style += 1;
        }
    }

    println!("{found} of {} HID interfaces found via the tree walk", interfaces.len());
    println!("{virtual_style} interface path(s) carry no VID/PID (virtual devices)");
    match VirtualJoystick::open() {
        Ok(_) => println!("virtual joystick: opened"),
        Err(error) => println!("virtual joystick: {error}"),
    }
}

fn parent_of_interface(path: &str) -> Option<u32> {
    unsafe {
        let wide_path = wide(path);
        let mut property_type = DEVPROPTYPE(0);
        let mut buffer = [0u8; 1024];
        let mut size = buffer.len() as u32;
        if CM_Get_Device_Interface_PropertyW(
            PCWSTR(wide_path.as_ptr()),
            &DEVPKEY_Device_InstanceId,
            &mut property_type,
            Some(buffer.as_mut_ptr()),
            &mut size,
            0,
        ) != CR_SUCCESS
        {
            return None;
        }

        let mut devinst = 0u32;
        if CM_Locate_DevNodeW(&mut devinst, PCWSTR(buffer.as_ptr() as *const u16), CM_LOCATE_DEVNODE_NORMAL)
            != CR_SUCCESS
        {
            return None;
        }
        let mut parent = 0u32;
        (CM_Get_Parent(&mut parent, devinst, 0) == CR_SUCCESS).then_some(parent)
    }
}
