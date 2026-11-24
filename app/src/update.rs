//! Looks for a newer version of the program.
//!
//! One HTTPS request asks GitHub for the latest published release of this
//! project. Nothing is downloaded or installed: the window only says that a
//! newer version exists and offers to open the download page in the browser.
//!
//! The only thing taken from the answer is the three numbers of the version.
//! The page that is opened is a fixed address, never one from the answer.

use std::ffi::c_void;
use std::fmt;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_DISABLE_COOKIES, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_DISABLE_FEATURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::util::wide;

/// Where the question goes: GitHub's description of the latest release.
const HOST: PCWSTR = w!("api.github.com");
const PATH: PCWSTR = w!("/repos/bliatun-code/EthosBtJoystick/releases/latest");
/// GitHub refuses requests that do not say who is asking.
const AGENT: PCWSTR = w!("EthosBtJoystick");

/// The page with the newest release, opened by the "Download" button.
pub const DOWNLOAD_PAGE: &str = "https://github.com/bliatun-code/EthosBtJoystick/releases/latest";

/// The description of a release is a few kilobytes; more than this is not one.
const MAX_ANSWER: usize = 1024 * 1024;

/// The first automatic check comes a little after start, when the program
/// has finished starting up.
const FIRST_CHECK: Duration = Duration::from_secs(5);
const ONE_DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// A version number such as 1.2.0. Stored as text ("1.2.0") in the settings.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// The version of this program.
    pub fn current() -> Self {
        let number = |text: &str| text.parse().unwrap_or(0);
        Self {
            major: number(env!("CARGO_PKG_VERSION_MAJOR")),
            minor: number(env!("CARGO_PKG_VERSION_MINOR")),
            patch: number(env!("CARGO_PKG_VERSION_PATCH")),
        }
    }

    /// `1.2.0` or `v1.2.0`. Anything else, pre-releases such as `1.3.0-beta`
    /// included, is not a version to tell the user about.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let mut parts = text.split('.');
        let mut number = || {
            let part = parts.next()?;
            // digits only (no sign, no spaces), and few enough not to overflow
            if part.is_empty() || part.len() > 6 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse::<u32>().ok()
        };
        let version = Self { major: number()?, minor: number()?, patch: number()? };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl TryFrom<String> for Version {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text).ok_or_else(|| format!("not a version number: {text}"))
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.to_string()
    }
}

/// The part of GitHub's answer that is used.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The version in GitHub's description of a release (`"tag_name": "v1.2.0"`).
pub fn version_in_release(json: &str) -> Option<Version> {
    let release: Release = serde_json::from_str(json).ok()?;
    if release.draft || release.prerelease {
        return None;
    }
    Version::parse(&release.tag_name)
}

/// Why a check gave no version.
#[derive(Debug)]
pub enum CheckError {
    /// The request did not get through (no network, no answer in time ...).
    Network(windows::core::Error),
    /// GitHub answered with something other than "200 OK".
    Status(u32),
    /// The answer was not the description of a release with a version number.
    Answer,
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckError::Network(error) => write!(f, "the request failed: {error}"),
            CheckError::Status(status) => write!(f, "GitHub answered with HTTP status {status}"),
            CheckError::Answer => write!(f, "the answer did not contain a version number"),
        }
    }
}

impl From<windows::core::Error> for CheckError {
    fn from(error: windows::core::Error) -> Self {
        CheckError::Network(error)
    }
}

/// Asks GitHub for the newest published version of the program.
pub fn fetch_latest() -> Result<Version, CheckError> {
    let answer = http_get()?;
    std::str::from_utf8(&answer).ok().and_then(version_in_release).ok_or(CheckError::Answer)
}

/// A WinHTTP handle that is closed when it goes out of scope.
struct Handle(*mut c_void);

impl Handle {
    fn new(handle: *mut c_void) -> windows::core::Result<Self> {
        if handle.is_null() {
            Err(windows::core::Error::from_win32())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// `GET https://api.github.com/repos/.../releases/latest`, through the proxy
/// Windows is set up with, if any.
fn http_get() -> Result<Vec<u8>, CheckError> {
    unsafe {
        let session =
            Handle::new(WinHttpOpen(AGENT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
        // looking up the name, connecting, sending and each wait for data:
        // none of them may hang for long
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 10_000, 15_000)?;
        let connection = Handle::new(WinHttpConnect(session.0, HOST, 443, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            PATH,
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        // nothing is remembered from one request to the next
        WinHttpSetOption(
            Some(request.0.cast_const()),
            WINHTTP_OPTION_DISABLE_FEATURE,
            Some(&WINHTTP_DISABLE_COOKIES.to_ne_bytes()),
        )?;

        let headers: Vec<u16> =
            "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28".encode_utf16().collect();
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )?;
        if status != 200 {
            return Err(CheckError::Status(status));
        }

        let mut answer = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32, &mut read)?;
            if read == 0 {
                return Ok(answer);
            }
            answer.extend_from_slice(&chunk[..read as usize]);
            if answer.len() > MAX_ANSWER {
                return Err(CheckError::Answer);
            }
        }
    }
}

/// Opens the page with the newest release in the user's browser.
pub fn open_download_page() {
    let page = wide(DOWNLOAD_PAGE);
    unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(page.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// What is known about new versions right now.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct UpdateState {
    /// A check is running.
    pub checking: bool,
    /// The last check gave no answer.
    pub failed: bool,
    /// The newest published version, once a check has succeeded.
    pub latest: Option<Version>,
}

impl UpdateState {
    /// A published version that is newer than this program.
    pub fn newer(&self) -> Option<Version> {
        self.newer_than(Version::current())
    }

    fn newer_than(&self, current: Version) -> Option<Version> {
        self.latest.filter(|latest| *latest > current)
    }
}

/// How long to wait before the next automatic check.
fn next_check(succeeded: bool, failures_in_a_row: u32) -> Duration {
    match (succeeded, failures_in_a_row) {
        (true, _) => ONE_DAY,
        // right after sign-in the network may not be up yet: try again soon
        (false, 0 | 1) => Duration::from_secs(5 * 60),
        (false, _) => Duration::from_secs(60 * 60),
    }
}

enum Command {
    CheckNow,
    Automatic(bool),
}

type Source = Box<dyn Fn() -> Result<Version, CheckError> + Send>;

/// Checks for new versions on a thread of its own.
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
    commands: Sender<Command>,
}

impl Updater {
    /// With `automatic`, GitHub is asked shortly after start and then once a
    /// day; without it only when [`Updater::check_now`] is called. `changed`
    /// is called (on the worker thread) whenever the state changes.
    pub fn spawn(automatic: bool, changed: impl Fn() + Send + 'static) -> Self {
        Self::spawn_with(Box::new(fetch_latest), automatic, FIRST_CHECK, Box::new(changed))
    }

    /// For the demo (`--demo-update`): "finds" the next version after this
    /// one without asking anyone.
    pub fn pretend(changed: impl Fn() + Send + 'static) -> Self {
        let source = || {
            std::thread::sleep(Duration::from_millis(800));
            let current = Version::current();
            Ok(Version { minor: current.minor + 1, patch: 0, ..current })
        };
        Self::spawn_with(Box::new(source), true, Duration::from_secs(2), Box::new(changed))
    }

    fn spawn_with(source: Source, automatic: bool, first_check: Duration, changed: Box<dyn Fn() + Send>) -> Self {
        let state = Arc::new(Mutex::new(UpdateState::default()));
        let (commands, inbox) = mpsc::channel();
        let shared = state.clone();
        let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
            let set = |change: &dyn Fn(&mut UpdateState)| {
                change(&mut shared.lock().unwrap());
                changed();
            };
            let mut automatic = automatic;
            let mut failures = 0u32;
            let mut due = Instant::now() + first_check;
            loop {
                // wait for the next automatic check or for a command; the
                // thread ends when the `Updater` is gone
                let command = if automatic {
                    match inbox.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(command) => Some(command),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                } else {
                    match inbox.recv() {
                        Ok(command) => Some(command),
                        Err(_) => return,
                    }
                };
                if let Some(Command::Automatic(on)) = command {
                    // `due` stays as it was: switching the setting off and on
                    // again does not ask GitHub more often
                    automatic = on;
                    continue;
                }

                set(&|state| state.checking = true);
                let result = source();
                failures = if result.is_ok() { 0 } else { failures + 1 };
                set(&|state| {
                    state.checking = false;
                    state.failed = result.is_err();
                    if let Ok(version) = &result {
                        state.latest = Some(*version);
                    }
                });
                due = Instant::now() + next_check(result.is_ok(), failures);
            }
        });
        Self { state, commands }
    }

    pub fn state(&self) -> UpdateState {
        *self.state.lock().unwrap()
    }

    /// Asks GitHub now, whatever the automatic setting is.
    pub fn check_now(&self) {
        let _ = self.commands.send(Command::CheckNow);
    }

    pub fn set_automatic(&self, on: bool) {
        let _ = self.commands.send(Command::Automatic(on));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc::Receiver;

    fn version(major: u32, minor: u32, patch: u32) -> Version {
        Version { major, minor, patch }
    }

    #[test]
    fn version_numbers() {
        assert_eq!(Version::parse("1.2.0"), Some(version(1, 2, 0)));
        assert_eq!(Version::parse("v1.2.0"), Some(version(1, 2, 0)));
        assert_eq!(Version::parse("V10.20.30"), Some(version(10, 20, 30)));
        assert_eq!(Version::parse("0.0.1"), Some(version(0, 0, 1)));

        for text in [
            "", "v", "1", "1.2", "1.2.0.1", "1..0", "1.2.", ".1.2", "1.2.x", " 1.2.0", "1.2.0 ", "1.2.0-beta", "+1.2.3",
            "-1.2.3", "vv1.2.3", "1234567.0.0", "1.2.٣",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(version(1, 10, 0) > version(1, 9, 9));
        assert!(version(2, 0, 0) > version(1, 99, 99));
        assert!(version(1, 2, 1) > version(1, 2, 0));
        assert_eq!(version(1, 2, 0).to_string(), "1.2.0");
    }

    #[test]
    fn this_program_has_a_version() {
        assert_eq!(Some(Version::current()), Version::parse(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn version_is_stored_as_text() {
        assert_eq!(serde_json::to_string(&version(1, 3, 0)).unwrap(), r#""1.3.0""#);
        assert_eq!(serde_json::from_str::<Version>(r#""1.3.0""#).unwrap(), version(1, 3, 0));
        assert!(serde_json::from_str::<Version>(r#""soon""#).is_err());
    }

    #[test]
    fn version_from_githubs_answer() {
        // the fields GitHub sends that matter here, and a few that do not
        let answer = r#"{
            "url": "https://api.github.com/repos/bliatun-code/EthosBtJoystick/releases/1",
            "html_url": "https://github.com/bliatun-code/EthosBtJoystick/releases/tag/v1.2.0",
            "id": 1,
            "tag_name": "v1.2.0",
            "name": "Ethos BT Joystick 1.2.0",
            "draft": false,
            "prerelease": false,
            "assets": [{ "name": "EthosBtJoystick-v1.2.0-win-x64.zip", "size": 1 }],
            "body": "Adds an installer."
        }"#;
        assert_eq!(version_in_release(answer), Some(version(1, 2, 0)));

        assert_eq!(version_in_release(r#"{"tag_name": "v1.3.0"}"#), Some(version(1, 3, 0)));
        assert_eq!(version_in_release(r#"{"tag_name": "v1.3.0", "prerelease": true}"#), None);
        assert_eq!(version_in_release(r#"{"tag_name": "v1.3.0", "draft": true}"#), None);
        assert_eq!(version_in_release(r#"{"tag_name": "nightly"}"#), None);
        assert_eq!(version_in_release(r#"{"tag_name": 5}"#), None);
        assert_eq!(version_in_release(r#"{"message": "Not Found"}"#), None);
        assert_eq!(version_in_release("<html>sign in to the hotel network</html>"), None);
        assert_eq!(version_in_release(""), None);
    }

    #[test]
    fn only_a_higher_version_is_news() {
        let known = |latest| UpdateState { latest, ..Default::default() };
        let current = version(1, 2, 0);
        assert_eq!(known(None).newer_than(current), None);
        assert_eq!(known(Some(version(1, 2, 0))).newer_than(current), None);
        assert_eq!(known(Some(version(1, 1, 9))).newer_than(current), None);
        assert_eq!(known(Some(version(1, 2, 1))).newer_than(current), Some(version(1, 2, 1)));
    }

    #[test]
    fn failed_checks_are_tried_again_sooner() {
        assert_eq!(next_check(true, 0), ONE_DAY);
        assert!(next_check(false, 1) < next_check(false, 2));
        assert!(next_check(false, 5) < ONE_DAY);
    }

    /// An updater whose "GitHub" gives the answers in `answers`, one per
    /// check, and a channel that gets a message for every change of state.
    fn updater(automatic: bool, answers: Vec<Option<Version>>) -> (Updater, Receiver<()>, Arc<AtomicU32>) {
        let asked = Arc::new(AtomicU32::new(0));
        let counter = asked.clone();
        let source = move || {
            let number = counter.fetch_add(1, Ordering::SeqCst) as usize;
            answers.get(number).copied().flatten().ok_or(CheckError::Status(503))
        };
        let (changes, changed) = mpsc::channel();
        let updater = Updater::spawn_with(
            Box::new(source),
            automatic,
            Duration::ZERO,
            Box::new(move || {
                let _ = changes.send(());
            }),
        );
        (updater, changed, asked)
    }

    /// Waits until a check has started and finished.
    fn one_check(changed: &Receiver<()>) {
        for _ in 0..2 {
            changed.recv_timeout(Duration::from_secs(5)).expect("the state changes");
        }
    }

    #[test]
    fn automatic_check_at_start() {
        let (updater, changed, asked) = updater(true, vec![Some(version(9, 0, 0))]);
        one_check(&changed);
        assert_eq!(updater.state(), UpdateState { checking: false, failed: false, latest: Some(version(9, 0, 0)) });
        assert_eq!(updater.state().newer(), Some(version(9, 0, 0)));
        // the next one is a day away
        assert!(changed.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn nothing_is_asked_while_switched_off() {
        let (updater, changed, asked) = updater(false, vec![Some(version(0, 0, 1)), None]);
        assert!(changed.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert_eq!(updater.state(), UpdateState::default());

        // "Check now" works all the same
        updater.check_now();
        one_check(&changed);
        assert_eq!(updater.state(), UpdateState { checking: false, failed: false, latest: Some(version(0, 0, 1)) });
        assert_eq!(updater.state().newer(), None);

        // a check that fails says so, and keeps what was known
        updater.check_now();
        one_check(&changed);
        assert_eq!(updater.state(), UpdateState { checking: false, failed: true, latest: Some(version(0, 0, 1)) });
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn switching_it_on_checks_once() {
        let (updater, changed, asked) = updater(false, vec![Some(version(9, 0, 0)); 3]);
        updater.set_automatic(true);
        one_check(&changed);
        // off and on again does not ask again
        updater.set_automatic(false);
        updater.set_automatic(true);
        assert!(changed.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }
}
