//! Shared state between the Bluetooth worker, the joystick and the GUI.
//!
//! The Bluetooth notification callback calls [`Bridge::on_frame`], which maps
//! the channels and writes them straight to the virtual joystick. The GUI only
//! reads snapshots.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::adapter::AdapterInfo;
use crate::config::{Failsafe, Mapping};
use crate::diag::{self, DropReason, Event, EventKind, EventLog, FrameStats, LinkQuality, Second};
use crate::joystick::{JoyState, VirtualJoystick, KEEPALIVE_INTERVAL};
use crate::protocol::{Frame, MAX_CHANNELS};
use crate::util::local_time;

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

    // diagnostics
    /// Verdict on the link (`None` while not receiving or still measuring).
    pub quality: Option<LinkQuality>,
    /// Best single second of this connection (frames).
    pub best_rate: f32,
    /// Times per second that new data arrives. Bluetooth delivers frames in
    /// bursts, so this can be far below the frame rate.
    pub update_rate: f32,
    /// Average age of the newest data (what the verdict is based on), in ms.
    pub age_ms: f32,
    /// Longest pause between frames in the last few seconds.
    pub max_gap_ms: f32,
    /// Frames per second for the last half minute, oldest first.
    pub history: Vec<Second>,
    pub events: Vec<Event>,
    /// The link keeps dropping after a few seconds (adapter / driver trouble).
    pub quick_drops: bool,
    pub adapter: Option<AdapterInfo>,
    pub connection: Option<ConnectionInfo>,
}

/// What Windows and the radio agreed on for the current connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// Connection interval in units of 1.25 ms.
    pub interval_units: u16,
    pub latency: u16,
    /// Supervision timeout in units of 10 ms.
    pub timeout_units: u16,
    /// 1 = 1M, 2 = 2M, 3 = coded (long range), 0 = unknown.
    pub phy: u8,
}

impl ConnectionInfo {
    pub fn interval_ms(&self) -> f32 {
        self.interval_units as f32 * 1.25
    }

    pub fn phy_name(&self) -> &'static str {
        match self.phy {
            1 => "1M",
            2 => "2M",
            3 => "Coded",
            _ => "?",
        }
    }
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

    output: JoyState,
    /// When the joystick was last written (for the keep-alive).
    last_write: Instant,

    // diagnostics
    stats: FrameStats,
    events: EventLog,
    /// Frames received in the current connection.
    connection_frames: u64,
    /// When the current connection was established.
    connected_at: Option<Instant>,
    /// When the frames stopped (while in `SignalLost`).
    signal_lost_at: Option<Instant>,
    adapter: Option<AdapterInfo>,
    connection: Option<ConnectionInfo>,
    fast_connection: bool,
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
                output: JoyState::NEUTRAL,
                last_write: Instant::now(),
                stats: FrameStats::new(Instant::now()),
                events: EventLog::default(),
                connection_frames: 0,
                connected_at: None,
                signal_lost_at: None,
                adapter: None,
                connection: None,
                fast_connection: false,
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
        let mut inner = self.lock();
        let now = Instant::now();
        let receiving = inner.last_frame.is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
        let summary = inner.stats.summary(now);
        let live = matches!(inner.link, LinkState::Receiving | LinkState::SignalLost);
        Snapshot {
            quality: if live { summary.quality } else { None },
            best_rate: summary.best_rate,
            update_rate: if receiving { summary.update_rate } else { 0.0 },
            age_ms: summary.age_ms,
            max_gap_ms: summary.max_gap_ms,
            history: summary.history,
            events: inner.events.events().to_vec(),
            quick_drops: diag::repeated_quick_drops(inner.events.events(), now),
            adapter: inner.adapter.clone(),
            connection: inner.connection,
            frame_rate: if receiving { summary.rate } else { 0.0 },
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
        inner.connection_frames += 1;
        inner.stats.on_frame(now);

        let resumed = matches!(inner.link, LinkState::Connected | LinkState::SignalLost);
        if resumed {
            match (inner.link, inner.signal_lost_at.take()) {
                (LinkState::SignalLost, Some(lost)) => {
                    let gap_ms = now.saturating_duration_since(lost).as_millis() as u32;
                    inner.events.push(local_time(), now, EventKind::SignalBack { gap_ms });
                }
                _ => {
                    let after_ms =
                        inner.connected_at.map_or(0, |t| now.saturating_duration_since(t).as_millis() as u32);
                    let channels = frame.count as u8;
                    inner.events.push(local_time(), now, EventKind::FirstData { after_ms, channels });
                }
            }
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
            // the pause started when the last frame arrived
            let now = Instant::now();
            inner.signal_lost_at = inner.last_frame.or(Some(now));
            inner.events.push(local_time(), now, EventKind::SignalLost);
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
        inner.stats = FrameStats::new(Instant::now());
        inner.connection_frames = 0;
        inner.connected_at = None;
        inner.signal_lost_at = None;
        inner.connection = None;
    }

    // ---- diagnostics -----------------------------------------------------

    /// Adds an entry to the connection event log.
    pub fn log(&self, kind: EventKind) {
        self.lock().events.push(local_time(), Instant::now(), kind);
        self.notify();
    }

    pub fn clear_events(&self) {
        self.lock().events.clear();
    }

    /// The attempt succeeded: the link is up and subscribed.
    pub fn connection_established(&self, setup_ms: u32) {
        let mut inner = self.lock();
        let now = Instant::now();
        inner.connected_at = Some(now);
        inner.stats = FrameStats::new(now);
        inner.events.push(local_time(), now, EventKind::Connected { setup_ms });
    }

    /// The connection ended; logs how long it lasted.
    pub fn connection_ended(&self, reason: DropReason) {
        let mut inner = self.lock();
        let now = Instant::now();
        if let Some(connected_at) = inner.connected_at.take() {
            let kind = EventKind::Disconnected {
                reason,
                lasted: now.saturating_duration_since(connected_at),
                frames: inner.connection_frames,
            };
            inner.events.push(local_time(), now, kind);
        }
        inner.connection = None;
        inner.signal_lost_at = None;
    }

    pub fn set_adapter(&self, adapter: AdapterInfo) {
        self.lock().adapter = Some(adapter);
        self.notify();
    }

    /// Stores what Windows reports for the current connection; logs it when it
    /// is new or has changed.
    pub fn set_connection_info(&self, info: ConnectionInfo) {
        let mut inner = self.lock();
        let changed = inner.connection.map_or(true, |old| {
            (old.interval_units, old.latency, old.timeout_units) != (info.interval_units, info.latency, info.timeout_units)
        });
        inner.connection = Some(info);
        if changed {
            let kind = EventKind::Parameters {
                interval_units: info.interval_units,
                latency: info.latency,
                timeout_ms: info.timeout_units as u32 * 10,
            };
            inner.events.push(local_time(), Instant::now(), kind);
        }
        drop(inner);
        self.notify();
    }

    /// Ask Windows for the fastest connection interval when connecting.
    pub fn fast_connection(&self) -> bool {
        self.lock().fast_connection
    }

    pub fn set_fast_connection(&self, enabled: bool) {
        self.lock().fast_connection = enabled;
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
