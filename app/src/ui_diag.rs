//! The "Diagnostics" tab and the text report behind "Copy diagnostics".

use std::fmt::Write as _;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};

use crate::adapter::{self, AdapterInfo};
use crate::bridge::{LinkState, Snapshot};
use crate::config::{Config, Lang};
use crate::diag::{pause_cause, DropReason, Event, EventKind, LinkQuality, PauseCause, HISTORY_SECONDS};
use crate::joystick::{format_driver_version, DriverStatus};
use crate::ui::{link_error_text, status_line, tr, GRAY, GREEN, RED, YELLOW};

/// What the tab asks the app to do.
pub enum Action {
    RefreshAdapter,
    ClearEvents,
}

pub fn quality_color(quality: LinkQuality) -> Color32 {
    match quality {
        LinkQuality::Good => GREEN,
        LinkQuality::Weak => YELLOW,
        LinkQuality::Poor => RED,
    }
}

pub fn quality_word(lang: Lang, quality: LinkQuality) -> &'static str {
    match quality {
        LinkQuality::Good => tr(lang, "god", "good"),
        LinkQuality::Weak => tr(lang, "svak", "weak"),
        LinkQuality::Poor => tr(lang, "dårlig", "poor"),
    }
}

/// Advice for a link that is not good.
fn quality_advice(lang: Lang, quality: LinkQuality, cause: PauseCause) -> Option<&'static str> {
    match (quality, cause) {
        (LinkQuality::Good, _) => None,
        // The pauses are what one lost connection interval costs: a shorter
        // interval helps, a stronger signal would not.
        (_, PauseCause::LongInterval) => Some(tr(
            lang,
            "Oppholdene kommer av det lange forbindelsesintervallet: hver gang ett intervall går tapt, blir oppholdet dobbelt så langt. Prøv «Be Windows om raskest mulig forbindelse» nedenfor.",
            "The pauses come from the long connection interval: every time one interval is lost, the pause is twice as long. Try “Ask Windows for the fastest connection” below.",
        )),
        (LinkQuality::Weak, PauseCause::Signal) => Some(tr(
            lang,
            "Rammene kommer ujevnt. Flytt radioen nærmere PC-en, og sjekk at Bluetooth-antennene på PC-en er montert.",
            "Frames arrive unevenly. Move the radio closer to the PC, and check that the PC's Bluetooth antennas are fitted.",
        )),
        (LinkQuality::Poor, PauseCause::Signal) => Some(tr(
            lang,
            "For ustabilt til presis styring. Flytt radioen nærmere PC-en, og sjekk at Bluetooth-antennene på PC-en er montert.",
            "Too unstable for precise control. Move the radio closer to the PC, and check that the PC's Bluetooth antennas are fitted.",
        )),
    }
}

/// True when frames clearly arrive in bursts rather than one by one.
fn bursty(snap: &Snapshot) -> bool {
    snap.update_rate > 0.0 && snap.update_rate < snap.frame_rate * 0.7
}

pub fn quick_drops_text(lang: Lang) -> &'static str {
    tr(
        lang,
        "Forbindelsen faller gjentatte ganger etter få sekunder. Det tyder på Bluetooth-adapteren eller driveren på PC-en: prøv en nyere Bluetooth-driver eller en annen adapter.",
        "The connection keeps dropping after a few seconds. That points to the PC's Bluetooth adapter or its driver: try a newer Bluetooth driver or another adapter.",
    )
}

fn duration_text(lang: Lang, duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if h > 0 {
        format!("{h} {} {m} min", tr(lang, "t", "h"))
    } else if m > 0 {
        format!("{m} min {s} s")
    } else if seconds >= 10 {
        format!("{s} s")
    } else {
        format!("{:.1} s", duration.as_secs_f32())
    }
}

pub fn event_text(lang: Lang, event: &Event) -> String {
    let mut text = match &event.kind {
        EventKind::Target(name) => format!("{} {name}", tr(lang, "Kobler til", "Connecting to")),
        EventKind::ConnectFailed(error) => {
            format!("{}: {}", tr(lang, "Tilkobling feilet", "Connection failed"), link_error_text(lang, error))
        }
        EventKind::Connected { setup_ms } => {
            format!("{} ({} {setup_ms} ms)", tr(lang, "Tilkoblet", "Connected"), tr(lang, "oppsett", "set up in"))
        }
        EventKind::FirstData { after_ms, channels } => format!(
            "{} {after_ms} ms, {channels} {}",
            tr(lang, "Første data etter", "First data after"),
            tr(lang, "kanaler", "channels")
        ),
        EventKind::SignalLost => tr(lang, "Data stoppet", "Data stopped").to_string(),
        EventKind::SignalBack { gap_ms } => {
            format!("{} {gap_ms} ms", tr(lang, "Data tilbake etter", "Data back after"))
        }
        EventKind::Disconnected { reason, lasted, frames } => {
            let why = match reason {
                DropReason::LinkLost => tr(lang, "Bluetooth-forbindelsen ble brutt", "the Bluetooth link went down"),
                DropReason::NoData => tr(
                    lang,
                    "ingen data på 5 s, appen lukket forbindelsen",
                    "no data for 5 s, the app closed the link",
                ),
                DropReason::User => tr(lang, "koblet fra av brukeren", "disconnected by the user"),
            };
            format!(
                "{} {}: {why} ({frames} {})",
                tr(lang, "Frakoblet etter", "Disconnected after"),
                duration_text(lang, *lasted),
                tr(lang, "rammer", "frames")
            )
        }
        EventKind::Parameters { interval_units, latency, timeout_ms } => format!(
            "{} {:.2} ms, {} {latency}, {} {timeout_ms} ms",
            tr(lang, "Intervall", "Interval"),
            *interval_units as f32 * 1.25,
            tr(lang, "latens", "latency"),
            tr(lang, "tidsavbrudd", "timeout")
        ),
        EventKind::FastRequested { accepted } => format!(
            "{}: {}",
            tr(lang, "Ba om raskest mulig intervall", "Asked for the fastest interval"),
            if *accepted { tr(lang, "godtatt", "accepted") } else { tr(lang, "ikke godtatt", "not accepted") }
        ),
    };
    if event.repeats > 1 {
        let _ = write!(text, " (×{})", event.repeats);
    }
    text
}

fn adapter_lines(lang: Lang, adapter: &AdapterInfo) -> (String, String) {
    let mut first = adapter.name.clone();
    if !adapter.chip_maker.is_empty() && !adapter.name.to_lowercase().contains(&adapter.chip_maker.to_lowercase()) {
        let _ = write!(first, " ({})", adapter.chip_maker);
    }
    if !adapter.bluetooth_version.is_empty() {
        let _ = write!(first, " · Bluetooth {}", adapter.bluetooth_version);
    }
    let driver = format!(
        "{}: {} {}, {}",
        tr(lang, "Driver", "Driver"),
        adapter.driver_provider,
        adapter.driver_version,
        if adapter.driver_date.is_empty() { "?" } else { &adapter.driver_date }
    );
    (first, driver)
}

/// Notes about an adapter that is a likely suspect when the link is unstable.
fn adapter_notes(lang: Lang, adapter: &AdapterInfo, this_year: u32) -> Vec<String> {
    let mut notes = Vec::new();
    if adapter.radio_on == Some(false) {
        notes.push(tr(lang, "Bluetooth er slått av i Windows.", "Bluetooth is switched off in Windows.").to_string());
    }
    if adapter.low_energy == Some(false) || adapter.central_role == Some(false) {
        notes.push(
            tr(
                lang,
                "Adapteren støtter ikke Bluetooth Low Energy som sentral enhet, og kan ikke brukes med radioen.",
                "The adapter does not support Bluetooth Low Energy in the central role and cannot be used with the radio.",
            )
            .to_string(),
        );
    }
    let old_version = adapter.bluetooth_version.split('.').next().and_then(|major| major.parse::<u32>().ok()).is_some_and(|major| major < 5);
    let old_driver = adapter.driver_year().filter(|year| *year + 3 <= this_year);
    if old_version || old_driver.is_some() {
        let what = match (old_version, old_driver) {
            (true, Some(year)) => format!(
                "{} {year}",
                tr(lang, "Eldre adapter (før Bluetooth 5) med driver fra", "Older adapter (before Bluetooth 5) with a driver from")
            ),
            (true, None) => tr(lang, "Eldre adapter (før Bluetooth 5)", "Older adapter (before Bluetooth 5)").to_string(),
            (false, Some(year)) => format!("{} {year}", tr(lang, "Driveren er fra", "The driver is from")),
            (false, None) => String::new(),
        };
        notes.push(format!(
            "{what}. {}",
            tr(
                lang,
                "Er forbindelsen ustabil, er dette det første å se på: prøv en nyere driver eller en annen adapter.",
                "If the connection is unstable this is the first thing to look at: try a newer driver or another adapter."
            )
        ));
    }
    notes
}

/// Frames per second for the last half minute, one bar per second.
fn rate_graph(ui: &mut egui::Ui, snap: &Snapshot) {
    const BAR: f32 = 9.0;
    const HEIGHT: f32 = 44.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(BAR * HISTORY_SECONDS as f32, HEIGHT), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);

    let top = snap.best_rate.max(snap.history.iter().map(|s| s.frames as f32).fold(1.0, f32::max));
    let offset = HISTORY_SECONDS - snap.history.len();
    for (i, second) in snap.history.iter().enumerate() {
        let height = (second.frames as f32 / top).clamp(0.0, 1.0) * (HEIGHT - 4.0);
        let x = rect.left() + (offset + i) as f32 * BAR;
        let bar = egui::Rect::from_min_max(
            egui::pos2(x + 1.0, rect.bottom() - 2.0 - height),
            egui::pos2(x + BAR - 1.0, rect.bottom() - 2.0),
        );
        // colour by the longest pause in that second
        let color = match second.max_gap_ms {
            0..=50 => GREEN,
            51..=120 => YELLOW,
            _ => RED,
        };
        painter.rect_filled(bar, 1.0, color.gamma_multiply(0.8));
    }
}

pub fn show(
    ui: &mut egui::Ui,
    lang: Lang,
    snap: &Snapshot,
    config: &mut Config,
    driver_status: DriverStatus,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        if ui.button(RichText::new(tr(lang, "Kopier diagnostikk", "Copy diagnostics")).strong()).clicked() {
            ui.ctx().copy_text(report(snap, driver_status));
        }
        ui.label(
            RichText::new(tr(
                lang,
                "Tekst du kan lime inn i en e-post eller feilrapport. Inneholder ingen Bluetooth-adresser.",
                "Text you can paste into an e-mail or bug report. Contains no Bluetooth addresses.",
            ))
            .weak(),
        );
    });
    ui.add_space(8.0);

    // ---- the link ----
    ui.heading(tr(lang, "Forbindelsen", "The connection"));
    match (snap.link, snap.quality) {
        (LinkState::Receiving | LinkState::SignalLost, Some(quality)) => {
            let text = format!(
                "{}: {:.0} Hz ({} {:.0}), {} {:.0} ms",
                capitalised(quality_word(lang, quality)),
                snap.frame_rate,
                tr(lang, "beste", "best"),
                snap.best_rate,
                tr(lang, "lengste opphold", "longest pause"),
                snap.max_gap_ms
            );
            status_line(ui, quality_color(quality), &text);
            // Bluetooth hands the frames over in bursts, one per connection
            // interval: the simulator only gets new values that often.
            if bursty(snap) {
                ui.label(format!(
                    "{} {:.0} {}",
                    tr(lang, "Rammene kommer i klumper: nye verdier", "The frames arrive in bursts: new values"),
                    snap.update_rate,
                    tr(lang, "ganger per sekund.", "times per second.")
                ));
            }
            let cause = pause_cause(snap.max_gap_ms, snap.connection.map(|connection| connection.interval_ms()));
            if let Some(advice) = quality_advice(lang, quality, cause) {
                ui.label(RichText::new(advice).color(quality_color(quality)));
            }
        }
        (LinkState::Receiving | LinkState::SignalLost | LinkState::Connected, None) => {
            status_line(ui, GRAY, tr(lang, "Måler …", "Measuring …"));
        }
        _ => status_line(ui, GRAY, tr(lang, "Ikke tilkoblet", "Not connected")),
    }
    if let Some(connection) = &snap.connection {
        ui.label(
            RichText::new(format!(
                "{}: {} {:.2} ms · PHY {} · {} {} ms",
                tr(lang, "Avtalt med radioen", "Agreed with the radio"),
                tr(lang, "intervall", "interval"),
                connection.interval_ms(),
                connection.phy_name(),
                tr(lang, "tidsavbrudd", "timeout"),
                connection.timeout_units as u32 * 10
            ))
            .weak(),
        );
    }
    if snap.quick_drops {
        ui.label(RichText::new(quick_drops_text(lang)).color(RED));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        rate_graph(ui, snap);
        ui.label(
            RichText::new(tr(
                lang,
                "Rammer per sekund, siste 30 s.\nFargen viser lengste opphold i sekundet:\ngrønn under 50 ms, gul under 120 ms, rød over.",
                "Frames per second, last 30 s.\nThe colour shows the longest pause in that second:\ngreen below 50 ms, yellow below 120 ms, red above.",
            ))
            .small()
            .weak(),
        );
    });

    ui.add_space(6.0);
    if ui
        .checkbox(
            &mut config.fast_connection,
            tr(
                lang,
                "Be Windows om raskest mulig forbindelse (eksperimentelt, gjelder fra neste tilkobling)",
                "Ask Windows for the fastest connection (experimental, applies from the next connection)",
            ),
        )
        .on_hover_text(tr(
            lang,
            "Kan gi høyere oppdateringsfrekvens. Slå av igjen hvis forbindelsen blir mindre stabil.",
            "May give a higher update rate. Switch it off again if the connection becomes less stable.",
        ))
        .changed()
    {
        // picked up by the app, which passes it on to the Bluetooth worker
    }

    // ---- the adapter ----
    ui.add_space(12.0);
    ui.heading(tr(lang, "Bluetooth-adapteren i PC-en", "The PC's Bluetooth adapter"));
    match &snap.adapter {
        Some(adapter) if adapter.found() => {
            let (first, driver) = adapter_lines(lang, adapter);
            ui.label(first);
            ui.label(RichText::new(driver).weak());
            let this_year = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() }.wYear as u32;
            for note in adapter_notes(lang, adapter, this_year) {
                ui.label(RichText::new(note).color(YELLOW));
            }
        }
        Some(_) => {
            ui.label(RichText::new(tr(lang, "Fant ingen Bluetooth-adapter.", "No Bluetooth adapter was found.")).color(RED));
        }
        None => {
            ui.label(RichText::new(tr(lang, "Leser …", "Reading …")).weak());
        }
    }
    if ui.small_button(tr(lang, "Les på nytt", "Read again")).clicked() {
        actions.push(Action::RefreshAdapter);
    }

    // ---- the log ----
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.heading(tr(lang, "Hendelser", "Events"));
        if ui.small_button(tr(lang, "Tøm", "Clear")).clicked() {
            actions.push(Action::ClearEvents);
        }
    });
    egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt("events")
            .max_height(230.0)
            .min_scrolled_height(120.0)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if snap.events.is_empty() {
                    ui.label(RichText::new(tr(lang, "Ingen hendelser ennå.", "No events yet.")).weak());
                }
                for event in &snap.events {
                    let (h, m, s) = event.time;
                    let color = match &event.kind {
                        EventKind::Disconnected { reason, .. } if *reason != DropReason::User => Some(RED),
                        EventKind::ConnectFailed(_) | EventKind::SignalLost => Some(YELLOW),
                        _ => None,
                    };
                    ui.horizontal_top(|ui| {
                        ui.label(RichText::new(format!("{h:02}:{m:02}:{s:02}")).monospace().weak());
                        let text = RichText::new(event_text(lang, event));
                        ui.add(egui::Label::new(match color {
                            Some(color) => text.color(color),
                            None => text,
                        })
                        .wrap());
                    });
                }
            });
    });
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The text behind "Copy diagnostics". Always in English (it is meant for
/// whoever maintains the program) and without Bluetooth addresses.
pub fn report(snap: &Snapshot, driver_status: DriverStatus) -> String {
    let lang = Lang::En;
    let mut out = String::new();
    let _ = writeln!(out, "Ethos BT Joystick {} - diagnostics", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "Windows build: {}", adapter::windows_build());

    let _ = writeln!(out);
    match &snap.adapter {
        Some(adapter) if adapter.found() => {
            let (first, driver) = adapter_lines(lang, adapter);
            let _ = writeln!(out, "Bluetooth adapter: {first}");
            let _ = writeln!(out, "{driver}");
            let _ = writeln!(
                out,
                "Low energy: {}, central role: {}, radio on: {}",
                yes_no(adapter.low_energy),
                yes_no(adapter.central_role),
                yes_no(adapter.radio_on)
            );
        }
        _ => {
            let _ = writeln!(out, "Bluetooth adapter: not found");
        }
    }

    let _ = writeln!(out);
    let joystick = match (driver_status, snap.driver_version) {
        (DriverStatus::Running, Some(version)) => format!("driver {}, running", format_driver_version(version)),
        (DriverStatus::Running, None) => "running, device not open".to_string(),
        (DriverStatus::Problem(code), _) => format!("installed but not started (problem code {code})"),
        (DriverStatus::NotInstalled, _) => "driver not installed".to_string(),
    };
    let _ = writeln!(out, "Virtual joystick: {joystick}");

    let _ = writeln!(out);
    let name = if snap.device_name.is_empty() { "-" } else { snap.device_name.as_str() };
    let _ = writeln!(out, "Radio: {name}");
    let _ = writeln!(out, "State: {:?}", snap.link);
    if let Some(error) = &snap.link_error {
        let _ = writeln!(out, "Last error: {}", link_error_text(lang, error));
    }
    if matches!(snap.link, LinkState::Receiving | LinkState::SignalLost) {
        let quality = snap.quality.map_or("measuring", |quality| quality_word(lang, quality));
        let _ = writeln!(
            out,
            "Link: {} channels, {:.0} Hz now, best {:.0} Hz, new data {:.0} times/s, longest pause {:.0} ms, quality {quality}",
            snap.channel_count, snap.frame_rate, snap.best_rate, snap.update_rate, snap.max_gap_ms
        );
    }
    if let Some(connection) = &snap.connection {
        let _ = writeln!(
            out,
            "Connection: interval {:.2} ms, latency {}, timeout {} ms, PHY {}",
            connection.interval_ms(),
            connection.latency,
            connection.timeout_units as u32 * 10,
            connection.phy_name()
        );
    }
    let _ = writeln!(out, "Frames: {} received, {} dropped", snap.frames_total, snap.bad_frames);
    if !snap.history.is_empty() {
        let rates: Vec<String> = snap.history.iter().map(|second| second.frames.to_string()).collect();
        let updates: Vec<String> = snap.history.iter().map(|second| second.updates.to_string()).collect();
        let pauses: Vec<String> = snap.history.iter().map(|second| second.max_gap_ms.to_string()).collect();
        let _ = writeln!(out, "Frames per second, oldest first: {}", rates.join(" "));
        let _ = writeln!(out, "New data per second:             {}", updates.join(" "));
        let _ = writeln!(out, "Longest pause per second (ms):   {}", pauses.join(" "));
    }
    if snap.quick_drops {
        let _ = writeln!(out, "Pattern: the link keeps dropping after a few seconds");
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "Events:");
    if snap.events.is_empty() {
        let _ = writeln!(out, "  (none)");
    }
    for event in &snap.events {
        let (h, m, s) = event.time;
        let _ = writeln!(out, "  {h:02}:{m:02}:{s:02}  {}", event_text(lang, event));
    }
    out
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::LinkError;
    use std::time::Instant;

    fn event(kind: EventKind) -> Event {
        Event { time: (9, 5, 7), at: Instant::now(), kind, repeats: 1 }
    }

    #[test]
    fn event_texts_follow_the_language() {
        let dropped = event(EventKind::Disconnected {
            reason: DropReason::NoData,
            lasted: Duration::from_secs(4),
            frames: 12,
        });
        assert_eq!(
            event_text(Lang::En, &dropped),
            "Disconnected after 4.0 s: no data for 5 s, the app closed the link (12 frames)"
        );
        assert!(event_text(Lang::No, &dropped).starts_with("Frakoblet etter 4.0 s: ingen data"));

        let mut failed = event(EventKind::ConnectFailed(LinkError::NotFound));
        failed.repeats = 3;
        assert!(event_text(Lang::En, &failed).ends_with("(×3)"));

        let parameters = event(EventKind::Parameters { interval_units: 10, latency: 0, timeout_ms: 2000 });
        assert_eq!(event_text(Lang::En, &parameters), "Interval 12.50 ms, latency 0, timeout 2000 ms");
    }

    #[test]
    fn durations() {
        assert_eq!(duration_text(Lang::En, Duration::from_millis(3400)), "3.4 s");
        assert_eq!(duration_text(Lang::En, Duration::from_secs(75)), "1 min 15 s");
        assert_eq!(duration_text(Lang::No, Duration::from_secs(3700)), "1 t 1 min");
    }

    #[test]
    fn old_adapters_get_a_note() {
        let old = AdapterInfo {
            name: "Realtek Bluetooth 4.2 Adapter".into(),
            bluetooth_version: "4.2".into(),
            driver_date: "2021-03-01".into(),
            low_energy: Some(true),
            central_role: Some(true),
            radio_on: Some(true),
            ..Default::default()
        };
        let notes = adapter_notes(Lang::En, &old, 2026);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("before Bluetooth 5") && notes[0].contains("2021"));

        let modern = AdapterInfo {
            bluetooth_version: "5.3".into(),
            driver_date: "2025-10-15".into(),
            low_energy: Some(true),
            central_role: Some(true),
            radio_on: Some(true),
            ..Default::default()
        };
        assert!(adapter_notes(Lang::En, &modern, 2026).is_empty());

        let off = AdapterInfo { radio_on: Some(false), ..modern };
        assert_eq!(adapter_notes(Lang::No, &off, 2026).len(), 1);
    }

    /// The copied report: complete, readable, and free of Bluetooth addresses.
    #[test]
    fn report_contents() {
        use crate::bridge::{Bridge, ConnectionInfo, LinkState};
        use crate::config::Mapping;
        use crate::protocol::{encode_frame, FrameDecoder};

        let bridge = Bridge::new(Mapping::default());
        bridge.set_adapter(AdapterInfo {
            name: "Test Adapter".into(),
            chip_maker: "Realtek".into(),
            bluetooth_version: "4.2".into(),
            driver_provider: "Realtek".into(),
            driver_version: "1.2.3.4".into(),
            driver_date: "2021-03-01".into(),
            low_energy: Some(true),
            central_role: Some(true),
            radio_on: Some(true),
        });
        bridge.log(EventKind::Target("X20RS".into()));
        bridge.begin_connection("X20RS");
        bridge.connection_established(850);
        bridge.set_link(LinkState::Connected, None);
        bridge.set_connection_info(ConnectionInfo { interval_units: 10, latency: 0, timeout_units: 200, phy: 1 });
        let mut decoder = FrameDecoder::new();
        decoder.push(&encode_frame(&[1500; 16]), |frame| bridge.on_frame(frame));
        bridge.connection_ended(DropReason::LinkLost);

        let text = report(&bridge.snapshot(), DriverStatus::Running);
        for expected in [
            "Ethos BT Joystick",
            "Windows build:",
            "Bluetooth adapter: Test Adapter (Realtek) · Bluetooth 4.2",
            "Driver: Realtek 1.2.3.4, 2021-03-01",
            "Radio: X20RS",
            "Connecting to X20RS",
            "Connected (set up in 850 ms)",
            "Interval 12.50 ms, latency 0, timeout 2000 ms",
            "First data after",
            "the Bluetooth link went down (1 frames)",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }

        // nothing that looks like a Bluetooth address (AA:BB:CC:DD:EE:FF)
        let bytes = text.as_bytes();
        let looks_like_address = bytes.windows(17).any(|window| {
            window.iter().enumerate().all(|(i, byte)| if i % 3 == 2 { *byte == b':' } else { byte.is_ascii_hexdigit() })
        });
        assert!(!looks_like_address, "address-like text in:\n{text}");
    }
}
