//! Shared state between the Bluetooth worker, the joystick and the GUI.
//!
//! The Bluetooth notification callback calls [`Bridge::on_frame`], which maps
//! the channels and writes them straight to the virtual joystick. The GUI only
//! reads snapshots.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{Failsafe, Mapping};
use crate::joystick::{JoyState, VirtualJoystick, KEEPALIVE_INTERVAL};
use crate::protocol::{Frame, MAX_CHANNELS};

/// Called when something the GUI shows has changed (not for every frame).
type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkState {
    /// No radio selected.
    Idle,
    /// Waiting for the selected radio to show up.
    Searching,
    Connecting,
    /// Connected and subscribed, no trainer frame received yet.
    Connected,
    /// Trainer frames are arriving.
    Receiving,
    /// Connected, but the frames stopped.
    SignalLost,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkError {
    /// Windows does not know the device (not advertising / out of range).
    NotFound,
    /// The GATT services could not be read.
    Unreachable,
    /// The device has no FrSky trainer service (0xFFF0).
    NoTrainerService,
    /// The trainer service has no 0xFFF6 characteristic.
    NoCharacteristic,
    /// Notifications could not be enabled.
    SubscribeFailed,
    /// The Bluetooth link dropped.
    LinkLost,
    /// Connected, but no trainer data for several seconds.
    NoData,
    /// Any other Bluetooth error (message from Windows).
    Bluetooth(String),
}

#[derive(Clone, Debug)]
pub struct ScanEntry {
    pub address: u64,
    pub name: String,
    pub rssi: i16,
    /// The advertisement lists the FrSky trainer service.
    pub trainer_service: bool,
    pub last_seen: Instant,
}

impl ScanEntry {
    /// FrSky radios advertise the trainer service and/or a name that starts
    /// with the radio family (X20, X18, X14, XE, Twin ...).
    pub fn looks_like_radio(&self) -> bool {
        const PREFIXES: [&str; 6] = ["X20", "X18", "X14", "XE", "TWIN", "FRSKY"];
        let name = self.name.to_uppercase();
        self.trainer_service || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
    }
}

/// Everything the GUI needs to draw one frame.
#[derive(Clone)]
pub struct Snapshot {
    pub link: LinkState,
    pub link_error: Option<LinkError>,
    pub device_name: String,
    pub scanning: bool,
    pub scan_error: Option<String>,
    pub devices: Vec<ScanEntry>,
    pub channels: [u16; MAX_CHANNELS],
    pub channel_count: usize,
    pub frames_total: u64,
    pub bad_frames: u64,
    pub frame_rate: f32,
    pub output: JoyState,
    pub joystick_open: bool,
    /// Version of the installed driver (when the joystick is open).
    pub driver_version: Option<u16>,
}

struct Inner {
    mapping: Mapping,

    joystick: Option<VirtualJoystick>,
    joystick_suspended: bool,

    link: LinkState,
    link_error: Option<LinkError>,
    device_name: String,

    scanning: bool,
    scan_error: Option<String>,
    devices: BTreeMap<u64, ScanEntry>,

    channels: [u16; MAX_CHANNELS],
    channel_count: usize,
    last_frame: Option<Instant>,
    frames_total: u64,
    bad_frames: u64,
    frame_rate: f32,
    rate_start: Instant,
    rate_count: u32,

    output: JoyState,
    /// When the joystick was last written (for the keep-alive).
    last_write: Instant,
}

impl Inner {
    fn write_output(&mut self, state: JoyState) {
        self.output = state;
        self.last_write = Instant::now();
        if let Some(joystick) = &self.joystick {
            if joystick.write(&state).is_err() {
                // device went away (driver removed / updated); reopened later
                self.joystick = None;
            }
        }
    }
}

pub struct Bridge {
    inner: Mutex<Inner>,
    wake: Mutex<Option<Wake>>,
}

impl Bridge {
    pub fn new(mapping: Mapping) -> Self {
        Self {
            wake: Mutex::new(None),
            inner: Mutex::new(Inner {
                mapping,
                joystick: None,
                joystick_suspended: false,
                link: LinkState::Idle,
                link_error: None,
                device_name: String::new(),
                scanning: false,
                scan_error: None,
                devices: BTreeMap::new(),
                channels: [0; MAX_CHANNELS],
                channel_count: 0,
                last_frame: None,
                frames_total: 0,
                bad_frames: 0,
                frame_rate: 0.0,
                rate_start: Instant::now(),
                rate_count: 0,
                output: JoyState::NEUTRAL,
                last_write: Instant::now(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers the callback that asks the GUI to redraw.
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(wake));
    }

    /// Tells the GUI that the status changed. Never call this while holding
    /// the state lock.
    fn notify(&self) {
        let wake = self.wake.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Starts the background thread that looks after the virtual joystick: it
    /// opens the device when the driver appears, and sends the keep-alive that
    /// stops the driver's watchdog from centring the joystick while we run.
    pub fn start_housekeeping(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new().name("joystick".into()).spawn(move || {
            let mut tick = 0u32;
            loop {
                let Some(bridge) = weak.upgrade() else { break };
                bridge.keepalive();
                if tick % 10 == 0 {
                    bridge.ensure_joystick();
                }
                drop(bridge);
                tick = tick.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }

    /// Rewrites the current state if nothing was written for a while.
    pub fn keepalive(&self) {
        let mut inner = self.lock();
        if inner.joystick.is_some() && inner.last_write.elapsed() >= KEEPALIVE_INTERVAL {
            let state = inner.output;
            inner.write_output(state);
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.lock();
        let receiving = inner.last_frame.is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
        Snapshot {
            link: inner.link,
            link_error: inner.link_error.clone(),
            device_name: inner.device_name.clone(),
            scanning: inner.scanning,
            scan_error: inner.scan_error.clone(),
            devices: inner.devices.values().cloned().collect(),
            channels: inner.channels,
            channel_count: inner.channel_count,
            frames_total: inner.frames_total,
            bad_frames: inner.bad_frames,
            frame_rate: if receiving { inner.frame_rate } else { 0.0 },
            output: inner.output,
            joystick_open: inner.joystick.is_some(),
            driver_version: inner.joystick.as_ref().map(|joystick| joystick.driver_version()),
        }
    }

    // ---- data path -------------------------------------------------------

    /// A valid trainer frame arrived.
    pub fn on_frame(&self, frame: &Frame) {
        let mut inner = self.lock();
        let now = Instant::now();

        inner.channels = frame.channels;
        inner.channel_count = frame.count;
        inner.last_frame = Some(now);
        inner.frames_total += 1;

        inner.rate_count += 1;
        let elapsed = now.duration_since(inner.rate_start);
        if elapsed >= Duration::from_secs(1) {
            inner.frame_rate = inner.rate_count as f32 / elapsed.as_secs_f32();
            inner.rate_start = now;
            inner.rate_count = 0;
        }

        let resumed = matches!(inner.link, LinkState::Connected | LinkState::SignalLost);
        if resumed {
            inner.link = LinkState::Receiving;
            inner.link_error = None;
        }

        let state = inner.mapping.apply(frame.channels());
        inner.write_output(state);
        drop(inner);

        if resumed {
            self.notify();
        }
    }

    pub fn add_bad_frames(&self, count: u64) {
        self.lock().bad_frames += count;
    }

    /// Time since the last frame of the current connection.
    pub fn frame_age(&self) -> Option<Duration> {
        self.lock().last_frame.map(|t| t.elapsed())
    }

    /// The frames stopped: apply the failsafe.
    pub fn signal_lost(&self) {
        let mut inner = self.lock();
        let changed = inner.link == LinkState::Receiving;
        if changed {
            inner.link = LinkState::SignalLost;
        }
        if inner.mapping.failsafe == Failsafe::Center && inner.output != JoyState::NEUTRAL {
            inner.write_output(JoyState::NEUTRAL);
        }
        drop(inner);

        if changed {
            self.notify();
        }
    }

    /// Centre everything (disconnect / exit).
    pub fn neutralize(&self) {
        self.lock().write_output(JoyState::NEUTRAL);
    }

    pub fn set_mapping(&self, mapping: Mapping) {
        let mut inner = self.lock();
        inner.mapping = mapping;
        if inner.link == LinkState::Receiving {
            let count = inner.channel_count;
            let channels = inner.channels;
            let state = inner.mapping.apply(&channels[..count]);
            inner.write_output(state);
        }
    }

    // ---- link state ------------------------------------------------------

    pub fn set_link(&self, link: LinkState, error: Option<LinkError>) {
        let mut inner = self.lock();
        inner.link = link;
        inner.link_error = error;
        if matches!(link, LinkState::Idle | LinkState::Searching) {
            inner.channel_count = 0;
        }
        drop(inner);
        self.notify();
    }

    /// A new connection attempt starts: forget the previous frame statistics.
    pub fn begin_connection(&self, name: &str) {
        let mut inner = self.lock();
        inner.device_name = name.to_string();
        inner.last_frame = None;
        inner.frame_rate = 0.0;
        inner.rate_start = Instant::now();
        inner.rate_count = 0;
    }

    // ---- joystick --------------------------------------------------------

    /// Opens the virtual joystick if it is not open yet (called periodically).
    pub fn ensure_joystick(&self) {
        {
            let inner = self.lock();
            if inner.joystick.is_some() || inner.joystick_suspended {
                return;
            }
        }
        // enumerate devices without holding the lock
        if let Ok(joystick) = VirtualJoystick::open() {
            let mut inner = self.lock();
            if inner.joystick.is_none() && !inner.joystick_suspended {
                let _ = joystick.write(&inner.output);
                inner.last_write = Instant::now();
                inner.joystick = Some(joystick);
                drop(inner);
                self.notify();
            }
        }
    }

    /// Closes the joystick handle while the driver is being (un)installed, so
    /// that Windows can remove the device without a restart.
    pub fn suspend_joystick(&self, suspended: bool) {
        let mut inner = self.lock();
        inner.joystick_suspended = suspended;
        if suspended {
            inner.joystick = None;
        }
    }

    // ---- scanning --------------------------------------------------------

    pub fn set_scanning(&self, scanning: bool, error: Option<String>) {
        let mut inner = self.lock();
        let changed = inner.scanning != scanning || inner.scan_error != error;
        inner.scanning = scanning;
        inner.scan_error = error;
        drop(inner);

        if changed {
            self.notify();
        }
    }

    pub fn clear_devices(&self) {
        self.lock().devices.clear();
    }

    pub fn report_device(&self, address: u64, name: &str, rssi: i16, trainer_service: bool) {
        let mut inner = self.lock();
        let now = Instant::now();
        let entry = inner.devices.entry(address).or_insert_with(|| ScanEntry {
            address,
            name: String::new(),
            rssi,
            trainer_service: false,
            last_seen: now,
        });
        // the name / service list may only be in the scan response, so never
        // overwrite known information with "nothing"
        if !name.is_empty() {
            entry.name = name.to_string();
        }
        entry.trainer_service |= trainer_service;
        entry.rssi = rssi;
        entry.last_seen = now;
    }

    /// Forget devices that have not been seen for a while.
    pub fn prune_devices(&self, max_age: Duration) {
        self.lock().devices.retain(|_, entry| entry.last_seen.elapsed() <= max_age);
    }
}
