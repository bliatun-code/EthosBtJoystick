//! Settings (stored as JSON in %APPDATA%\EthosBtJoystick\config.json) and the
//! channel -> joystick mapping.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::joystick::{JoyState, AXIS_COUNT, AXIS_MAX, BUTTON_COUNT};

/// Pulse width range sent by the radio (µs).
pub const US_MIN: u16 = 988;
pub const US_MAX: u16 = 2012;
pub const US_CENTER: u16 = 1500;

/// A 3-position switch: low <= 1300, middle 1301..1699, high >= 1700.
pub const SWITCH_LOW_MAX: u16 = 1300;
pub const SWITCH_HIGH_MIN: u16 = 1700;

/// How many radio channels can be selected as a source.
pub const SELECTABLE_CHANNELS: u8 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Lang {
    No,
    En,
}

impl Lang {
    /// Norwegian if the Windows display language *or* the regional format is
    /// Norwegian (many Norwegian users run Windows in English), otherwise English.
    pub fn system_default() -> Self {
        use windows::Win32::Globalization::{GetUserDefaultLCID, GetUserDefaultUILanguage};

        // primary language id 0x14 = Norwegian (bokmål and nynorsk)
        const NORWEGIAN: u32 = 0x14;
        let ui_language = unsafe { GetUserDefaultUILanguage() } as u32;
        let region = unsafe { GetUserDefaultLCID() };
        if ui_language & 0x3FF == NORWEGIAN || region & 0x3FF == NORWEGIAN {
            Lang::No
        } else {
            Lang::En
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SwitchPos {
    Low,
    Mid,
    High,
}

impl SwitchPos {
    pub fn matches(self, us: u16) -> bool {
        match self {
            SwitchPos::Low => us <= SWITCH_LOW_MAX,
            SwitchPos::Mid => us > SWITCH_LOW_MAX && us < SWITCH_HIGH_MIN,
            SwitchPos::High => us >= SWITCH_HIGH_MIN,
        }
    }
}

/// What the joystick does when the radio stops sending.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Failsafe {
    /// Centre all axes and release all buttons.
    Center,
    /// Keep the last received values.
    Hold,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AxisMap {
    /// Zero based radio channel, `None` = axis stays centred.
    pub channel: Option<u8>,
    pub invert: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ButtonMap {
    /// Zero based radio channel.
    pub channel: u8,
    pub position: SwitchPos,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Mapping {
    pub axes: [AxisMap; AXIS_COUNT],
    /// Button N (1-based) = entry N-1.
    pub buttons: Vec<ButtonMap>,
    pub failsafe: Failsafe,
}

impl Default for Mapping {
    fn default() -> Self {
        let mut axes = [AxisMap { channel: None, invert: false }; AXIS_COUNT];
        for (i, axis) in axes.iter_mut().enumerate() {
            axis.channel = Some(i as u8);
        }
        Self { axes, buttons: Vec::new(), failsafe: Failsafe::Center }
    }
}

impl Mapping {
    /// Converts received channels (µs) into the joystick state.
    pub fn apply(&self, channels: &[u16]) -> JoyState {
        let mut state = JoyState::NEUTRAL;

        for (i, axis) in self.axes.iter().enumerate() {
            let Some(us) = axis.channel.and_then(|ch| channels.get(ch as usize)) else {
                continue;
            };
            let value = scale_axis(*us);
            state.axes[i] = if axis.invert { AXIS_MAX - value } else { value };
        }

        for (i, button) in self.buttons.iter().enumerate().take(BUTTON_COUNT) {
            if let Some(us) = channels.get(button.channel as usize) {
                if button.position.matches(*us) {
                    state.buttons |= 1 << i;
                }
            }
        }

        state
    }

    /// Drops anything a hand-edited config file could have broken.
    pub fn sanitize(&mut self) {
        for axis in &mut self.axes {
            if axis.channel.is_some_and(|ch| ch >= SELECTABLE_CHANNELS) {
                axis.channel = None;
            }
        }
        self.buttons.truncate(BUTTON_COUNT);
        for button in &mut self.buttons {
            button.channel = button.channel.min(SELECTABLE_CHANNELS - 1);
        }
    }
}

/// 988..2012 µs -> 0..32767 (1500 -> 16384).
pub fn scale_axis(us: u16) -> u16 {
    let us = us.clamp(US_MIN, US_MAX) as u32;
    let span = (US_MAX - US_MIN) as u32;
    (((us - US_MIN as u32) * AXIS_MAX as u32 + span / 2) / span) as u16
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SavedDevice {
    pub address: u64,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub language: Lang,
    /// The radio that was connected last.
    pub device: Option<SavedDevice>,
    /// Connect to `device` when the app starts.
    pub auto_connect: bool,
    /// The close button hides the window in the notification area instead of
    /// ending the program.
    pub close_to_tray: bool,
    /// Ask Windows for the shortest Bluetooth connection interval. On by default:
    /// measured, it cut the average delay from about 25 ms to about 8 ms.
    pub fast_connection: bool,
    /// Also list Bluetooth devices that do not advertise a name.
    pub show_unnamed: bool,
    pub mapping: Mapping,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: Lang::system_default(),
            device: None,
            auto_connect: true,
            close_to_tray: false,
            fast_connection: true,
            show_unnamed: false,
            mapping: Mapping::default(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join("EthosBtJoystick").join("config.json")
    }

    pub fn load() -> Self {
        let mut config: Config = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        config.mapping.sanitize();
        config
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::joystick::AXIS_CENTER;

    #[test]
    fn axis_scaling() {
        assert_eq!(scale_axis(US_MIN), 0);
        assert_eq!(scale_axis(US_CENTER), AXIS_CENTER);
        assert_eq!(scale_axis(US_MAX), AXIS_MAX);
        assert_eq!(scale_axis(0), 0);
        assert_eq!(scale_axis(4000), AXIS_MAX);
        assert!(scale_axis(1501) > scale_axis(1500));
    }

    #[test]
    fn default_mapping_is_one_to_one() {
        let mapping = Mapping::default();
        let channels = [988, 2012, 1500, 1500, 1500, 1500, 1500, 1500];
        let state = mapping.apply(&channels);
        assert_eq!(state.axes[0], 0);
        assert_eq!(state.axes[1], AXIS_MAX);
        assert_eq!(state.axes[2], AXIS_CENTER);
        assert_eq!(state.buttons, 0);
    }

    #[test]
    fn invert_unmapped_and_missing_channels() {
        let mut mapping = Mapping::default();
        mapping.axes[0].invert = true;
        mapping.axes[1].channel = None;
        mapping.axes[2].channel = Some(12); // not present in an 8 channel frame
        let state = mapping.apply(&[988, 988, 988, 988, 988, 988, 988, 988]);
        assert_eq!(state.axes[0], AXIS_MAX);
        assert_eq!(state.axes[1], AXIS_CENTER);
        assert_eq!(state.axes[2], AXIS_CENTER);
        assert_eq!(state.axes[3], 0);
    }

    #[test]
    fn switch_positions_become_buttons() {
        let mut mapping = Mapping::default();
        mapping.buttons = vec![
            ButtonMap { channel: 4, position: SwitchPos::Low },
            ButtonMap { channel: 4, position: SwitchPos::Mid },
            ButtonMap { channel: 4, position: SwitchPos::High },
        ];
        let mut ch = [1500u16; 8];
        ch[4] = 1000;
        assert_eq!(mapping.apply(&ch).buttons, 0b001);
        ch[4] = 1500;
        assert_eq!(mapping.apply(&ch).buttons, 0b010);
        ch[4] = 2000;
        assert_eq!(mapping.apply(&ch).buttons, 0b100);
    }

    #[test]
    fn config_roundtrip_and_partial_files() {
        let mut config = Config::default();
        config.device = Some(SavedDevice { address: 0xAABBCCDDEEFF, name: "X20".into() });
        config.mapping.buttons.push(ButtonMap { channel: 5, position: SwitchPos::High });
        let text = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&text).unwrap(), config);

        // unknown / missing fields fall back to defaults
        let partial: Config = serde_json::from_str(r#"{"auto_connect": false}"#).unwrap();
        assert!(!partial.auto_connect);
        assert_eq!(partial.mapping, Mapping::default());

        // the fast connection is on unless a settings file says otherwise
        assert!(partial.fast_connection);
        let off: Config = serde_json::from_str(r#"{"fast_connection": false}"#).unwrap();
        assert!(!off.fast_connection);
    }
}
