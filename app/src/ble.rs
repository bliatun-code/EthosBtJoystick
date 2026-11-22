//! Bluetooth LE worker: scans for radios, connects to the FrSky trainer
//! service and feeds the notifications into the frame decoder.
//!
//! Everything runs on one background thread that owns the WinRT objects. The
//! GUI talks to it through [`BleHandle`]; results are published in the
//! [`Bridge`]. Windows does not need to be paired with the radio.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{IInspectable, GUID};
use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher, BluetoothLEScanningMode,
};
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristic, GattClientCharacteristicConfigurationDescriptorValue, GattCommunicationStatus,
    GattDeviceService, GattSession, GattValueChangedEventArgs,
};
use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice};
use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::bridge::{Bridge, LinkError, LinkState};
use crate::protocol::FrameDecoder;

/// FrSky trainer service / characteristic (16 bit UUIDs 0xFFF0 / 0xFFF6).
const TRAINER_SERVICE: GUID = GUID::from_u128(0x0000fff0_0000_1000_8000_00805f9b34fb);
const TRAINER_CHARACTERISTIC: GUID = GUID::from_u128(0x0000fff6_0000_1000_8000_00805f9b34fb);

/// No frame for this long: apply the failsafe.
const FAILSAFE_AFTER: Duration = Duration::from_millis(500);
/// No frame for this long: drop the link and connect again.
const RECONNECT_AFTER: Duration = Duration::from_secs(5);
/// Pause between connection attempts when the radio is not advertising.
const RETRY_INTERVAL: Duration = Duration::from_secs(6);
/// Minimum pause between attempts triggered by an advertisement.
const MIN_ATTEMPT_GAP: Duration = Duration::from_secs(2);
/// Scan results older than this are removed from the list.
const SCAN_ENTRY_LIFETIME: Duration = Duration::from_secs(20);

pub enum Command {
    StartScan,
    StopScan,
    Connect { address: u64, name: String },
    Disconnect,
    Shutdown,
}

/// Handle used by the GUI to control the worker thread.
pub struct BleHandle {
    tx: Sender<Command>,
    done: Receiver<()>,
}

impl BleHandle {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// Stops the worker and waits (at most `timeout`) until it has released
    /// the Bluetooth link.
    pub fn shutdown(&self, timeout: Duration) {
        let _ = self.tx.send(Command::Shutdown);
        let _ = self.done.recv_timeout(timeout);
    }
}

pub fn spawn(bridge: Arc<Bridge>) -> BleHandle {
    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("bluetooth".into())
        .spawn(move || {
            Worker::new(bridge).run(rx);
            let _ = done_tx.send(());
        })
        .expect("failed to start the Bluetooth thread");
    BleHandle { tx, done: done_rx }
}

struct Target {
    address: u64,
    name: String,
}

struct Watcher {
    watcher: BluetoothLEAdvertisementWatcher,
    token: EventRegistrationToken,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.watcher.Stop();
        let _ = self.watcher.RemoveReceived(self.token);
    }
}

struct Connection {
    device: BluetoothLEDevice,
    service: GattDeviceService,
    characteristic: GattCharacteristic,
    value_token: EventRegistrationToken,
    status_token: EventRegistrationToken,
    _session: Option<GattSession>,
    /// Set by the connection status event when the link drops.
    dropped: Arc<AtomicBool>,
    since: Instant,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.characteristic.RemoveValueChanged(self.value_token);
        let _ = self.device.RemoveConnectionStatusChanged(self.status_token);
        // closing the objects makes Windows release the Bluetooth link
        let _ = self.service.Close();
        let _ = self.device.Close();
    }
}

struct Worker {
    bridge: Arc<Bridge>,
    watcher: Option<Watcher>,
    user_scan: bool,
    target: Option<Target>,
    /// Address of `target` (0 = none), read by the advertisement callback.
    target_address: Arc<AtomicU64>,
    /// The target was seen advertising since the last attempt.
    target_seen: Arc<AtomicBool>,
    connection: Option<Connection>,
    last_attempt: Option<Instant>,
    next_attempt: Instant,
    last_housekeeping: Instant,
    /// Failed attempts in a row (used to alternate the GATT cache mode).
    failures: u32,
}

impl Worker {
    fn new(bridge: Arc<Bridge>) -> Self {
        Self {
            bridge,
            watcher: None,
            user_scan: false,
            target: None,
            target_address: Arc::new(AtomicU64::new(0)),
            target_seen: Arc::new(AtomicBool::new(false)),
            connection: None,
            last_attempt: None,
            next_attempt: Instant::now(),
            last_housekeeping: Instant::now(),
            failures: 0,
        }
    }

    fn run(mut self, rx: Receiver<Command>) {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }

        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
        }

        self.connection = None;
        self.watcher = None;
        self.bridge.neutralize();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::StartScan => {
                self.bridge.clear_devices();
                self.user_scan = true;
            }
            Command::StopScan => self.user_scan = false,
            Command::Connect { address, name } => {
                self.connection = None;
                self.target_address.store(address, Ordering::Relaxed);
                self.target_seen.store(false, Ordering::Relaxed);
                self.target = Some(Target { address, name });
                self.user_scan = false;
                self.last_attempt = None;
                self.failures = 0;
                self.next_attempt = Instant::now();
                self.bridge.neutralize();
            }
            Command::Disconnect => {
                self.connection = None;
                self.target = None;
                self.target_address.store(0, Ordering::Relaxed);
                self.bridge.set_link(LinkState::Idle, None);
                self.bridge.neutralize();
            }
            Command::Shutdown => {}
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();

        // 1. Is the current connection still alive?
        if let Some(connection) = &self.connection {
            let dropped = connection.dropped.load(Ordering::Relaxed);
            let silent = self.bridge.frame_age().unwrap_or_else(|| connection.since.elapsed());

            if dropped || silent > RECONNECT_AFTER {
                self.connection = None;
                self.bridge.signal_lost();
                let error = if dropped { LinkError::LinkLost } else { LinkError::NoData };
                self.bridge.set_link(LinkState::Searching, Some(error));
                self.target_seen.store(false, Ordering::Relaxed);
                self.next_attempt = now + Duration::from_millis(500);
            } else if silent > FAILSAFE_AFTER {
                self.bridge.signal_lost();
            }
        }

        // 2. The advertisement watcher runs while the user scans, and while we
        //    wait for the selected radio to (re)appear.
        let want_watcher = self.user_scan || (self.target.is_some() && self.connection.is_none());
        if want_watcher && self.watcher.is_none() {
            match self.start_watcher() {
                Ok(watcher) => {
                    self.watcher = Some(watcher);
                    self.bridge.set_scanning(self.user_scan, None);
                }
                Err(error) => {
                    self.user_scan = false;
                    self.bridge.set_scanning(false, Some(error.message().to_string()));
                }
            }
        } else if !want_watcher && self.watcher.is_some() {
            self.watcher = None;
            self.bridge.set_scanning(false, None);
        } else if self.watcher.is_some() {
            self.bridge.set_scanning(self.user_scan, None);
        }

        // 3. Connect to the selected radio.
        if self.connection.is_none() {
            if let Some(target) = &self.target {
                let seen = self.target_seen.load(Ordering::Relaxed);
                let gap_ok = self.last_attempt.map_or(true, |t| now.duration_since(t) >= MIN_ATTEMPT_GAP);
                if (seen && gap_ok) || now >= self.next_attempt {
                    self.target_seen.store(false, Ordering::Relaxed);
                    self.bridge.begin_connection(&target.name);
                    self.bridge.set_link(LinkState::Connecting, None);

                    // Reading the services fresh is the reliable default, but Windows
                    // sometimes only answers from its cache - so alternate on failure.
                    let cache_mode = if self.failures % 2 == 0 {
                        BluetoothCacheMode::Uncached
                    } else {
                        BluetoothCacheMode::Cached
                    };
                    match connect(&self.bridge, target.address, cache_mode) {
                        Ok(connection) => {
                            self.connection = Some(connection);
                            self.failures = 0;
                            self.bridge.set_link(LinkState::Connected, None);
                        }
                        Err(error) => {
                            self.failures += 1;
                            self.bridge.set_link(LinkState::Searching, Some(error));
                        }
                    }
                    let done = Instant::now();
                    self.last_attempt = Some(done);
                    self.next_attempt = done + RETRY_INTERVAL;
                }
            }
        }

        // 4. Housekeeping once a second. (The virtual joystick is looked after
        //    by the bridge's own thread, so a slow connection attempt here
        //    cannot starve its keep-alive.)
        if now.duration_since(self.last_housekeeping) >= Duration::from_secs(1) {
            self.last_housekeeping = now;
            if self.user_scan {
                self.bridge.prune_devices(SCAN_ENTRY_LIFETIME);
            }
        }
    }

    fn start_watcher(&self) -> windows::core::Result<Watcher> {
        let watcher = BluetoothLEAdvertisementWatcher::new()?;
        // active scanning also requests the scan response, which usually
        // carries the device name
        watcher.SetScanningMode(BluetoothLEScanningMode::Active)?;

        let bridge = self.bridge.clone();
        let target_address = self.target_address.clone();
        let target_seen = self.target_seen.clone();

        let token = watcher.Received(&TypedEventHandler::<
            BluetoothLEAdvertisementWatcher,
            BluetoothLEAdvertisementReceivedEventArgs,
        >::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };

            let address = args.BluetoothAddress()?;
            let rssi = args.RawSignalStrengthInDBm().unwrap_or(-127);

            let mut name = String::new();
            let mut trainer_service = false;
            if let Ok(advertisement) = args.Advertisement() {
                if let Ok(local_name) = advertisement.LocalName() {
                    name = local_name.to_string();
                }
                if let Ok(uuids) = advertisement.ServiceUuids() {
                    trainer_service = uuids.into_iter().any(|uuid| uuid == TRAINER_SERVICE);
                }
            }

            bridge.report_device(address, name.trim(), rssi, trainer_service);
            if address == target_address.load(Ordering::Relaxed) {
                target_seen.store(true, Ordering::Relaxed);
            }
            Ok(())
        }))?;

        watcher.Start()?;
        Ok(Watcher { watcher, token })
    }
}

/// Connects to the radio and subscribes to the trainer notifications.
fn connect(bridge: &Arc<Bridge>, address: u64, cache_mode: BluetoothCacheMode) -> Result<Connection, LinkError> {
    let other = |error: windows::core::Error| LinkError::Bluetooth(error.message().to_string());

    // Windows only knows unpaired devices it has seen advertising recently.
    let device = BluetoothLEDevice::FromBluetoothAddressAsync(address)
        .and_then(|operation| operation.get())
        .map_err(|_| LinkError::NotFound)?;

    // From here on `guard` closes the device again if anything fails.
    let guard = DeviceGuard(Some(device));
    let device = guard.0.as_ref().unwrap();

    let services = device
        .GetGattServicesForUuidWithCacheModeAsync(TRAINER_SERVICE, cache_mode)
        .and_then(|operation| operation.get())
        .map_err(|_| LinkError::Unreachable)?;
    if services.Status().map_err(other)? != GattCommunicationStatus::Success {
        return Err(LinkError::Unreachable);
    }
    let service = services
        .Services()
        .and_then(|list| list.GetAt(0))
        .map_err(|_| LinkError::NoTrainerService)?;

    let characteristics = service
        .GetCharacteristicsForUuidWithCacheModeAsync(TRAINER_CHARACTERISTIC, cache_mode)
        .and_then(|operation| operation.get())
        .map_err(|_| LinkError::Unreachable)?;
    if characteristics.Status().map_err(other)? != GattCommunicationStatus::Success {
        return Err(LinkError::Unreachable);
    }
    let characteristic = characteristics
        .Characteristics()
        .and_then(|list| list.GetAt(0))
        .map_err(|_| LinkError::NoCharacteristic)?;

    // Notifications -> frame decoder -> joystick.
    let decoder = Mutex::new(FrameDecoder::new());
    let frame_bridge = bridge.clone();
    let value_token = characteristic
        .ValueChanged(&TypedEventHandler::<GattCharacteristic, GattValueChangedEventArgs>::new(
            move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let buffer = args.CharacteristicValue()?;
                let length = buffer.Length()? as usize;
                if length == 0 {
                    return Ok(());
                }
                let mut data = vec![0u8; length];
                DataReader::FromBuffer(&buffer)?.ReadBytes(&mut data)?;

                let mut decoder = decoder.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let bad_before = decoder.bad_frames;
                decoder.push(&data, |frame| frame_bridge.on_frame(frame));
                let bad = decoder.bad_frames - bad_before;
                if bad > 0 {
                    frame_bridge.add_bad_frames(bad);
                }
                Ok(())
            },
        ))
        .map_err(other)?;

    let subscribed = characteristic
        .WriteClientCharacteristicConfigurationDescriptorAsync(
            GattClientCharacteristicConfigurationDescriptorValue::Notify,
        )
        .and_then(|operation| operation.get());
    if !matches!(subscribed, Ok(GattCommunicationStatus::Success)) {
        let _ = characteristic.RemoveValueChanged(value_token);
        let _ = service.Close();
        return Err(LinkError::SubscribeFailed);
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let dropped_flag = dropped.clone();
    let status_token = device
        .ConnectionStatusChanged(&TypedEventHandler::<BluetoothLEDevice, IInspectable>::new(
            move |sender, _| {
                if let Some(device) = sender {
                    if device.ConnectionStatus()? == BluetoothConnectionStatus::Disconnected {
                        dropped_flag.store(true, Ordering::Relaxed);
                    }
                }
                Ok(())
            },
        ))
        .map_err(other)?;

    // Ask Windows to keep the link up (best effort).
    let session = device
        .BluetoothDeviceId()
        .and_then(|id| GattSession::FromDeviceIdAsync(&id))
        .and_then(|operation| operation.get())
        .ok();
    if let Some(session) = &session {
        let _ = session.SetMaintainConnection(true);
    }

    let mut guard = guard;
    Ok(Connection {
        device: guard.0.take().unwrap(),
        service,
        characteristic,
        value_token,
        status_token,
        _session: session,
        dropped,
        since: Instant::now(),
    })
}

/// Closes the device unless it was moved into a [`Connection`].
struct DeviceGuard(Option<BluetoothLEDevice>);

impl Drop for DeviceGuard {
    fn drop(&mut self) {
        if let Some(device) = self.0.take() {
            let _ = device.Close();
        }
    }
}
