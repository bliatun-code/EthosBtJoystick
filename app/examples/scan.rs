//! Developer tool: scans for Bluetooth LE devices for a few seconds using the
//! same worker as the GUI and prints what it found.
//!
//!     cargo run --example scan [seconds]

use std::sync::Arc;
use std::time::Duration;

use ethos_bt_joystick::ble::{self, Command};
use ethos_bt_joystick::bridge::Bridge;
use ethos_bt_joystick::config::Mapping;
use ethos_bt_joystick::joystick::driver_status;
use ethos_bt_joystick::util::format_address;

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(6);

    println!("Virtual joystick driver: {:?}", driver_status());

    let bridge = Arc::new(Bridge::new(Mapping::default()));
    let ble = ble::spawn(bridge.clone());
    ble.send(Command::StartScan);
    println!("Scanning for {seconds} s ...");
    std::thread::sleep(Duration::from_secs(seconds));

    let snapshot = bridge.snapshot();
    if let Some(error) = &snapshot.scan_error {
        println!("Scan error: {error}");
    }
    println!("scanning = {}, {} device(s):", snapshot.scanning, snapshot.devices.len());
    for device in &snapshot.devices {
        println!(
            "  {}  {:>4} dBm  {}  {}",
            format_address(device.address),
            device.rssi,
            if device.trainer_service { "FFF0" } else { "    " },
            if device.name.is_empty() { "(no name)" } else { &device.name }
        );
    }

    // Exercise the connect path with an address that does not exist: must end
    // in "Searching" with a NotFound error, without touching any real device.
    ble.send(Command::Connect { address: 0x0000_1122_3344_5566, name: "nonexistent".into() });
    std::thread::sleep(Duration::from_secs(3));
    let snapshot = bridge.snapshot();
    println!("connect to a nonexistent address -> {:?}, error = {:?}", snapshot.link, snapshot.link_error);
    ble.send(Command::Disconnect);
    std::thread::sleep(Duration::from_millis(300));
    println!("after disconnect -> {:?}", bridge.snapshot().link);

    ble.shutdown(Duration::from_secs(3));
}
