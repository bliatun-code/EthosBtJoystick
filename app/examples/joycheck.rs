//! Developer tool: verifies the installed virtual joystick driver end to end.
//!
//! 1. Round trip: writes test patterns to the driver's control collection
//!    (exactly like the app does) and reads the joystick's input report back
//!    from the joystick collection (exactly what a simulator sees).
//! 2. Watchdog (driver 1.1 and later): writes a pattern, stays silent and
//!    checks that the driver returns to neutral by itself.
//!
//! Close the app first - its keep-alive would defeat the watchdog test.
//!
//!     cargo run --example joycheck
//!     cargo run --example joycheck -- --read     (only print the current state)

use std::time::{Duration, Instant};

use ethos_bt_joystick::joystick::{
    driver_status, format_driver_version, read_joystick_state, JoyState, VirtualJoystick, AXIS_MAX, DRIVER_VERSION,
};

fn main() {
    // `--read`: only print what the joystick reports right now
    if std::env::args().any(|arg| arg == "--read") {
        match read_joystick_state() {
            Ok(state) => {
                let neutral = if state == JoyState::NEUTRAL { "neutral" } else { "NOT neutral" };
                println!("{neutral}: axes {:?}  buttons {:#010x}", state.axes, state.buttons);
            }
            Err(error) => fail(&format!("cannot read the joystick: {error}")),
        }
        return;
    }

    println!("device node:    {:?}", driver_status());

    let before = match read_joystick_state() {
        Ok(state) => state,
        Err(error) => fail(&format!("cannot read the joystick: {error}")),
    };
    println!("current state:  axes {:?}  buttons {:#010x}", before.axes, before.buttons);

    let joystick = match VirtualJoystick::open() {
        Ok(joystick) => joystick,
        Err(error) => fail(&format!("cannot open the control collection: {error}")),
    };
    let version = joystick.driver_version();
    println!(
        "driver version: {} (this build embeds {})",
        format_driver_version(version),
        format_driver_version(DRIVER_VERSION)
    );

    let patterns = [
        JoyState { axes: [0, AXIS_MAX, 1000, 2000, 3000, 4000, 5000, 6000], buttons: 0x8000_0001 },
        JoyState { axes: [AXIS_MAX, 0, 31000, 21000, 11000, 1, 32766, 12345], buttons: 0x0F0F_A5A5 },
    ];

    let mut all_ok = true;
    for pattern in patterns {
        // If the app is forwarding radio data at the same moment it may
        // overwrite our pattern, so allow a few attempts.
        let ok = (0..5).any(|_| {
            if let Err(error) = joystick.write(&pattern) {
                fail(&format!("cannot write: {error}"));
            }
            matches!(read_joystick_state(), Ok(state) if state == pattern)
        });
        println!("round trip {:?} / {:#010x}: {}", pattern.axes, pattern.buttons, verdict(ok));
        all_ok &= ok;
    }

    if version >= 0x0110 {
        // Keep-alive: a pattern that is rewritten regularly must stay ...
        let pattern = patterns[0];
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(1800) {
            let _ = joystick.write(&pattern);
            std::thread::sleep(Duration::from_millis(250));
        }
        let kept = matches!(read_joystick_state(), Ok(state) if state == pattern);
        println!("kept alive for 1.8 s with a write every 250 ms: {}", verdict(kept));
        all_ok &= kept;

        // ... and one that is not must go back to neutral within about 1 s.
        let _ = joystick.write(&pattern);
        let start = Instant::now();
        let mut neutral_after = None;
        while start.elapsed() < Duration::from_millis(2500) {
            std::thread::sleep(Duration::from_millis(50));
            if matches!(read_joystick_state(), Ok(state) if state == JoyState::NEUTRAL) {
                neutral_after = Some(start.elapsed());
                break;
            }
        }
        match neutral_after {
            Some(elapsed) => println!("watchdog: neutral {} ms after the last write: OK", elapsed.as_millis()),
            None => println!("watchdog: still not neutral after 2.5 s: FAILED"),
        }
        all_ok &= neutral_after.is_some_and(|elapsed| elapsed >= Duration::from_millis(700));
    } else {
        println!("watchdog: not tested - driver {} has none (update the driver in the app)", format_driver_version(version));
    }

    let _ = joystick.write(&before);
    println!("state restored");
    println!("RESULT: {}", verdict(all_ok));
    std::process::exit(if all_ok { 0 } else { 1 });
}

fn verdict(ok: bool) -> &'static str {
    if ok {
        "OK"
    } else {
        "FAILED"
    }
}

fn fail(message: &str) -> ! {
    println!("FAILED: {message}");
    std::process::exit(1);
}
