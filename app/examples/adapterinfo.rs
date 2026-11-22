//! Developer tool: prints the diagnostics report as the app would copy it,
//! without a radio connected - i.e. what it knows about this PC's Bluetooth
//! adapter and the virtual joystick.
//!
//!     cargo run --example adapterinfo

use ethos_bt_joystick::adapter;
use ethos_bt_joystick::bridge::Bridge;
use ethos_bt_joystick::config::Mapping;
use ethos_bt_joystick::joystick::driver_status;
use ethos_bt_joystick::ui_diag;

fn main() {
    let bridge = Bridge::new(Mapping::default());
    bridge.set_adapter(adapter::query());
    bridge.ensure_joystick();
    print!("{}", ui_diag::report(&bridge.snapshot(), driver_status(), false));
}
