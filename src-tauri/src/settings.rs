//! User settings.
//!
//! Plain JSON in the app data directory. No database, no migrations framework — every field has
//! a `#[serde(default)]` so an older file still loads after new fields are added, and an
//! unreadable file falls back to defaults rather than refusing to start.
//!
//! The rule: if a reasonable person could want it different, it is a setting.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activation {
    /// Hold the key, speak, release to insert. The v1 default.
    #[default]
    Hold,
    /// Tap to start, tap again to stop.
    Toggle,
    /// Under the tap threshold toggles; over it behaves as hold-to-talk.
    Both,
}

/// Nine anchors plus the screen the HUD lives on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HudPosition {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    Center,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    #[default]
    BottomRight,
    Hidden,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Tauri accelerator string, e.g. `Ctrl+Shift+Space`.
    ///
    /// Default avoids `Cmd+Option+Space`, which macOS already binds to Finder search — the key
    /// would fire Finder as well as us. `fn` (Globe) is the nicer default but cannot be
    /// bound through Carbon hotkeys; that needs an NSEvent monitor and is still to come.
    #[serde(default = "default_shortcut")]
    pub shortcut: String,

    #[serde(default)]
    pub activation: Activation,

    /// Milliseconds below which a press counts as a tap rather than a hold, in `Both` mode.
    #[serde(default = "default_tap_threshold")]
    pub tap_threshold_ms: u64,

    #[serde(default = "default_true")]
    pub remove_fillers: bool,

    #[serde(default = "default_language")]
    pub language: String,

    #[serde(default)]
    pub hud_position: HudPosition,

    #[serde(default)]
    pub theme: Theme,

    #[serde(default = "default_true")]
    pub play_sounds: bool,

    #[serde(default)]
    pub launch_at_login: bool,

    /// Registry id of the active model. Whisper Base by default — M0 found it the best
    /// accuracy-per-megabyte for dictation, and it is what OpenWhispr recommends too.
    #[serde(default = "default_model")]
    pub model: String,

    /// Beam search instead of greedy decoding. Slower, but materially better on long or
    /// accented speech, which is where Base is weakest.
    #[serde(default = "default_true")]
    pub accurate: bool,

    /// Words Whisper should always get right — names, tools, acronyms.
    ///
    /// Fed to the decoder as its initial prompt, which conditions spelling and casing. This is
    /// the cheapest accuracy lever available and costs nothing at runtime.
    #[serde(default)]
    pub dictionary: Vec<String>,

    /// Spoken trigger phrases replaced with longer text after transcription.
    #[serde(default)]
    pub snippets: Vec<Snippet>,

    /// System sound played when recording starts. Empty means silent.
    #[serde(default = "default_sound_start")]
    pub sound_start: String,

    /// System sound played when recording stops.
    #[serde(default = "default_sound_stop")]
    pub sound_stop: String,

    /// Input device to record from. Empty means "whatever macOS calls the default".
    ///
    /// Stored as the device *name* rather than an index: cpal exposes no stable identifier, and
    /// an index silently points at a different microphone as soon as one is unplugged. A name
    /// that no longer resolves falls back to the system default rather than failing to record.
    #[serde(default)]
    pub input_device: String,

    /// Capitalises the first letter of the insert and of each new sentence.
    ///
    /// Whisper usually punctuates but is inconsistent about case after a full stop, which shows
    /// up immediately when dictating more than one sentence at a time.
    #[serde(default = "default_true")]
    pub autocapitalize: bool,

    /// Show a Dock icon as well as the menu-bar item.
    ///
    /// Off by default: this is a menu-bar utility, and a Dock icon for something you drive
    /// entirely from a hotkey is clutter. Some people want it anyway, so it is a setting.
    #[serde(default)]
    pub show_in_dock: bool,

    /// Left-clicking the menu-bar icon starts or stops dictation instead of opening the menu.
    ///
    /// The menu stays reachable on right-click, so nothing becomes unreachable.
    #[serde(default)]
    pub menubar_click_records: bool,

    #[serde(default = "default_true")]
    pub history_enabled: bool,

    /// Days to keep transcripts. 0 keeps them until the count cap is hit.
    #[serde(default = "default_history_days")]
    pub history_days: u32,
}

fn default_history_days() -> u32 {
    30
}

/// A spoken shorthand and what it expands to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub trigger: String,
    pub replacement: String,
}

// Both short. The previous stop cue was Pop, which is 1.6 seconds long and lands well after the
// user has moved on — it read as clumsy rather than as feedback.
fn default_sound_start() -> String {
    "Tink".to_string()
}
fn default_sound_stop() -> String {
    "Purr".to_string()
}

fn default_model() -> String {
    "base".to_string()
}

fn default_shortcut() -> String {
    "Ctrl+Shift+Space".to_string()
}
fn default_tap_threshold() -> u64 {
    200
}
fn default_true() -> bool {
    true
}
fn default_language() -> String {
    "en".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: default_shortcut(),
            activation: Activation::default(),
            tap_threshold_ms: default_tap_threshold(),
            remove_fillers: true,
            language: default_language(),
            hud_position: HudPosition::default(),
            theme: Theme::default(),
            play_sounds: true,
            launch_at_login: false,
            model: default_model(),
            accurate: true,
            dictionary: Vec::new(),
            snippets: Vec::new(),
            sound_start: default_sound_start(),
            sound_stop: default_sound_stop(),
            input_device: String::new(),
            autocapitalize: true,
            show_in_dock: false,
            menubar_click_records: false,
            history_enabled: true,
            history_days: default_history_days(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(s) => s,
            Err(e) => {
                // Never refuse to start over a bad settings file — the app is still usable with
                // defaults, and losing dictation is worse than losing preferences.
                eprintln!("[settings] using defaults ({e:#})");
                Self::default()
            }
        }
    }

    fn try_load() -> Result<Self> {
        let path = path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&raw).context("parsing settings.json")
    }

    pub fn save(&self) -> Result<()> {
        let path = path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serialising settings")?;

        // Write-then-rename so a crash mid-write cannot leave a truncated settings file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).context("replacing settings.json")?;
        Ok(())
    }
}

pub fn dir() -> PathBuf {
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support/whisper-lite")
}

fn path() -> PathBuf {
    dir().join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shortcut_avoids_the_finder_conflict() {
        // Cmd+Option+Space is macOS's Finder search binding; picking it means the key fires
        // Finder as well as us, which is exactly the bug this default exists to avoid.
        assert_ne!(Settings::default().shortcut, "Cmd+Option+Space");
    }

    #[test]
    fn defaults_match_the_prd() {
        let s = Settings::default();
        assert_eq!(s.activation, Activation::Hold);
        assert_eq!(s.hud_position, HudPosition::BottomRight);
        assert!(s.remove_fillers);
        assert!(!s.launch_at_login);
    }

    #[test]
    fn partial_json_fills_in_defaults() {
        // An older settings file missing newer keys must still load.
        let s: Settings = serde_json::from_str(r#"{"shortcut":"Alt+X"}"#).unwrap();
        assert_eq!(s.shortcut, "Alt+X");
        assert_eq!(s.activation, Activation::Hold);
        assert_eq!(s.tap_threshold_ms, 200);
    }

    #[test]
    fn empty_json_is_all_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.shortcut, default_shortcut());
    }

    #[test]
    fn old_settings_files_get_the_new_model_field() {
        // A file written before the model picker existed must still load.
        let s: Settings = serde_json::from_str(r#"{"shortcut":"Ctrl+Shift+Space"}"#).unwrap();
        assert_eq!(s.model, "base");
        assert!(s.accurate);
    }

    #[test]
    fn missing_input_device_means_system_default() {
        // Files written before the microphone picker existed have no such key, and an empty
        // string is what the recorder reads as "use whatever macOS considers default".
        let s: Settings = serde_json::from_str(r#"{"shortcut":"Ctrl+Shift+Space"}"#).unwrap();
        assert_eq!(s.input_device, "");
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = Settings::default();
        s.hud_position = HudPosition::TopLeft;
        s.activation = Activation::Toggle;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hud_position, HudPosition::TopLeft);
        assert_eq!(back.activation, Activation::Toggle);
    }
}
