//! Single-key hotkeys: Fn (Globe) and Caps Lock.
//!
//! ## Why this exists separately from the normal hotkey path
//!
//! `tauri-plugin-global-shortcut` registers through Carbon's `RegisterEventHotKey`, which takes a
//! *keycode plus modifiers*. Fn and Caps Lock are not keycodes — they are modifier flags — so
//! Carbon simply cannot express them. Any single-key hotkey therefore needs a different
//! mechanism, and that mechanism is a CGEventTap watching `FlagsChanged`.
//!
//! ## What the tap buys us
//!
//! A tap can **consume** the event, which matters enormously for Caps Lock: without consuming it
//! the key still toggles capitals on, making the feature unusable. Returning `Drop` means the
//! system never sees the keypress at all, so Caps Lock stops behaving like Caps Lock while
//! whisper-lite is running and becomes a dictation key.
//!
//! ## The costs, stated plainly
//!
//! - **Secure input disables it.** When any app turns on secure input (password fields, some
//!   terminals), taps stop receiving events. The Carbon path is immune; this one is not. Normal
//!   combinations remain available for anyone who cares more about that.
//! - **It needs Accessibility permission**, which the app already requires for text injection.
//! - Fn is bound by macOS to the emoji picker / input-source switching by default. We consume the
//!   event, so that mostly stops, but setting "Press 🌐 to: Do Nothing" in Keyboard settings is
//!   still the cleaner result.

#![cfg(target_os = "macos")]

use core_foundation::runloop::{kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop};
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Which single key is being watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialKey {
    Fn,
    CapsLock,
}

impl SpecialKey {
    /// Parses the accelerator strings the settings file uses for these two keys.
    pub fn parse(accelerator: &str) -> Option<Self> {
        match accelerator {
            "Fn" => Some(SpecialKey::Fn),
            "CapsLock" => Some(SpecialKey::CapsLock),
            _ => None,
        }
    }

    fn flag(self) -> CGEventFlags {
        match self {
            SpecialKey::Fn => CGEventFlags::CGEventFlagSecondaryFn,
            SpecialKey::CapsLock => CGEventFlags::CGEventFlagAlphaShift,
        }
    }
}

/// Runs the tap until `stop` is set.
pub struct Monitor {
    stop: Arc<AtomicBool>,
}

impl Monitor {
    /// Starts watching `key`, calling `on_change(pressed)` as it goes down and up.
    ///
    /// The tap needs its own thread with a live run loop; CGEventTap delivers nothing without one.
    pub fn start(key: SpecialKey, on_change: impl Fn(bool) + Send + Sync + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();

        std::thread::spawn(move || {
            let target = key.flag();

            // FlagsChanged reports the *current* flag set, so the press/release edge has to be
            // derived by comparing against the previous state.
            //
            // Seeded to `true` deliberately. There is no way to read the live modifier state when
            // the tap starts, so the first event has to resolve the unknown. Starting at `false`
            // means an initial event carrying the flag looks like a fresh press and starts
            // recording the moment the app launches — exactly what was happening. Starting at
            // `true` makes that same event resolve to a spurious *release* instead, which is a
            // no-op because nothing is recording yet.
            let was_down = AtomicBool::new(true);

            // Ignore anything in the first moments after the tap comes up: macOS can deliver a
            // synthetic flags event as the tap is installed, and a phantom press there would
            // start a recording nobody asked for.
            let started = std::time::Instant::now();
            const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

            // The callback cannot re-enable the tap directly — the tap does not exist yet when
            // the closure is built — so it raises a flag the run loop acts on.
            let needs_reenable = Arc::new(AtomicBool::new(false));
            let flag = needs_reenable.clone();

            let tap = CGEventTap::new(
                CGEventTapLocation::HID,
                CGEventTapPlacement::HeadInsertEventTap,
                // Default (not ListenOnly) is what allows dropping the event.
                CGEventTapOptions::Default,
                vec![CGEventType::FlagsChanged],
                move |_proxy, event_type, event| {
                    // macOS disables a tap that takes too long, or on certain user input, and
                    // notifies through this same callback regardless of the event mask. Without
                    // re-enabling, the hotkey silently stops working for the rest of the session.
                    if matches!(
                        event_type,
                        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                    ) {
                        eprintln!("[hotkey] tap was disabled by the system — re-enabling");
                        flag.store(true, Ordering::SeqCst);
                        return CallbackResult::Keep;
                    }

                    let is_down = event.get_flags().contains(target);
                    let previously = was_down.swap(is_down, Ordering::SeqCst);

                    if is_down != previously && started.elapsed() > SETTLE {
                        on_change(is_down);
                    }

                    match key {
                        // Swallow Caps Lock entirely — otherwise every dictation would also turn
                        // capitals on, which makes it useless as a hotkey.
                        SpecialKey::CapsLock => CallbackResult::Drop,
                        // Fn is passed through. Consuming it would break the fn-row function keys
                        // (brightness, volume), which are far more valuable than suppressing the
                        // emoji picker.
                        SpecialKey::Fn => CallbackResult::Keep,
                    }
                },
            );

            let Ok(tap) = tap else {
                eprintln!(
                    "[hotkey] could not create event tap for {key:?} — \
                     Accessibility permission is probably not granted"
                );
                return;
            };

            let loop_source = match tap.mach_port().create_runloop_source(0) {
                Ok(source) => source,
                Err(()) => {
                    eprintln!("[hotkey] could not create run loop source for {key:?}");
                    return;
                }
            };

            let run_loop = CFRunLoop::get_current();
            unsafe {
                run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
            }
            tap.enable();

            println!("[hotkey] watching {key:?}");

            // Wake periodically so the stop flag is noticed; CFRunLoopRun would block forever.
            //
            // Must be DefaultMode, not CommonModes: CommonModes is a *set* of modes used when
            // adding a source, not a mode the loop can actually run in. Passing it here meant
            // the loop returned immediately without processing anything, so the tap was alive
            // but no events were ever delivered.
            while !stop_for_thread.load(Ordering::SeqCst) {
                CFRunLoop::run_in_mode(
                    unsafe { kCFRunLoopDefaultMode },
                    std::time::Duration::from_millis(250),
                    false,
                );

                if needs_reenable.swap(false, Ordering::SeqCst) {
                    tap.enable();
                }
            }

            println!("[hotkey] stopped watching {key:?}");
        });

        Self { stop }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
