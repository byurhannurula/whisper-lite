//! The floating status pill.
//!
//! A separate always-on-top, transparent, non-activating window. Two rules matter:
//!
//! - **Never steal focus.** On macOS that means the window must not become key, or
//!   the user's cursor leaves the field they were dictating into.
//! - **Created once, then shown and hidden.** Destroying and recreating a webview costs ~200ms,
//!   which would land squarely in the latency budget.

use crate::settings::HudPosition;
use serde::Serialize;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl};

pub const WINDOW: &str = "hud";

/// Bumped on every state change.
///
/// Auto-hide timers capture the value at the moment they are scheduled and give up if it has
/// moved on. Without this, a timer from a previous dictation fires mid-way through the next one
/// and hides a HUD that should still be showing — which looked like the HUD randomly vanishing.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

const WIDTH: f64 = 250.0;
const HEIGHT: f64 = 80.0;
/// Gap from the screen edge. Enough to clear the Dock in its default position.
const MARGIN: f64 = 24.0;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatePayload {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(WINDOW).is_some() {
        return Ok(());
    }

    let window =
        tauri::WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("src/hud/index.html".into()))
            .title("Whisper Lite")
            .inner_size(WIDTH, HEIGHT)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .shadow(false)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()?;

    // Clicks pass straight through to whatever is underneath.
    let _ = window.set_ignore_cursor_events(true);
    // Follow the user across Spaces and sit above fullscreen apps.
    let _ = window.set_visible_on_all_workspaces(true);

    println!("[hud] window created");
    Ok(())
}

/// Shows the pill at `position`.
///
/// Position must be applied on every show, not once at startup: macOS does not reliably honour
/// a position set on a window that has never been realised, so the HUD ended up wherever the
/// window manager first placed it.
pub fn show_at(app: &AppHandle, position: HudPosition, state: &'static str, label: Option<String>) {
    if position == HudPosition::Hidden {
        return;
    }
    reposition(app, position);
    set_state(app, state, label);
    reposition(app, position);
}

pub fn set_state(app: &AppHandle, state: &'static str, label: Option<String>) {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let Some(window) = app.get_webview_window(WINDOW) else {
        eprintln!("[hud] no window to update");
        return;
    };

    if state == "idle" {
        let _ = window.emit("hud:state", StatePayload { state, label });
        let _ = window.hide();
        return;
    }

    // Show first, then emit. A hidden webview can be throttled, so emitting into it before it is
    // on screen risks the pill staying at its default (invisible) state while the window itself
    // is shown — which looks exactly like "the HUD never appears".
    if !window.is_visible().unwrap_or(false) {
        if let Err(e) = window.show() {
            eprintln!("[hud] show failed: {e}");
        }
    }

    if let Err(e) = window.emit("hud:state", StatePayload { state, label }) {
        eprintln!("[hud] emit failed: {e}");
    }

    println!(
        "[hud] state={state} visible={:?} pos={:?} size={:?}",
        window.is_visible(),
        window.outer_position().map(|p| (p.x, p.y)),
        window.outer_size().map(|s| (s.width, s.height)),
    );
}

#[derive(Clone, Serialize)]
pub struct TickPayload {
    pub level: f32,
}

/// Input level, emitted continuously while recording.
pub fn set_tick(app: &AppHandle, level: f32) {
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.emit("hud:tick", TickPayload { level });
    }
}

/// Moves the HUD to the configured anchor on the monitor containing the cursor.
pub fn reposition(app: &AppHandle, position: HudPosition) {
    let Some(window) = app.get_webview_window(WINDOW) else {
        return;
    };

    if position == HudPosition::Hidden {
        let _ = window.hide();
        return;
    }

    // Follow the monitor the user is actually working on, not always the primary.
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());

    let Some(monitor) = monitor else { return };

    let scale = monitor.scale_factor();
    let size = monitor.size().to_logical::<f64>(scale);
    let origin = monitor.position().to_logical::<f64>(scale);

    let (x, y) = match position {
        HudPosition::TopLeft => (MARGIN, MARGIN),
        HudPosition::TopCenter => ((size.width - WIDTH) / 2.0, MARGIN),
        HudPosition::TopRight => (size.width - WIDTH - MARGIN, MARGIN),
        HudPosition::MiddleLeft => (MARGIN, (size.height - HEIGHT) / 2.0),
        HudPosition::Center => ((size.width - WIDTH) / 2.0, (size.height - HEIGHT) / 2.0),
        HudPosition::MiddleRight => (size.width - WIDTH - MARGIN, (size.height - HEIGHT) / 2.0),
        HudPosition::BottomLeft => (MARGIN, size.height - HEIGHT - MARGIN),
        HudPosition::BottomCenter => ((size.width - WIDTH) / 2.0, size.height - HEIGHT - MARGIN),
        HudPosition::BottomRight => (size.width - WIDTH - MARGIN, size.height - HEIGHT - MARGIN),
        HudPosition::Hidden => return,
    };

    println!(
        "[hud] reposition {position:?}: monitor {}x{} @scale {scale} origin ({},{}) -> ({},{})",
        size.width,
        size.height,
        origin.x,
        origin.y,
        origin.x + x,
        origin.y + y
    );

    if let Err(e) = window.set_size(LogicalSize::new(WIDTH, HEIGHT)) {
        eprintln!("[hud] set_size failed: {e}");
    }
    if let Err(e) = window.set_position(LogicalPosition::new(origin.x + x, origin.y + y)) {
        eprintln!("[hud] set_position failed: {e}");
    }
}
