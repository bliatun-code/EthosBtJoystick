//! Link diagnostics: frame timing statistics, a plain-language quality verdict
//! and the connection event log.
//!
//! The radio produces about 100 trainer frames a second, but Bluetooth hands
//! them over in bursts, one per connection interval (44 ms on the link this
//! was calibrated on), and now and then an interval is lost. What the pilot
//! feels is how stale the stick positions are, so the verdict is built on the
//! *average age of the newest data*: at any moment, how long ago did the last
//! frame arrive, averaged over time. A single lost interval once a second
//! barely moves that average (and is not felt); losing every other interval
//! doubles or triples it (and is).
//!
//! Calibration, from real use:
//! * 44 ms interval, one interval lost per second, pauses of 85-130 ms:
//!   average age about 25 ms, and the pilot calls the response good.
//! * Antennas missing, 35-50 frames per second and unstable: too laggy to
//!   fly a helicopter. Losing a third to half of the intervals gives an
//!   average age of roughly 45-65 ms.

use std::time::{Duration, Instant};

use crate::bridge::LinkError;

/// Seconds of history kept for the rate graph.
pub const HISTORY_SECONDS: usize = 30;
/// The verdict looks at this many of the most recent complete seconds.
const WINDOW_SECONDS: usize = 3;
/// Maximum number of events kept in the log.
const MAX_EVENTS: usize = 200;

/// Frames that arrive less than this apart came in the same Bluetooth
/// connection event: the simulator only ever sees the last one of them.
const BURST_GAP: Duration = Duration::from_millis(4);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Second {
    pub frames: u16,
    /// Longest pause between two frames that ended in this second.
    pub max_gap_ms: u16,
    /// How many times new data arrived. Bluetooth delivers frames in bursts,
    /// one burst per connection interval, so this can be far below `frames`.
    pub updates: u16,
    /// Sum of the squared pauses (ms²) that ended in this second; gives the
    /// average age of the data, see [`average_age_ms`].
    pub gap_squares: u32,
}

impl Second {
    /// Average age of the newest data during this second.
    pub fn age_ms(&self) -> f32 {
        average_age_ms(self.gap_squares as f32, 1000.0)
    }
}

/// Average age of the newest data over a stretch of time: if the pauses
/// between arrivals are g1, g2 ..., the data is on average
/// (g1² + g2² + ...) / (2 × total time) old. Evenly spaced frames 10 ms apart
/// give 5 ms; bursts every 44 ms give 22 ms.
pub fn average_age_ms(gap_squares: f32, total_ms: f32) -> f32 {
    if total_ms > 0.0 {
        gap_squares / (2.0 * total_ms)
    } else {
        0.0
    }
}

/// Why the pauses between frames are as long as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseCause {
    /// Lost packets: weak signal, distance, missing antennas.
    Signal,
    /// The connection interval itself is long, so every lost interval costs a
    /// long pause. A shorter interval is the cure, not a stronger signal.
    LongInterval,
}

/// Decides which advice fits. With a long connection interval (25 ms or more)
/// a pause of up to about two intervals is what a single lost interval gives.
pub fn pause_cause(max_gap_ms: f32, interval_ms: Option<f32>) -> PauseCause {
    match interval_ms {
        Some(interval) if interval >= 25.0 && max_gap_ms <= interval * 2.6 => PauseCause::LongInterval,
        _ => PauseCause::Signal,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkQuality {
    Good,
    /// Usable, but the response is noticeably slower than it could be.
    Weak,
    /// So much data is late or missing that control feels laggy.
    Poor,
}

/// Average age up to which the response is good / still usable (see the
/// calibration notes at the top of this file).
pub const GOOD_AGE_MS: f32 = 35.0;
pub const WEAK_AGE_MS: f32 = 55.0;
/// A single pause this long is a hiccup worth flagging, whatever the average.
const STALL_MS: f32 = 400.0;
const LONG_STALL_MS: f32 = 1000.0;

/// The verdict: mainly the average age of the data, plus outright stalls.
pub fn classify(age_ms: f32, max_gap_ms: f32) -> LinkQuality {
    if age_ms <= GOOD_AGE_MS && max_gap_ms <= STALL_MS {
        LinkQuality::Good
    } else if age_ms <= WEAK_AGE_MS && max_gap_ms <= LONG_STALL_MS {
        LinkQuality::Weak
    } else {
        LinkQuality::Poor
    }
}

#[derive(Clone, Debug)]
pub struct Summary {
    /// Complete seconds, oldest first (the current, incomplete second is not included).
    pub history: Vec<Second>,
    /// Frames per second, averaged over the last few complete seconds.
    pub rate: f32,
    /// Times per second that new data arrived (see [`Second::updates`]).
    pub update_rate: f32,
    /// Best single second of this connection.
    pub best_rate: f32,
    /// Average age of the newest data over the same seconds (what the verdict
    /// is based on).
    pub age_ms: f32,
    /// Longest pause in the same window, including one that is still going on.
    pub max_gap_ms: f32,
    /// `None` until a full second has been measured.
    pub quality: Option<LinkQuality>,
}

/// Per-second frame statistics for one connection.
pub struct FrameStats {
    start: Instant,
    /// Index (seconds since `start`) of the bucket currently being filled.
    current: u64,
    buckets: [Second; HISTORY_SECONDS + 1],
    last_frame: Option<Instant>,
    best_rate: u16,
}

impl FrameStats {
    pub fn new(now: Instant) -> Self {
        Self { start: now, current: 0, buckets: [Second::default(); HISTORY_SECONDS + 1], last_frame: None, best_rate: 0 }
    }

    fn bucket(&mut self, second: u64) -> &mut Second {
        &mut self.buckets[(second % (HISTORY_SECONDS as u64 + 1)) as usize]
    }

    /// Moves on to the second `now` falls in, closing the ones before it.
    fn advance(&mut self, now: Instant) {
        let second = now.saturating_duration_since(self.start).as_secs();
        while self.current < second {
            let finished = *self.bucket(self.current);
            self.best_rate = self.best_rate.max(finished.frames);
            self.current += 1;
            *self.bucket(self.current) = Second::default();
        }
    }

    pub fn on_frame(&mut self, now: Instant) {
        self.advance(now);
        let since_last = self.last_frame.map(|last| now.saturating_duration_since(last));
        let gap = since_last.map_or(0, |gap| gap.as_millis().min(u16::MAX as u128) as u16);
        // the first frame, or one that did not arrive together with the previous one
        let new_update = since_last.map_or(true, |gap| gap >= BURST_GAP);
        self.last_frame = Some(now);
        let bucket = self.bucket(self.current);
        bucket.frames = bucket.frames.saturating_add(1);
        bucket.max_gap_ms = bucket.max_gap_ms.max(gap);
        bucket.gap_squares = bucket.gap_squares.saturating_add(gap as u32 * gap as u32);
        if new_update {
            bucket.updates = bucket.updates.saturating_add(1);
        }
    }

    pub fn summary(&mut self, now: Instant) -> Summary {
        self.advance(now);

        let complete = (self.current as usize).min(HISTORY_SECONDS);
        let history: Vec<Second> =
            (0..complete).map(|i| *self.bucket(self.current - (complete - i) as u64)).collect();

        let window = &history[history.len().saturating_sub(WINDOW_SECONDS)..];
        let average = |value: fn(&Second) -> u16| {
            if window.is_empty() {
                0.0
            } else {
                window.iter().map(|second| value(second) as f32).sum::<f32>() / window.len() as f32
            }
        };
        let rate = average(|second| second.frames);
        let update_rate = average(|second| second.updates);

        // pauses: the window, the second being filled, and a pause still in progress
        let ongoing = self.last_frame.map_or(0, |last| now.saturating_duration_since(last).as_millis());
        let max_gap_ms = window
            .iter()
            .map(|s| s.max_gap_ms as u128)
            .chain([self.bucket(self.current).max_gap_ms as u128, ongoing])
            .max()
            .unwrap_or(0) as f32;

        // average age of the data over the window, counting a pause in progress
        let ongoing_ms = ongoing as f32;
        let gap_squares = window.iter().map(|second| second.gap_squares as f32).sum::<f32>() + ongoing_ms * ongoing_ms;
        let age_ms = average_age_ms(gap_squares, window.len() as f32 * 1000.0 + ongoing_ms);

        let best_rate = self.best_rate as f32;
        let quality = (!window.is_empty() && self.last_frame.is_some()).then(|| classify(age_ms, max_gap_ms));
        Summary { history, rate, update_rate, best_rate, age_ms, max_gap_ms, quality }
    }
}

// ---------------------------------------------------------------------------
// Event log
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// Windows reported that the Bluetooth link went down.
    LinkLost,
    /// The link was up, but no trainer frame arrived for several seconds, so
    /// the app closed it to start over.
    NoData,
    /// The user clicked Disconnect (or picked another radio / closed the app).
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// The user asked to connect to this radio.
    Target(String),
    /// A connection attempt failed.
    ConnectFailed(LinkError),
    /// Connected and subscribed to the trainer data.
    Connected { setup_ms: u32 },
    /// The first trainer frame arrived.
    FirstData { after_ms: u32, channels: u8 },
    /// Frames stopped for more than half a second.
    SignalLost,
    /// Frames came back after `gap_ms`.
    SignalBack { gap_ms: u32 },
    Disconnected { reason: DropReason, lasted: Duration, frames: u64 },
    /// What Windows and the radio agreed on (interval in units of 1.25 ms).
    Parameters { interval_units: u16, latency: u16, timeout_ms: u32 },
    /// Result of asking Windows for the fastest connection interval.
    FastRequested { accepted: bool },
}

#[derive(Clone, Debug)]
pub struct Event {
    /// Local wall-clock time (hour, minute, second).
    pub time: (u8, u8, u8),
    pub at: Instant,
    pub kind: EventKind,
    /// How many times this same event happened in a row (failed attempts repeat).
    pub repeats: u32,
}

#[derive(Default)]
pub struct EventLog {
    events: Vec<Event>,
}

impl EventLog {
    pub fn push(&mut self, time: (u8, u8, u8), at: Instant, kind: EventKind) {
        if let Some(last) = self.events.last_mut() {
            if last.kind == kind && matches!(kind, EventKind::ConnectFailed(_)) {
                last.repeats += 1;
                last.time = time;
                last.at = at;
                return;
            }
        }
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(Event { time, at, kind, repeats: 1 });
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// True if the link has dropped several times in a short while, each time
/// after only a few seconds - the pattern of a Bluetooth adapter or driver
/// that cannot keep this kind of connection up.
pub fn repeated_quick_drops(events: &[Event], now: Instant) -> bool {
    const WITHIN: Duration = Duration::from_secs(180);
    const QUICK: Duration = Duration::from_secs(20);
    events
        .iter()
        .filter(|event| now.saturating_duration_since(event.at) <= WITHIN)
        .filter(|event| {
            matches!(&event.kind, EventKind::Disconnected { reason, lasted, .. }
                if *reason != DropReason::User && *lasted <= QUICK)
        })
        .count()
        >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds frames at a fixed interval from `from` to `to` (milliseconds).
    fn feed(stats: &mut FrameStats, start: Instant, from: u64, to: u64, interval: u64) {
        let mut t = from;
        while t < to {
            stats.on_frame(start + Duration::from_millis(t));
            t += interval;
        }
    }

    #[test]
    fn steady_link_is_good() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        feed(&mut stats, start, 0, 5000, 10); // 100 Hz for 5 s
        let summary = stats.summary(start + Duration::from_millis(5005));
        assert_eq!(summary.history.len(), 5);
        assert!((summary.rate - 100.0).abs() < 1.5, "rate {}", summary.rate);
        assert!(summary.max_gap_ms <= 15.0);
        assert_eq!(summary.quality, Some(LinkQuality::Good));
    }

    #[test]
    fn nothing_is_judged_before_a_full_second() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        feed(&mut stats, start, 0, 500, 10);
        assert_eq!(stats.summary(start + Duration::from_millis(500)).quality, None);
    }

    #[test]
    fn a_slower_but_steady_radio_is_still_good() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        feed(&mut stats, start, 0, 5000, 20); // 50 Hz, perfectly regular
        let summary = stats.summary(start + Duration::from_millis(5005));
        assert_eq!(summary.quality, Some(LinkQuality::Good));
    }

    /// Feeds bursts of four frames on a 43.75 ms connection interval from
    /// `from_ms` to `to_ms`, dropping the intervals for which `lost` is true.
    fn feed_bursts(stats: &mut FrameStats, start: Instant, from_ms: u64, to_ms: u64, lost: impl Fn(u64) -> bool) {
        const INTERVAL_US: u64 = 43_750;
        let mut event = from_ms * 1000 / INTERVAL_US + 1;
        while event * INTERVAL_US < to_ms * 1000 {
            if !lost(event) {
                for frame in 0..4 {
                    stats.on_frame(start + Duration::from_micros(event * INTERVAL_US + frame * 300));
                }
            }
            event += 1;
        }
    }

    /// Losing a third to half of the connection intervals is what a weak
    /// signal does - and what was too laggy to fly with.
    #[test]
    fn lost_intervals_degrade_the_verdict() {
        let start = Instant::now();

        // every third interval lost: about 60 frames per second
        let mut stats = FrameStats::new(start);
        feed_bursts(&mut stats, start, 0, 6000, |event| event % 3 == 0);
        let summary = stats.summary(start + Duration::from_millis(6001));
        assert!((55.0..=65.0).contains(&summary.rate), "rate {}", summary.rate);
        assert!((GOOD_AGE_MS..=WEAK_AGE_MS).contains(&summary.age_ms), "age {}", summary.age_ms);
        assert_eq!(summary.quality, Some(LinkQuality::Weak));

        // two of three lost: about 30 frames per second
        let mut stats = FrameStats::new(start);
        feed_bursts(&mut stats, start, 0, 6000, |event| event % 3 != 0);
        let summary = stats.summary(start + Duration::from_millis(6001));
        assert!((27.0..=34.0).contains(&summary.rate), "rate {}", summary.rate);
        assert!(summary.age_ms > WEAK_AGE_MS, "age {}", summary.age_ms);
        assert_eq!(summary.quality, Some(LinkQuality::Poor));
    }

    #[test]
    fn a_pause_in_progress_counts() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        feed(&mut stats, start, 0, 3000, 10);
        // frames stop at 3.0 s; a stall of half a second is flagged, 1.5 s is poor
        let summary = stats.summary(start + Duration::from_millis(3500));
        assert!(summary.max_gap_ms >= 490.0);
        assert_eq!(summary.quality, Some(LinkQuality::Weak));
        let summary = stats.summary(start + Duration::from_millis(4500));
        assert_eq!(summary.quality, Some(LinkQuality::Poor));
    }

    #[test]
    fn average_age_examples() {
        // evenly spaced frames 10 ms apart: 100 pauses of 10 ms in a second
        assert_eq!(average_age_ms(100.0 * 10.0 * 10.0, 1000.0), 5.0);
        // one burst every 43.75 ms
        let per_second = 1000.0 / 43.75;
        assert!((average_age_ms(per_second * 43.75 * 43.75, 1000.0) - 21.875).abs() < 0.01);
    }

    #[test]
    fn history_is_limited_and_ordered() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        // second n gets n+1 frames
        for second in 0..40u64 {
            for frame in 0..=second {
                stats.on_frame(start + Duration::from_millis(second * 1000 + frame * 10));
            }
        }
        let summary = stats.summary(start + Duration::from_millis(40_000));
        assert_eq!(summary.history.len(), HISTORY_SECONDS);
        assert_eq!(summary.history.first().unwrap().frames, 11); // second 10
        assert_eq!(summary.history.last().unwrap().frames, 40); // second 39
        assert_eq!(summary.best_rate, 40.0);
    }

    /// The pattern measured on a real link that the pilot calls good: a
    /// 43.75 ms connection interval, four frames arriving together every
    /// interval (91 frames per second, new data 23 times per second), and
    /// about once a second one interval - sometimes two - is lost.
    #[test]
    fn the_measured_good_link_is_good() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        // one lost interval per second (85 ms pauses) ...
        feed_bursts(&mut stats, start, 0, 6000, |event| event % 23 == 22);
        let summary = stats.summary(start + Duration::from_millis(6001));
        assert!((85.0..=93.0).contains(&summary.rate), "rate {}", summary.rate);
        assert!((20.0..=24.0).contains(&summary.update_rate), "updates {}", summary.update_rate);
        assert!((85.0..=90.0).contains(&summary.max_gap_ms), "gap {}", summary.max_gap_ms);
        assert!((22.0..=27.0).contains(&summary.age_ms), "age {}", summary.age_ms);
        assert_eq!(summary.quality, Some(LinkQuality::Good));

        // ... or two in a row (130 ms pauses): still good
        let mut stats = FrameStats::new(start);
        feed_bursts(&mut stats, start, 0, 6000, |event| event % 23 >= 21);
        let summary = stats.summary(start + Duration::from_millis(6001));
        assert!((125.0..=135.0).contains(&summary.max_gap_ms), "gap {}", summary.max_gap_ms);
        assert!(summary.age_ms <= GOOD_AGE_MS, "age {}", summary.age_ms);
        assert_eq!(summary.quality, Some(LinkQuality::Good));
    }

    #[test]
    fn pauses_are_attributed_to_the_right_cause() {
        // up to about two intervals: what a lost interval costs on a long interval
        assert_eq!(pause_cause(87.0, Some(43.75)), PauseCause::LongInterval);
        assert_eq!(pause_cause(87.0, Some(13.75)), PauseCause::Signal);
        assert_eq!(pause_cause(87.0, None), PauseCause::Signal);
        assert_eq!(pause_cause(300.0, Some(43.75)), PauseCause::Signal);
    }

    #[test]
    fn evenly_spaced_frames_are_one_update_each() {
        let start = Instant::now();
        let mut stats = FrameStats::new(start);
        feed(&mut stats, start, 0, 4000, 10);
        let summary = stats.summary(start + Duration::from_millis(4001));
        assert!((summary.update_rate - summary.rate).abs() < 1.0);
    }

    #[test]
    fn classify_thresholds() {
        assert_eq!(classify(5.0, 12.0), LinkQuality::Good);
        assert_eq!(classify(25.0, 130.0), LinkQuality::Good);
        assert_eq!(classify(45.0, 130.0), LinkQuality::Weak);
        assert_eq!(classify(25.0, 600.0), LinkQuality::Weak); // a stall
        assert_eq!(classify(70.0, 200.0), LinkQuality::Poor);
        assert_eq!(classify(25.0, 1500.0), LinkQuality::Poor); // a long stall
    }

    #[test]
    fn repeated_failures_are_folded_and_the_log_is_bounded() {
        let now = Instant::now();
        let mut log = EventLog::default();
        for _ in 0..5 {
            log.push((1, 2, 3), now, EventKind::ConnectFailed(LinkError::NotFound));
        }
        assert_eq!(log.events().len(), 1);
        assert_eq!(log.events()[0].repeats, 5);

        for i in 0..(MAX_EVENTS + 50) {
            log.push((0, 0, 0), now, EventKind::SignalBack { gap_ms: i as u32 });
        }
        assert_eq!(log.events().len(), MAX_EVENTS);
    }

    #[test]
    fn quick_drop_pattern() {
        let now = Instant::now();
        let drop = |reason: DropReason, lasted_s: u64| EventKind::Disconnected {
            reason,
            lasted: Duration::from_secs(lasted_s),
            frames: 100,
        };
        let mut log = EventLog::default();
        log.push((0, 0, 0), now, drop(DropReason::LinkLost, 4));
        log.push((0, 0, 1), now, drop(DropReason::NoData, 6));
        assert!(!repeated_quick_drops(log.events(), now));
        log.push((0, 0, 2), now, drop(DropReason::LinkLost, 3));
        assert!(repeated_quick_drops(log.events(), now));

        // long sessions and user disconnects do not count
        let mut log = EventLog::default();
        for _ in 0..3 {
            log.push((0, 0, 0), now, drop(DropReason::LinkLost, 600));
            log.push((0, 0, 0), now, drop(DropReason::User, 2));
        }
        assert!(!repeated_quick_drops(log.events(), now));
    }
}
