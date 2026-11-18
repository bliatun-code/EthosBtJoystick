//! Developer tool: verifies the installed virtual joystick driver end to end.
//!
//! It writes a test pattern to the driver's control collection (exactly like
//! the app does) and reads the joystick's input report back from the joystick
//! collection (exactly what a simulator sees). The previous state is restored
//! afterwards.
//!
//!     cargo run --example joycheck

use ethos_bt_joystick::joystick::{driver_status, read_joystick_state, JoyState, VirtualJoystick, AXIS_MAX};

fn main() {
    println!("driver status: {:?}", driver_status());

    let before = match read_joystick_state() {
        Ok(state) => state,
        Err(error) => {
            println!("FAILED to read the joystick: {error}");
            std::process::exit(1);
        }
    };
    println!("current state:  axes {:?}  buttons {:#010x}", before.axes, before.buttons);

    let joystick = match VirtualJoystick::open() {
        Ok(joystick) => joystick,
        Err(error) => {
            println!("FAILED to open the control collection: {error}");
            std::process::exit(1);
        }
    };

    let patterns = [
        JoyState { axes: [0, AXIS_MAX, 1000, 2000, 3000, 4000, 5000, 6000], buttons: 0x8000_0001 },
        JoyState { axes: [AXIS_MAX, 0, 31000, 21000, 11000, 1, 32766, 12345], buttons: 0x0F0F_A5A5 },
    ];

    let mut all_ok = true;
    for pattern in patterns {
        // If the app is forwarding radio data at the same moment it may
        // overwrite our pattern, so allow a few attempts.
        let mut ok = false;
        for _ in 0..5 {
            if let Err(error) = joystick.write(&pattern) {
                println!("FAILED to write: {error}");
                std::process::exit(1);
            }
            match read_joystick_state() {
                Ok(state) if state == pattern => {
                    ok = true;
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    println!("FAILED to read back: {error}");
                    std::process::exit(1);
                }
            }
        }
        println!("round trip {:?} / {:#010x}: {}", pattern.axes, pattern.buttons, if ok { "OK" } else { "MISMATCH" });
        all_ok &= ok;
    }

    let _ = joystick.write(&before);
    println!("state restored");
    println!("{}", if all_ok { "RESULT: OK" } else { "RESULT: FAILED" });
    std::process::exit(if all_ok { 0 } else { 1 });
}
