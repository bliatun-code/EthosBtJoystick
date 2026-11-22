//! The window (egui).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::autostart;
use crate::ble::{self, BleHandle, Command};
use crate::bridge::{Bridge, ConnectionInfo, LinkError, LinkState, Snapshot};
use crate::diag::{EventKind, LinkQuality};
use crate::tray::{self, CloseFlags, Tray, TrayHandle};
use crate::ui_diag;
use crate::config::{
    AxisMap, ButtonMap, Config, Failsafe, Lang, Mapping, SavedDevice, SwitchPos, SELECTABLE_CHANNELS, US_CENTER,
    US_MAX, US_MIN,
};
use crate::installer::{self, Action, LaunchError, Outcome};
use crate::joystick::{
    driver_status, format_driver_version, DriverStatus, AXIS_COUNT, AXIS_MAX, AXIS_NAMES, BUTTON_COUNT,
    DRIVER_VERSION,
};
use crate::protocol::{encode_frame, FrameDecoder};
use crate::util::format_address;

pub(crate) const GREEN: Color32 = Color32::from_rgb(0x3F, 0xB9, 0x50);
pub(crate) const YELLOW: Color32 = Color32::from_rgb(0xD9, 0xA4, 0x1E);
pub(crate) const RED: Color32 = Color32::from_rgb(0xE5, 0x53, 0x4B);
pub(crate) const GRAY: Color32 = Color32::from_rgb(0x8B, 0x94, 0x9E);
const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x8E, 0xEA);

const SIDE_WIDTH: f32 = 320.0;

pub(crate) fn tr<'a>(lang: Lang, no: &'a str, en: &'a str) -> &'a str {
    match lang {
        Lang::No => no,
        Lang::En => en,
    }
}

/// How the program was started (command line switches).
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// `--demo`: simulated channels, no radio needed.
    pub demo: bool,
    /// `--demo-weak`: like `--demo`, but with the uneven timing of a weak signal.
    pub demo_weak: bool,
    /// `--minimized`: start with only the tray icon (used by "Start with Windows").
    pub minimized: bool,
    /// `--diagnostics`: open the Diagnostics tab.
    pub diagnostics: bool,
}

pub fn run(start: Options) -> eframe::Result<()> {
    let (demo, minimized) = (start.demo || start.demo_weak, start.minimized);
    let config = Config::load();
    let bridge = Arc::new(Bridge::new(config.mapping.clone()));
    bridge.start_housekeeping();
    let ble = ble::spawn(bridge.clone());
    if demo {
        spawn_demo(bridge.clone(), start.demo_weak);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ethos BT Joystick")
            .with_inner_size([1000.0, 780.0])
            .with_min_inner_size([860.0, 540.0])
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Ethos BT Joystick",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc, config, bridge, ble, demo, minimized);
            if start.diagnostics {
                app.tab = Tab::Diagnostics;
            }
            Ok(Box::new(app))
        }),
    )
}

struct DeviceRow {
    address: u64,
    name: String,
    rssi: Option<i16>,
    trainer: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Channels,
    Diagnostics,
}

struct App {
    bridge: Arc<Bridge>,
    ble: Rc<BleHandle>,
    config: Config,
    saved: Config,
    selected: Option<u64>,
    /// A radio has been handed to the Bluetooth worker (connected or trying).
    wants_connection: bool,
    driver_status: DriverStatus,
    driver_checked: Instant,
    driver_job: Option<Receiver<Outcome>>,
    /// Result of the last install / uninstall attempt.
    driver_note: Option<DriverNote>,
    driver_log: String,
    demo: bool,
    /// Which page the main area shows.
    tab: Tab,

    /// The main window (0 if it could not be determined).
    main_window: isize,
    tray: Option<Tray>,
    /// Language for the tray menu and tooltip, shared with other threads.
    tray_language: Arc<AtomicU8>,
    /// The "still running in the notification area" hint was shown.
    tray_notice_shown: bool,
    /// Shared with the tray module, which intercepts the close button.
    close_flags: CloseFlags,
    /// Started with --minimized: steps left until the window is in the
    /// notification area (see `update`).
    start_in_tray: u8,
    /// "Start with Windows" is on (read from the registry).
    autostart: bool,
    settings_error: Option<String>,
}

/// What happened to the last driver install / uninstall. Kept as data rather
/// than text, so the message follows the selected language.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DriverNote {
    Finished { action: Action, success: bool },
    /// The user answered "No" in the UAC prompt.
    Cancelled,
    /// The elevated helper could not be started (message from Windows).
    LaunchFailed(String),
}

impl DriverNote {
    fn success(&self) -> bool {
        matches!(self, DriverNote::Finished { success: true, .. })
    }

    fn text(&self, lang: Lang) -> String {
        match self {
            DriverNote::Finished { action: Action::Install, success: true } => {
                tr(lang, "Driveren er installert.", "The driver is installed.").to_string()
            }
            DriverNote::Finished { action: Action::Install, success: false } => tr(
                lang,
                "Installasjonen feilet – se loggen under.",
                "The installation failed – see the log below.",
            )
            .to_string(),
            DriverNote::Finished { action: Action::Uninstall, success: true } => {
                tr(lang, "Driveren er fjernet.", "The driver has been removed.").to_string()
            }
            DriverNote::Finished { action: Action::Uninstall, success: false } => tr(
                lang,
                "Avinstalleringen feilet – se loggen under.",
                "The removal failed – see the log below.",
            )
            .to_string(),
            DriverNote::Cancelled => tr(lang, "Avbrutt.", "Cancelled.").to_string(),
            DriverNote::LaunchFailed(error) => error.clone(),
        }
    }
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        bridge: Arc<Bridge>,
        ble: BleHandle,
        demo: bool,
        minimized: bool,
    ) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 4.0);
        });

        // status changes from the Bluetooth / joystick threads redraw the window
        let ctx = cc.egui_ctx.clone();
        bridge.set_wake(move || ctx.request_repaint());
        bridge.set_fast_connection(config.fast_connection);

        let ble = Rc::new(ble);
        let main_window = match cc.window_handle().map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
            _ => 0,
        };
        let tray_language = Arc::new(AtomicU8::new(tray_language_code(config.language)));

        // The tray icon. "Exit" in its menu must work while the window is
        // hidden (when nothing is being drawn), so it cleans up and ends the
        // process directly instead of going through the window's close path.
        let tray_window = Rc::new(Cell::new(0isize));
        let close_flags = CloseFlags::default();
        close_flags.close_to_tray.store(config.close_to_tray, Ordering::Relaxed);
        let tray = (main_window != 0)
            .then(|| {
                let (bridge, ble, tray_window) = (bridge.clone(), ble.clone(), tray_window.clone());
                let on_exit = Box::new(move || {
                    ble.shutdown(Duration::from_millis(1500));
                    bridge.neutralize();
                    Tray::remove_icon(tray_window.get());
                    std::process::exit(0);
                });
                Tray::new(main_window, &app_icon().rgba, 32, tray_language.clone(), close_flags.clone(), on_exit)
            })
            .flatten();
        if let Some(tray) = &tray {
            tray_window.set(tray.handle().raw());
            spawn_tooltip_updater(bridge.clone(), tray.handle(), tray_language.clone());
        }

        let mut app = Self {
            bridge,
            ble,
            saved: config.clone(),
            selected: config.device.as_ref().map(|d| d.address),
            config,
            wants_connection: false,
            driver_status: driver_status(),
            driver_checked: Instant::now(),
            driver_job: None,
            driver_note: None,
            driver_log: String::new(),
            demo,
            tab: Tab::Channels,
            main_window,
            tray,
            tray_language,
            tray_notice_shown: false,
            close_flags,
            start_in_tray: if minimized { 2 } else { 0 },
            autostart: autostart::is_enabled(),
            settings_error: None,
        };

        if !demo && app.config.auto_connect {
            if let Some(device) = app.config.device.clone() {
                app.connect(device.address, device.name);
            }
        }
        app
    }

    fn connect(&mut self, address: u64, name: String) {
        self.config.device = Some(SavedDevice { address, name: name.clone() });
        self.selected = Some(address);
        self.ble.send(Command::Connect { address, name });
        self.wants_connection = true;
    }

    fn disconnect(&mut self) {
        self.ble.send(Command::Disconnect);
        self.wants_connection = false;
    }

    fn poll_background(&mut self) {
        if let Some(job) = &self.driver_job {
            if let Ok(outcome) = job.try_recv() {
                self.driver_job = None;
                self.bridge.suspend_joystick(false);
                self.driver_status = driver_status();
                self.driver_checked = Instant::now();
                self.driver_note = Some(DriverNote::Finished { action: outcome.action, success: outcome.success });
                self.driver_log = outcome.log;
            }
        } else if self.driver_checked.elapsed() > Duration::from_secs(3) {
            self.driver_status = driver_status();
            self.driver_checked = Instant::now();
        }
    }

    fn start_driver_job(&mut self, action: Action, ctx: &egui::Context) {
        // close our handle so Windows can replace / remove the device
        self.bridge.suspend_joystick(true);
        match installer::launch_elevated(action) {
            Ok(process) => {
                let (tx, rx) = mpsc::channel();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(process.wait());
                    ctx.request_repaint();
                });
                self.driver_job = Some(rx);
                self.driver_note = None;
            }
            Err(LaunchError::Cancelled) => {
                self.bridge.suspend_joystick(false);
                self.driver_note = Some(DriverNote::Cancelled);
            }
            Err(LaunchError::Other(error)) => {
                self.bridge.suspend_joystick(false);
                self.driver_note = Some(DriverNote::LaunchFailed(error));
            }
        }
    }

    fn device_rows(&self, snap: &Snapshot) -> Vec<DeviceRow> {
        let mut rows: Vec<DeviceRow> = snap
            .devices
            .iter()
            .filter(|d| self.config.show_unnamed || !d.name.is_empty() || d.trainer_service)
            .map(|d| DeviceRow {
                address: d.address,
                name: d.name.clone(),
                rssi: Some(d.rssi),
                trainer: d.looks_like_radio(),
            })
            .collect();
        // stable order (not by signal strength, which would make rows jump around)
        rows.sort_by(|a, b| {
            b.trainer
                .cmp(&a.trainer)
                .then(a.name.is_empty().cmp(&b.name.is_empty()))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then(a.address.cmp(&b.address))
        });

        if let Some(saved) = &self.config.device {
            match rows.iter_mut().find(|r| r.address == saved.address) {
                Some(row) => {
                    if row.name.is_empty() {
                        row.name = saved.name.clone();
                    }
                }
                None => rows.insert(
                    0,
                    DeviceRow { address: saved.address, name: saved.name.clone(), rssi: None, trainer: false },
                ),
            }
        }
        rows
    }

    // ---- sections ----------------------------------------------------------

    fn radio_section(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let lang = self.config.language;
        section_title(ui, "1", tr(lang, "Radio (Bluetooth)", "Radio (Bluetooth)"));

        let name = if snap.device_name.is_empty() { "radio" } else { snap.device_name.as_str() };
        let (color, text) = match snap.link {
            LinkState::Idle => (GRAY, tr(lang, "Ikke tilkoblet", "Not connected").to_string()),
            LinkState::Searching => (YELLOW, format!("{} {name} …", tr(lang, "Venter på", "Waiting for"))),
            LinkState::Connecting => (YELLOW, format!("{} {name} …", tr(lang, "Kobler til", "Connecting to"))),
            LinkState::Connected => {
                (YELLOW, format!("{name}: {}", tr(lang, "tilkoblet, venter på data", "connected, waiting for data")))
            }
            LinkState::Receiving => {
                // the dot and the last word say how good the link is
                let mut text = format!(
                    "{name}: {} {} · {:.0} Hz",
                    snap.channel_count,
                    tr(lang, "kanaler", "channels"),
                    snap.frame_rate
                );
                if let Some(quality) = snap.quality {
                    text.push_str(&format!(" · {}", ui_diag::quality_word(lang, quality)));
                }
                (snap.quality.map_or(GREEN, ui_diag::quality_color), text)
            }
            LinkState::SignalLost => (RED, format!("{name}: {}", tr(lang, "ingen data", "no data"))),
        };
        status_line(ui, color, &text);
        if let Some(error) = &snap.link_error {
            ui.label(RichText::new(link_error_text(lang, error)).color(YELLOW));
        }
        if snap.quick_drops {
            ui.label(RichText::new(ui_diag::quick_drops_text(lang)).color(RED));
        } else if snap.link == LinkState::Receiving && snap.quality.is_some_and(|quality| quality != LinkQuality::Good)
        {
            ui.label(
                RichText::new(tr(
                    lang,
                    "Ujevn forbindelse – se fanen Diagnose.",
                    "Uneven connection – see the Diagnostics tab.",
                ))
                .color(YELLOW),
            );
        }

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if snap.scanning {
                if ui.button(tr(lang, "Stopp søk", "Stop search")).clicked() {
                    self.ble.send(Command::StopScan);
                }
                ui.spinner();
            } else if ui
                .add_enabled(!self.demo, egui::Button::new(tr(lang, "🔍 Søk etter radio", "🔍 Search for radio")))
                .clicked()
            {
                self.ble.send(Command::StartScan);
            }
        });
        if let Some(error) = &snap.scan_error {
            ui.label(
                RichText::new(format!("{}: {error}", tr(lang, "Bluetooth-feil", "Bluetooth error")))
                    .color(RED),
            );
        }

        let rows = self.device_rows(snap);
        let mut connect_to: Option<(u64, String)> = None;

        // nothing selected yet: pre-select the first device that looks like a radio
        if self.selected.is_none() {
            self.selected = rows.iter().find(|r| r.trainer).map(|r| r.address);
        }

        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(4.0)
            .inner_margin(4.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("devices")
                    .max_height(150.0)
                    .min_scrolled_height(150.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if rows.is_empty() {
                            ui.label(
                                RichText::new(tr(
                                    lang,
                                    "Ingen radioer funnet ennå. Slå på Bluetooth-trainer (Slave) på radioen og klikk «Søk etter radio».",
                                    "No radios found yet. Enable the Bluetooth trainer (Slave) on the radio and click “Search for radio”.",
                                ))
                                .weak(),
                            );
                        }
                        for row in &rows {
                            let label = if row.name.is_empty() { format_address(row.address) } else { row.name.clone() };
                            ui.horizontal(|ui| {
                                let response = ui
                                    .selectable_label(self.selected == Some(row.address), &label)
                                    .on_hover_text(format_address(row.address));
                                if response.clicked() {
                                    self.selected = Some(row.address);
                                }
                                if response.double_clicked() && !self.wants_connection {
                                    connect_to = Some((row.address, label.clone()));
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    match row.rssi {
                                        Some(rssi) => ui.label(RichText::new(format!("{rssi} dBm")).small().weak()),
                                        None => ui.label(
                                            RichText::new(tr(lang, "sist brukt", "last used")).small().weak(),
                                        ),
                                    };
                                    if row.trainer {
                                        ui.label(RichText::new("FrSky").small().color(GREEN)).on_hover_text(tr(
                                            lang,
                                            "Ser ut som en FrSky-radio (navnet eller trainer-tjenesten FFF0)",
                                            "Looks like a FrSky radio (its name or the FFF0 trainer service)",
                                        ));
                                    }
                                });
                            });
                        }
                    });
            });

        ui.horizontal(|ui| {
            if self.wants_connection {
                if ui.button(tr(lang, "Koble fra", "Disconnect")).clicked() {
                    self.disconnect();
                }
            } else {
                let target = rows.iter().find(|r| Some(r.address) == self.selected);
                let button = egui::Button::new(RichText::new(tr(lang, "Koble til", "Connect")).strong());
                if ui.add_enabled(target.is_some() && !self.demo, button).clicked() {
                    if let Some(row) = target {
                        let name = if row.name.is_empty() { format_address(row.address) } else { row.name.clone() };
                        connect_to = Some((row.address, name));
                    }
                }
            }
        });

        if let Some((address, name)) = connect_to {
            self.connect(address, name);
        }

        ui.checkbox(
            &mut self.config.auto_connect,
            tr(lang, "Koble til automatisk ved oppstart", "Connect automatically at startup"),
        );
        ui.checkbox(&mut self.config.show_unnamed, tr(lang, "Vis enheter uten navn", "Show unnamed devices"));
    }

    fn joystick_section(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let lang = self.config.language;
        section_title(ui, "2", tr(lang, "Virtuell joystick", "Virtual joystick"));

        let busy = self.driver_job.is_some();
        // an older driver than the one built into this program is installed
        let outdated = snap.driver_version.filter(|version| *version < DRIVER_VERSION);
        let (color, text) = if busy {
            (YELLOW, tr(lang, "Arbeider …", "Working …").to_string())
        } else {
            match (self.driver_status, snap.joystick_open) {
                (DriverStatus::Running, true) if outdated.is_some() => (
                    YELLOW,
                    format!(
                        "{} {} – {} {}",
                        tr(lang, "Klar, men med driver", "Ready, but with driver"),
                        format_driver_version(outdated.unwrap_or_default()),
                        tr(lang, "oppdater til", "update to"),
                        format_driver_version(DRIVER_VERSION)
                    ),
                ),
                (DriverStatus::Running, true) => (
                    GREEN,
                    tr(lang, "Klar – vises som «Ethos BT Joystick»", "Ready – shows up as “Ethos BT Joystick”")
                        .to_string(),
                ),
                (DriverStatus::Running, false) => {
                    (YELLOW, tr(lang, "Driveren kjører, åpner enheten …", "Driver running, opening device …").to_string())
                }
                (DriverStatus::Problem(code), _) => (
                    RED,
                    format!(
                        "{} ({} {code})",
                        tr(lang, "Driveren er installert, men startet ikke", "The driver is installed but did not start"),
                        tr(lang, "kode", "code")
                    ),
                ),
                (DriverStatus::NotInstalled, _) => {
                    (GRAY, tr(lang, "Driveren er ikke installert", "The driver is not installed").to_string())
                }
            }
        };
        status_line(ui, color, &text);

        ui.add_space(2.0);
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
            if self.driver_status == DriverStatus::NotInstalled {
                let button = egui::Button::new(RichText::new(tr(lang, "Installer driver …", "Install driver …")).strong());
                if ui.add_enabled(!busy, button).clicked() {
                    self.start_driver_job(Action::Install, &ctx);
                }
            } else {
                let button = if outdated.is_some() {
                    egui::Button::new(RichText::new(tr(lang, "Oppdater driver …", "Update driver …")).strong())
                } else {
                    egui::Button::new(tr(lang, "Installer på nytt", "Reinstall"))
                };
                if ui.add_enabled(!busy, button).clicked() {
                    self.start_driver_job(Action::Install, &ctx);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(tr(lang, "Avinstaller", "Uninstall")))
                    .clicked()
                {
                    self.start_driver_job(Action::Uninstall, &ctx);
                }
            }
            if busy {
                ui.spinner();
            }
        });
        if ui
            .add_enabled(
                self.driver_status == DriverStatus::Running,
                egui::Button::new(tr(lang, "🎮 Test i Windows (joy.cpl)", "🎮 Test in Windows (joy.cpl)")),
            )
            .clicked()
        {
            let _ = std::process::Command::new("control.exe").arg("joy.cpl").spawn();
        }

        if let Some(note) = &self.driver_note {
            ui.label(RichText::new(note.text(lang)).color(if note.success() { GREEN } else { RED }));
        }
        if outdated.is_some() && !busy {
            ui.label(
                RichText::new(tr(
                    lang,
                    "Den nye driveren setter joysticken i nøytral hvis programmet stopper uventet.",
                    "The new driver returns the joystick to neutral if the program stops unexpectedly.",
                ))
                .weak(),
            );
        }
        if self.driver_status == DriverStatus::NotInstalled && !busy {
            ui.label(
                RichText::new(tr(
                    lang,
                    "Krever administrator (UAC). Det lages et lokalt sertifikat som bare brukes til å signere denne driveren på denne PC-en.",
                    "Requires administrator (UAC). A local certificate is created that is only used to sign this driver on this PC.",
                ))
                .weak(),
            );
        }
        if !self.driver_log.is_empty() {
            egui::CollapsingHeader::new(tr(lang, "Installasjonslogg", "Installation log"))
                .id_salt("driver_log")
                .default_open(self.driver_note.as_ref().is_some_and(|note| !note.success()))
                .show(ui, |ui| {
                    egui::ScrollArea::both().id_salt("driver_log_scroll").max_height(160.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(&self.driver_log).monospace().small()).selectable(true));
                    });
                });
        }
    }

    fn settings_section(&mut self, ui: &mut egui::Ui) {
        let lang = self.config.language;
        section_title(ui, "3", tr(lang, "Innstillinger", "Settings"));

        let mut autostart = self.autostart;
        if ui
            .checkbox(&mut autostart, tr(lang, "Start sammen med Windows (minimert)", "Start with Windows (minimised)"))
            .changed()
        {
            self.settings_error = autostart::set_enabled(autostart).err();
            self.autostart = autostart::is_enabled();
        }
        ui.add_enabled(
            self.tray.as_ref().is_some_and(|tray| tray.intercepts_close()),
            egui::Checkbox::new(
                &mut self.config.close_to_tray,
                tr(
                    lang,
                    "Lukk-knappen skjuler vinduet i systemstatusfeltet",
                    "The close button hides the window in the notification area",
                ),
            ),
        );
        if let Some(error) = &self.settings_error {
            ui.label(RichText::new(error).color(RED));
        }
    }

    /// Sends the window to the notification area; the tray icon brings it back.
    fn hide_to_tray(&mut self) {
        let lang = self.config.language;
        let Some(tray) = &self.tray else { return };
        tray::send_to_tray(self.main_window);
        if !self.tray_notice_shown {
            self.tray_notice_shown = true;
            tray.show_notice(
                "Ethos BT Joystick",
                tr(
                    lang,
                    "Programmet kjører videre her. Klikk på ikonet for å åpne vinduet, høyreklikk for å avslutte.",
                    "The program keeps running here. Click the icon to open the window, right-click to exit.",
                ),
            );
        }
    }

    fn help_section(&mut self, ui: &mut egui::Ui) {
        let lang = self.config.language;
        egui::CollapsingHeader::new(tr(lang, "Slik gjør du det", "How to use"))
            .id_salt("help")
            .default_open(true)
            .show(ui, |ui| {
                let steps: [&str; 4] = match lang {
                    Lang::No => [
                        "På radioen: Model > Trainer > legg til en Bluetooth-link, slå den på og velg Slave (CH1–CH8 eller CH1–CH16).",
                        "Klikk «Installer driver» (bare første gang).",
                        "Klikk «Søk etter radio», velg radioen og klikk «Koble til».",
                        "Velg «Ethos BT Joystick» som kontroller i simulatoren og kalibrer der.",
                    ],
                    Lang::En => [
                        "On the radio: Model > Trainer > add a Bluetooth link, enable it and select Slave (CH1–CH8 or CH1–CH16).",
                        "Click “Install driver” (first time only).",
                        "Click “Search for radio”, select the radio and click “Connect”.",
                        "Select “Ethos BT Joystick” as controller in the simulator and calibrate it there.",
                    ],
                };
                for (i, step) in steps.iter().enumerate() {
                    ui.horizontal_top(|ui| {
                        ui.label(RichText::new(format!("{}.", i + 1)).strong());
                        ui.add(egui::Label::new(RichText::new(*step)).wrap());
                    });
                }
            });
    }

    fn channels_section(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let lang = self.config.language;
        ui.heading(tr(lang, "Kanaler fra radioen", "Channels from the radio"));

        let live = snap.link == LinkState::Receiving && snap.channel_count > 0;
        let count = if snap.channel_count > 0 { snap.channel_count.min(SELECTABLE_CHANNELS as usize) } else { 8 };
        let rows = count.div_ceil(2);
        let bar_width = ((ui.available_width() - 2.0 * 110.0 - 24.0) / 2.0).clamp(120.0, 260.0);

        egui::Grid::new("channels").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
            for row in 0..rows {
                for column in 0..2 {
                    let index = row + column * rows;
                    if index >= count {
                        continue;
                    }
                    let us = snap.channels[index];
                    ui.horizontal(|ui| {
                        ui.add_sized([38.0, 14.0], egui::Label::new(RichText::new(format!("CH{}", index + 1)).monospace()));
                        center_bar(ui, us_to_norm(us), bar_width, live);
                        let value = if live { format!("{us:4}") } else { "   –".to_string() };
                        ui.label(RichText::new(value).monospace().weak());
                    });
                }
                ui.end_row();
            }
        });
    }

    fn axes_section(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let lang = self.config.language;
        ui.heading(tr(lang, "Joystick-akser", "Joystick axes"));

        let live = snap.link == LinkState::Receiving;
        let bar_width = (ui.available_width() - 300.0).clamp(140.0, 320.0);

        egui::Grid::new("axes").num_columns(4).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label(RichText::new(tr(lang, "Akse", "Axis")).weak());
            ui.label(RichText::new(tr(lang, "Kilde", "Source")).weak());
            ui.label(RichText::new(tr(lang, "Inverter", "Invert")).weak());
            ui.label(RichText::new(tr(lang, "Til simulatoren", "To the simulator")).weak());
            ui.end_row();

            for i in 0..AXIS_COUNT {
                let axis: &mut AxisMap = &mut self.config.mapping.axes[i];
                ui.label(RichText::new(AXIS_NAMES[i]).strong());

                let selected = match axis.channel {
                    Some(channel) => format!("CH{}", channel + 1),
                    None => "–".to_string(),
                };
                egui::ComboBox::from_id_salt(("axis_source", i)).selected_text(selected).width(72.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut axis.channel, None, tr(lang, "– (ingen)", "– (none)"));
                    for channel in 0..SELECTABLE_CHANNELS {
                        ui.selectable_value(&mut axis.channel, Some(channel), format!("CH{}", channel + 1));
                    }
                });

                ui.checkbox(&mut axis.invert, "");

                let norm = snap.output.axes[i] as f32 / AXIS_MAX as f32 * 2.0 - 1.0;
                center_bar(ui, norm, bar_width, live && axis.channel.is_some());
                ui.end_row();
            }
        });
    }

    fn buttons_section(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let lang = self.config.language;
        ui.heading(tr(lang, "Knapper (brytere)", "Buttons (switches)"));
        ui.label(
            RichText::new(tr(
                lang,
                "En joystick-knapp er trykket så lenge kanalen står i valgt posisjon.",
                "A joystick button is pressed for as long as the channel is in the selected position.",
            ))
            .weak(),
        );

        let mut remove: Option<usize> = None;
        if !self.config.mapping.buttons.is_empty() {
            egui::Grid::new("buttons").num_columns(5).spacing([16.0, 6.0]).show(ui, |ui| {
                for (i, button) in self.config.mapping.buttons.iter_mut().enumerate() {
                    ui.label(RichText::new(format!("{} {}", tr(lang, "Knapp", "Button"), i + 1)).strong());

                    egui::ComboBox::from_id_salt(("button_channel", i))
                        .selected_text(format!("CH{}", button.channel + 1))
                        .width(72.0)
                        .show_ui(ui, |ui| {
                            for channel in 0..SELECTABLE_CHANNELS {
                                ui.selectable_value(&mut button.channel, channel, format!("CH{}", channel + 1));
                            }
                        });

                    egui::ComboBox::from_id_salt(("button_position", i))
                        .selected_text(switch_pos_text(lang, button.position))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for position in [SwitchPos::Low, SwitchPos::Mid, SwitchPos::High] {
                                ui.selectable_value(&mut button.position, position, switch_pos_text(lang, position));
                            }
                        });

                    let pressed = snap.output.buttons & (1 << i) != 0;
                    led(ui, pressed);

                    if ui.small_button("✖").on_hover_text(tr(lang, "Fjern", "Remove")).clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        }
        if let Some(index) = remove {
            self.config.mapping.buttons.remove(index);
        }

        ui.horizontal(|ui| {
            let can_add = self.config.mapping.buttons.len() < BUTTON_COUNT;
            if ui
                .add_enabled(can_add, egui::Button::new(tr(lang, "➕ Legg til knapp", "➕ Add button")))
                .clicked()
            {
                // default: the next channel after the last one used, starting at CH5
                let channel = self
                    .config
                    .mapping
                    .buttons
                    .last()
                    .map(|b| (b.channel + 1).min(SELECTABLE_CHANNELS - 1))
                    .unwrap_or(4);
                self.config.mapping.buttons.push(ButtonMap { channel, position: SwitchPos::High });
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(tr(lang, "Hvis radioen slutter å sende:", "If the radio stops sending:"));
            let text = |failsafe: Failsafe| match failsafe {
                Failsafe::Center => tr(lang, "Sentrer akser, slipp knapper", "Centre axes, release buttons"),
                Failsafe::Hold => tr(lang, "Behold siste verdier", "Keep the last values"),
            };
            egui::ComboBox::from_id_salt("failsafe")
                .selected_text(text(self.config.mapping.failsafe))
                .width(220.0)
                .show_ui(ui, |ui| {
                    for failsafe in [Failsafe::Center, Failsafe::Hold] {
                        ui.selectable_value(&mut self.config.mapping.failsafe, failsafe, text(failsafe));
                    }
                });
        });
        ui.horizontal(|ui| {
            if ui.button(tr(lang, "Tilbakestill mapping", "Reset mapping")).clicked() {
                self.config.mapping = Mapping::default();
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background();
        let snap = self.bridge.snapshot();
        let lang = self.config.language;
        self.tray_language.store(tray_language_code(lang), Ordering::Relaxed);

        // The close button can send the window to the notification area
        // instead of ending the program. The tray module intercepts it (see
        // `tray::main_window_proc`) and leaves a request here.
        self.close_flags.close_to_tray.store(self.config.close_to_tray, Ordering::Relaxed);
        if self.close_flags.hide_requested.swap(false, Ordering::Relaxed) {
            self.hide_to_tray();
        }

        // Started with --minimized. eframe shows the window after drawing its
        // first frame, so: make it invisible on screen before that happens,
        // then move it to the notification area in the next frame.
        match self.start_in_tray {
            2 if self.tray.is_some() => {
                tray::cloak_window(self.main_window);
                self.start_in_tray = 1;
                ctx.request_repaint();
            }
            1 => {
                self.start_in_tray = 0;
                tray::send_to_tray(self.main_window);
            }
            _ => self.start_in_tray = 0,
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Ethos BT Joystick");
                ui.label(
                    RichText::new(tr(
                        lang,
                        "FrSky Ethos-radio som trådløs joystick for simulatorer",
                        "FrSky Ethos radio as a wireless joystick for simulators",
                    ))
                    .weak(),
                );
                if self.demo {
                    ui.label(RichText::new("DEMO").strong().color(YELLOW));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let current = match self.config.language {
                        Lang::No => "Norsk",
                        Lang::En => "English",
                    };
                    egui::ComboBox::from_id_salt("language").selected_text(current).width(84.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.config.language, Lang::No, "Norsk");
                        ui.selectable_value(&mut self.config.language, Lang::En, "English");
                    });
                });
            });
            ui.add_space(2.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}: {}   ·   {}: {}",
                        tr(lang, "Rammer mottatt", "Frames received"),
                        snap.frames_total,
                        tr(lang, "forkastet", "dropped"),
                        snap.bad_frames
                    ))
                    .small()
                    .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).small().weak());
                });
            });
        });

        egui::SidePanel::left("side").resizable(false).exact_width(SIDE_WIDTH).show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("side_scroll").show(ui, |ui| {
                ui.add_space(6.0);
                self.radio_section(ui, &snap);
                ui.add_space(10.0);
                ui.separator();
                self.joystick_section(ui, &snap);
                ui.add_space(10.0);
                ui.separator();
                self.settings_section(ui);
                ui.add_space(10.0);
                ui.separator();
                self.help_section(ui);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Channels,
                    RichText::new(tr(lang, "Kanaler og mapping", "Channels and mapping")).size(15.0),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Diagnostics,
                    RichText::new(tr(lang, "Diagnose", "Diagnostics")).size(15.0),
                );
            });
            ui.separator();

            egui::ScrollArea::vertical().id_salt("main_scroll").auto_shrink([false, false]).show(ui, |ui| {
                match self.tab {
                    Tab::Channels => {
                        self.channels_section(ui, &snap);
                        ui.add_space(12.0);
                        self.axes_section(ui, &snap);
                        ui.add_space(12.0);
                        self.buttons_section(ui, &snap);
                    }
                    Tab::Diagnostics => {
                        let mut actions = Vec::new();
                        ui_diag::show(ui, lang, &snap, &mut self.config, self.driver_status, &mut actions);
                        for action in actions {
                            match action {
                                ui_diag::Action::RefreshAdapter => self.ble.send(Command::RefreshAdapter),
                                ui_diag::Action::ClearEvents => self.bridge.clear_events(),
                            }
                        }
                    }
                }
            });
        });

        if self.config != self.saved {
            if self.config.mapping != self.saved.mapping {
                self.bridge.set_mapping(self.config.mapping.clone());
            }
            self.bridge.set_fast_connection(self.config.fast_connection);
            self.config.save();
            self.saved = self.config.clone();
        }

        // Redraw quickly only while something on screen moves (live channels,
        // spinners). Otherwise a slow tick is enough: status changes from the
        // other threads request a redraw themselves (see `Bridge::set_wake`).
        let minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
        let moving = matches!(snap.link, LinkState::Receiving | LinkState::Connecting)
            || snap.scanning
            || self.driver_job.is_some();
        let interval = if moving && !minimized { 40 } else { 500 };
        ctx.request_repaint_after(Duration::from_millis(interval));
    }
}

fn tray_language_code(lang: Lang) -> u8 {
    match lang {
        Lang::No => tray::LANG_NORWEGIAN,
        Lang::En => tray::LANG_ENGLISH,
    }
}

/// One line for the tray icon's tooltip.
fn tray_tooltip(english: bool, snap: &Snapshot) -> String {
    let lang = if english { Lang::En } else { Lang::No };
    let name = if snap.device_name.is_empty() { "radio" } else { snap.device_name.as_str() };
    let status = match snap.link {
        LinkState::Idle => tr(lang, "ikke tilkoblet", "not connected").to_string(),
        LinkState::Searching => format!("{} {name}", tr(lang, "venter på", "waiting for")),
        LinkState::Connecting => format!("{} {name}", tr(lang, "kobler til", "connecting to")),
        LinkState::Connected => format!("{name}: {}", tr(lang, "venter på data", "waiting for data")),
        LinkState::Receiving => {
            format!("{name}: {} {}", snap.channel_count, tr(lang, "kanaler", "channels"))
        }
        LinkState::SignalLost => format!("{name}: {}", tr(lang, "ingen data", "no data")),
    };
    format!("Ethos BT Joystick – {status}")
}

/// Keeps the tray tooltip in step with the connection, independently of how
/// often the window is drawn.
fn spawn_tooltip_updater(bridge: Arc<Bridge>, tray: TrayHandle, language: Arc<AtomicU8>) {
    let _ = std::thread::Builder::new().name("tray-tooltip".into()).spawn(move || {
        let mut shown = String::new();
        loop {
            let english = language.load(Ordering::Relaxed) == tray::LANG_ENGLISH;
            let text = tray_tooltip(english, &bridge.snapshot());
            if text != shown {
                tray.set_tooltip(&text);
                shown = text;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

impl Drop for App {
    fn drop(&mut self) {
        self.config.save();
        // centre the joystick and release the Bluetooth link before the process exits
        self.ble.shutdown(Duration::from_millis(1500));
        self.bridge.neutralize();
    }
}

// ---- small widgets ---------------------------------------------------------

fn section_title(ui: &mut egui::Ui, number: &str, title: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 10.0, ACCENT);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            number,
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
        ui.label(RichText::new(title).strong().size(16.0));
    });
}

pub(crate) fn status_line(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 5.0, color);
        ui.add(egui::Label::new(text).wrap());
    });
}

fn led(ui: &mut egui::Ui, on: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let color = if on { GREEN } else { ui.visuals().extreme_bg_color };
    ui.painter().circle_filled(rect.center(), 6.0, color);
    ui.painter().circle_stroke(rect.center(), 6.0, egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()));
}

/// Horizontal bar that fills from the centre, like a stick position.
fn center_bar(ui: &mut egui::Ui, norm: f32, width: f32, live: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let background = ui.visuals().extreme_bg_color;
    let tick = ui.visuals().weak_text_color();
    let painter = ui.painter();

    painter.rect_filled(rect, 3.0, background);
    let center = rect.center().x;
    if live {
        let x = center + norm.clamp(-1.0, 1.0) * (rect.width() / 2.0 - 2.0);
        let fill = egui::Rect::from_min_max(
            egui::pos2(center.min(x), rect.top() + 2.0),
            egui::pos2(center.max(x), rect.bottom() - 2.0),
        );
        painter.rect_filled(fill, 0.0, ACCENT.gamma_multiply(0.45));
        let knob = egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(4.0, rect.height()));
        painter.rect_filled(knob, 1.0, ACCENT);
    }
    painter.line_segment(
        [egui::pos2(center, rect.top()), egui::pos2(center, rect.bottom())],
        egui::Stroke::new(1.0_f32, tick),
    );
}

fn us_to_norm(us: u16) -> f32 {
    let half = (US_MAX - US_MIN) as f32 / 2.0;
    (us as f32 - US_CENTER as f32) / half
}

fn switch_pos_text(lang: Lang, position: SwitchPos) -> &'static str {
    match position {
        SwitchPos::Low => tr(lang, "Lav (≤ 1300 µs)", "Low (≤ 1300 µs)"),
        SwitchPos::Mid => tr(lang, "Midt (1301–1699 µs)", "Middle (1301–1699 µs)"),
        SwitchPos::High => tr(lang, "Høy (≥ 1700 µs)", "High (≥ 1700 µs)"),
    }
}

pub(crate) fn link_error_text(lang: Lang, error: &LinkError) -> String {
    match error {
        LinkError::NotFound => tr(
            lang,
            "Fant ikke radioen. Er Bluetooth-trainer (Slave) slått på?",
            "Radio not found. Is the Bluetooth trainer (Slave) enabled?",
        )
        .to_string(),
        LinkError::Unreachable => tr(
            lang,
            "Fikk ikke kontakt med radioen – prøver igjen.",
            "Could not reach the radio – trying again.",
        )
        .to_string(),
        LinkError::NoTrainerService => tr(
            lang,
            "Enheten har ikke FrSky trainer-tjenesten (FFF0). Er dette riktig enhet?",
            "The device has no FrSky trainer service (FFF0). Is this the right device?",
        )
        .to_string(),
        LinkError::NoCharacteristic => tr(
            lang,
            "Fant ikke trainer-karakteristikken (FFF6) på enheten.",
            "The trainer characteristic (FFF6) was not found on the device.",
        )
        .to_string(),
        LinkError::SubscribeFailed => tr(
            lang,
            "Kunne ikke slå på varsler fra radioen – prøver igjen.",
            "Could not enable notifications from the radio – trying again.",
        )
        .to_string(),
        LinkError::LinkLost => tr(
            lang,
            "Forbindelsen ble brutt – kobler til på nytt.",
            "The connection was lost – reconnecting.",
        )
        .to_string(),
        LinkError::NoData => tr(
            lang,
            "Ingen trainer-data mottatt – kobler til på nytt.",
            "No trainer data received – reconnecting.",
        )
        .to_string(),
        LinkError::Bluetooth(message) => format!("{}: {message}", tr(lang, "Bluetooth-feil", "Bluetooth error")),
    }
}

/// 32x32 window icon: blue rounded square with a white "stick".
fn app_icon() -> egui::IconData {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5 - 16.0, y as f32 + 0.5 - 16.0);
            // rounded square
            let (dx, dy) = ((fx.abs() - 10.0).max(0.0), (fy.abs() - 10.0).max(0.0));
            let inside = (dx * dx + dy * dy).sqrt() <= 5.5;
            // gimbal ring + stick knob
            let r = (fx * fx + fy * fy).sqrt();
            let ring = (r - 9.5).abs() <= 1.2;
            let knob = ((fx - 3.0).powi(2) + (fy + 3.0).powi(2)).sqrt() <= 4.2;

            let pixel = &mut rgba[(y * SIZE + x) * 4..(y * SIZE + x) * 4 + 4];
            if inside {
                let color = if ring || knob { [255, 255, 255] } else { [0x3B, 0x8E, 0xEA] };
                pixel[..3].copy_from_slice(&color);
                pixel[3] = 255;
            }
        }
    }
    egui::IconData { rgba, width: SIZE as u32, height: SIZE as u32 }
}

/// Demo mode (`--demo`): feeds synthetic 16 channel frames through the real
/// decoder so the window can be tried without a radio. It goes through the
/// same steps as a real connection, so the Diagnostics tab has something to show.
fn spawn_demo(bridge: Arc<Bridge>, weak: bool) {
    std::thread::spawn(move || {
        bridge.log(EventKind::Target("Demo".into()));
        bridge.begin_connection("Demo");
        bridge.set_link(LinkState::Connecting, None);
        let connecting = Instant::now();
        std::thread::sleep(Duration::from_millis(900));
        bridge.log(EventKind::FastRequested { accepted: true });
        bridge.connection_established(connecting.elapsed().as_millis() as u32);
        // what Windows reports for a radio once it has granted the fast
        // interval: 15 ms, 1M PHY, 2 s supervision timeout
        bridge.set_connection_info(ConnectionInfo { interval_units: 12, latency: 0, timeout_units: 200, phy: 1 });
        bridge.set_link(LinkState::Connected, None);

        let mut decoder = FrameDecoder::new();
        let start = Instant::now();
        let mut deadline = start;
        let mut frame_number = 0u32;
        loop {
            // A good link: the radio's 100 frames a second arrive one or two
            // at a time, once per 15 ms connection interval.
            // A weak signal: after two good seconds, frames arrive in short
            // bursts with pauses in between (packets lost and sent again).
            frame_number += 1;
            let pause = if weak && start.elapsed() > Duration::from_secs(2) {
                if frame_number % 5 == 0 { 85 } else { 4 }
            } else if frame_number % 3 == 0 {
                0
            } else {
                15
            };
            let t = start.elapsed().as_secs_f32();
            let mut channels = [US_CENTER; 16];
            for (i, channel) in channels.iter_mut().enumerate().take(4) {
                let phase = t * (0.9 + i as f32 * 0.37) + i as f32;
                *channel = (US_CENTER as f32 + 480.0 * phase.sin()) as u16;
            }
            for (i, channel) in channels.iter_mut().enumerate().skip(4) {
                // switches: step through low / middle / high
                let step = ((t / (1.5 + i as f32 * 0.4)) as usize + i) % 3;
                *channel = [US_MIN, US_CENTER, US_MAX][step];
            }
            decoder.push(&encode_frame(&channels), |frame| bridge.on_frame(frame));

            // Sleep towards a deadline so the rate stays exact however coarse
            // the timer is; start over if the PC was asleep in between.
            deadline += Duration::from_millis(pause);
            let now = Instant::now();
            if now > deadline + Duration::from_secs(1) {
                deadline = now;
            }
            std::thread::sleep(deadline.saturating_duration_since(now));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_note_follows_the_language() {
        let note = DriverNote::Finished { action: Action::Install, success: true };
        assert_eq!(note.text(Lang::No), "Driveren er installert.");
        assert_eq!(note.text(Lang::En), "The driver is installed.");
        assert!(note.success());

        let failed = DriverNote::Finished { action: Action::Uninstall, success: false };
        assert!(!failed.success());
        assert_ne!(failed.text(Lang::No), failed.text(Lang::En));

        assert!(!DriverNote::Cancelled.success());
        assert_eq!(DriverNote::Cancelled.text(Lang::En), "Cancelled.");
        assert_eq!(DriverNote::LaunchFailed("x".into()).text(Lang::No), "x");
    }
}
