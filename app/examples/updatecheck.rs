//! Asks GitHub for the newest published version, the way the program does.
//!
//!     cargo run --example updatecheck

use ethos_bt_joystick::update::{fetch_latest, Version, DOWNLOAD_PAGE};

fn main() {
    let current = Version::current();
    match fetch_latest() {
        Ok(latest) if latest > current => {
            println!("Version {latest} is available (this build is {current}): {DOWNLOAD_PAGE}")
        }
        Ok(latest) => println!("Up to date: the newest published version is {latest}, this build is {current}."),
        Err(error) => {
            eprintln!("The check failed: {error}");
            std::process::exit(1);
        }
    }
}
