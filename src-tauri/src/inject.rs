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

    write_transient(&mut clipboard, text).context("writing to clipboard")?;

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

/// Writes the dictation so clipboard-history apps skip it: it is on the pasteboard for ~150ms and
/// only as a transport, so recording it would leak every dictation into their history.
#[cfg(target_os = "macos")]
fn write_transient(clipboard: &mut arboard::Clipboard, text: &str) -> Result<(), arboard::Error> {
    use arboard::SetExtApple;
    clipboard.set().exclude_from_history().text(text)
}

#[cfg(not(target_os = "macos"))]
fn write_transient(clipboard: &mut arboard::Clipboard, text: &str) -> Result<(), arboard::Error> {
    clipboard.set_text(text)
}

#[cfg(target_os = "macos")]
fn paste() -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default()).context("creating input simulator")?;
    enigo
        .key(Key::Meta, Direction::Press)
        .context("pressing Cmd — is Accessibility granted?")?;
    // Not `Key::Unicode('v')`: enigo searches only the *active* layout for "v" and falls back to
    // key code 0 when there is none, so on a Cyrillic or Greek layout it sent Cmd+A, select all.
    let v = enigo.raw(keymap::paste_keycode(), Direction::Click);
    // Release Cmd even if the 'v' failed, or the modifier stays stuck down system-wide.
    let release = enigo.key(Key::Meta, Direction::Release);
    v.context("pressing V")?;
    release.context("releasing Cmd")?;
    Ok(())
}

/// Finds the key that produces "v" for Cmd shortcuts.
///
/// macOS resolves Cmd shortcuts against the current *ASCII-capable* layout: with a Cyrillic
/// layout active, Cmd+V follows the user's Latin layout; with Dvorak, it follows Dvorak. Looking
/// the character up in that layout matches both cases, where a fixed QWERTY key code would send
/// Cmd+. on Dvorak.
#[cfg(target_os = "macos")]
mod keymap {
    use std::ffi::c_void;

    /// `kVK_ANSI_V` from Carbon's Events.h, for when the layout data is unavailable.
    const ANSI_V: u16 = 9;
    const UC_KEY_ACTION_DISPLAY: u16 = 3;
    const UC_KEY_TRANSLATE_NO_DEAD_KEYS: u32 = 1;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        static kTISPropertyUnicodeKeyLayoutData: *const c_void;
        fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
        fn LMGetKbdType() -> u8;
        #[allow(clippy::too_many_arguments)]
        fn UCKeyTranslate(
            layout: *const u8,
            keycode: u16,
            action: u16,
            modifiers: u32,
            keyboard_type: u32,
            options: u32,
            dead_key_state: *mut u32,
            max_len: usize,
            actual_len: *mut usize,
            out: *mut u16,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFRelease(cf: *const c_void);
    }

    pub fn paste_keycode() -> u16 {
        // SAFETY: the source is released exactly once; the layout data it owns is only read
        // while the source is still alive.
        unsafe {
            let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
            if source.is_null() {
                return ANSI_V;
            }
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            let found = if data.is_null() {
                None
            } else {
                find(CFDataGetBytePtr(data), 'v')
            };
            CFRelease(source);
            found.unwrap_or(ANSI_V)
        }
    }

    unsafe fn find(layout: *const u8, wanted: char) -> Option<u16> {
        let kbd = LMGetKbdType() as u32;
        (0..128u16).find(|&code| {
            let mut dead = 0u32;
            let mut len = 0usize;
            let mut buf = [0u16; 4];
            let status = UCKeyTranslate(
                layout,
                code,
                UC_KEY_ACTION_DISPLAY,
                0,
                kbd,
                UC_KEY_TRANSLATE_NO_DEAD_KEYS,
                &mut dead,
                buf.len(),
                &mut len,
                buf.as_mut_ptr(),
            );
            status == 0 && len == 1 && buf[0] == wanted as u16
        })
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn finds_a_key_for_v() {
            let code = super::paste_keycode();
            assert!(code < 128);
        }
    }
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
