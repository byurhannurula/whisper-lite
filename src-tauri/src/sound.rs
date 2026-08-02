//! Start and stop cues.
//!
//! Uses the system sounds already on the machine rather than bundling audio files — they are
//! tuned to sit under other audio without being startling, and shipping our own would mean
//! picking something that fights whatever the user is listening to.
//!
//! Always played on a detached thread. `afplay` takes tens of milliseconds to spawn, and the
//! start cue fires on the same path as beginning to record, which is squarely on the latency
//! budget.

/// Sounds offered in Settings, shortest first.
///
/// Duration matters more than timbre for a cue that fires on every dictation: anything past
/// about a second finishes after the user has already moved on and reads as clumsy. Pop, the
/// previous stop cue, is 1.6 seconds.
pub const CHOICES: &[(&str, &str)] = &[
    ("", "None"),
    ("Tink", "Tink — short, crisp"),
    ("Morse", "Morse — short, soft"),
    ("Purr", "Purr — soft, low"),
    ("Bottle", "Bottle — hollow"),
    ("Frog", "Frog — playful"),
    ("Blow", "Blow — airy"),
    ("Hero", "Hero — bright"),
    ("Glass", "Glass — long, bright"),
    ("Pop", "Pop — long"),
    ("Submarine", "Submarine — long, deep"),
];

pub fn play(name: &str) {
    if name.is_empty() {
        return;
    }
    let path = format!("/System/Library/Sounds/{name}.aiff");
    if !std::path::Path::new(&path).exists() {
        return;
    }
    spawn(path);
}

#[cfg(target_os = "macos")]
fn spawn(path: String) {
    // Always detached: spawning a player takes tens of milliseconds, and the start cue fires on
    // the same path as beginning to record, which is squarely on the latency budget.
    std::thread::spawn(move || {
        // Half volume — this fires every time the user dictates, so it has to be unobtrusive.
        let _ = std::process::Command::new("/usr/bin/afplay")
            .args(["-v", "0.5", &path])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

#[cfg(not(target_os = "macos"))]
fn spawn(_path: String) {
    // Windows and Linux cues arrive with those platforms (M5, v1.2).
}
