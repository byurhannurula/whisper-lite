//! Getting text into whatever field has focus.
//!
//! **Clipboard + synthetic paste only, for now.** The end state is to try the Accessibility API
//! first and fall back to paste, because the AX path avoids touching the clipboard at all. But AX
//! has to be written per-app-quirk — it silently no-ops in Electron apps, VS Code and Google Docs,
//! so it needs a verified-write-then-read-back loop and a bundle-ID blocklist. Paste works
//! everywhere today.
//!
//! The clipboard is saved and restored around the paste so dictating does not silently destroy
//! whatever the user had copied. Known limitation: only text is preserved. If the
//! clipboard held an image or rich content, it comes back as text or not at all.

use anyhow::{Context, Result};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::time::Duration;

/// How long to let the target app process the paste before restoring the clipboard.
///
/// Too short and the app pastes the *restored* contents; too long and the user notices their
/// clipboard is briefly wrong. 150ms is comfortable on a local machine.
const PASTE_SETTLE: Duration = Duration::from_millis(150);

pub fn insert(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let mut clipboard = arboard::Clipboard::new().context("opening clipboard")?;
    let previous = clipboard.get_text().ok();

    clipboard.set_text(text).context("writing to clipboard")?;

    // Give the pasteboard a moment to settle before synthesising the keystroke; pasting too
    // eagerly can pick up the previous contents.
    std::thread::sleep(Duration::from_millis(30));

    let result = paste();

    std::thread::sleep(PASTE_SETTLE);

    if let Some(prev) = previous {
        let _ = clipboard.set_text(prev);
    }

    result
}

#[cfg(target_os = "macos")]
fn paste() -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default()).context("creating input simulator")?;
    enigo
        .key(Key::Meta, Direction::Press)
        .context("pressing Cmd — is Accessibility granted?")?;
    let v = enigo.key(Key::Unicode('v'), Direction::Click);
    // Release Cmd even if the 'v' failed, or the modifier stays stuck down system-wide.
    let release = enigo.key(Key::Meta, Direction::Release);
    v.context("pressing V")?;
    release.context("releasing Cmd")?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn paste() -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default()).context("creating input simulator")?;
    enigo.key(Key::Control, Direction::Press)?;
    let v = enigo.key(Key::Unicode('v'), Direction::Click);
    let release = enigo.key(Key::Control, Direction::Release);
    v?;
    release?;
    Ok(())
}
