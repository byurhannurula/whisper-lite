//! whisper-lite: hold a key, talk, let go, your words are in the box.
//!
//! Everything runs in Rust; the webview provides the HUD and the settings window only.

mod audio;
mod engine;
mod history;
mod hud;
mod inject;
mod models;
mod settings;
mod sound;
#[cfg(target_os = "macos")]
mod specialkey;
mod text;

use anyhow::Result;
use settings::{Activation, Settings};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

struct App {
    /// Serialises start/stop; see the SAFETY note on `audio::Recorder`.
    recorder: Mutex<audio::Recorder>,
    /// The loaded model, or `None` when there is not one yet.
    ///
    /// Optional because a fresh install has no model on disk — nothing is bundled — and the app
    /// has to start anyway, or there is no way to reach the UI that downloads one.
    engine: Mutex<Option<engine::Engine>>,
    /// Serialises model loads, so the startup warm-up and a first dictation cannot both pay the
    /// cost or interleave their writes to `engine`.
    engine_load: Mutex<()>,
    settings: Mutex<Settings>,
    /// The shortcut currently registered, so it can be unregistered before rebinding.
    /// Rebinding without this leaves the old binding live and, on some macOS versions,
    /// makes the new one silently fail.
    bound: Mutex<Option<Shortcut>>,
    /// Live tap when the hotkey is a single key (Fn / Caps Lock), which Carbon cannot bind.
    /// Dropping it stops the tap.
    #[cfg(target_os = "macos")]
    special: Mutex<Option<specialkey::Monitor>>,
    /// True while a transcription is in flight, so a second press cannot start a new one.
    busy: AtomicBool,
    /// True while capturing, which is what toggle mode flips.
    recording: AtomicBool,
    /// When the current press started, for distinguishing a tap from a hold in `Both` mode.
    pressed_at: Mutex<Option<Instant>>,
    tray: Mutex<Option<TrayIcon>>,
    tray_status: Mutex<Option<MenuItem<tauri::Wry>>>,
    /// Cancel flags for in-flight model downloads, keyed by model id.
    downloads: Mutex<std::collections::HashMap<String, std::sync::Arc<AtomicBool>>>,
}

impl App {
    /// Resting label for the tray, which doubles as the shortcut reminder.
    fn idle_status(&self) -> String {
        let cfg = self.settings.lock().unwrap();
        // On a fresh install "Ready — hold ⌃⇧Space" would be a lie: pressing it cannot work
        // until something has been downloaded.
        if !models::is_installed(&cfg.model) {
            return "No model — open Settings".to_string();
        }
        format!("Ready — hold {}", cfg.shortcut)
    }

    fn set_status(&self, text: &str) {
        if let Ok(guard) = self.tray_status.lock() {
            if let Some(item) = guard.as_ref() {
                let _ = item.set_text(text);
            }
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // Registered exactly once. A duplicate registration panics at startup with
        // `Invalid argument (os error 22)`, and silently does nothing on some macOS versions.
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    let state = app.state::<App>();
                    let is_ours = state
                        .bound
                        .lock()
                        .map(|b| b.as_ref() == Some(shortcut))
                        .unwrap_or(false);
                    if !is_ours {
                        return;
                    }
                    match event.state() {
                        ShortcutState::Pressed => on_press(app),
                        ShortcutState::Released => on_release(app),
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            open_settings,
            close_settings,
            set_capture_mode,
            list_models,
            download_model,
            cancel_download,
            delete_model,
            set_active_model,
            sound_choices,
            preview_sound,
            input_devices,
            about,
            reveal,
            open_url,
            list_history,
            delete_history_entry,
            clear_history,
            reinsert
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            #[cfg(target_os = "macos")]
            // Managed state does not exist yet, so this cannot go through
            // `apply_activation_policy`. `init()` applies the user's preference straight after.
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let state = match init() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("\n[whisper-lite] startup failed:\n  {e:#}\n");
                    return Err(e.into());
                }
            };

            let shortcut = state.settings.lock().unwrap().shortcut.clone();
            let hud_position = state.settings.lock().unwrap().hud_position;
            app.manage(state);

            build_tray(&handle, &shortcut)?;

            // Now that settings are managed, honour "Show in Dock" — startup set Accessory
            // unconditionally because the preference was not readable yet.
            #[cfg(target_os = "macos")]
            apply_activation_policy(&handle, false);

            hud::create(&handle)?;
            hud::reposition(&handle, hud_position);
            // Belt and braces: the HUD must be hidden at rest regardless of how it was built.
            hud::set_state(&handle, "idle", None);

            rebind(&handle, &shortcut);

            // Warm the model in the background: the app is already usable, and this way the
            // first dictation is not the one that waits for the load. Failing here is normal and
            // not fatal — "no model downloaded yet" is exactly the state a fresh install is in,
            // and `begin` sends the user to the Models section when they try to dictate.
            {
                let handle = handle.clone();
                std::thread::spawn(move || {
                    match ensure_engine(&handle) {
                        Ok(()) => println!("[whisper-lite] model ready"),
                        Err(e) => eprintln!("[whisper-lite] no model ready: {e}"),
                    }
                    let state = handle.state::<App>();
                    state.set_status(&state.idle_status());
                });
            }

            // Debug aid: WHISPER_LITE_HUD_TEST=1 shows the indicator shortly after launch so it
            // can be verified without a microphone or a menu click.
            if std::env::var("WHISPER_LITE_HUD_TEST").is_ok() {
                let handle = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let position = handle.state::<App>().settings.lock().unwrap().hud_position;
                    hud::show_at(
                        &handle,
                        position,
                        "listening",
                        Some("Test indicator".into()),
                    );
                });
            }

            println!("[whisper-lite] ready — hold {shortcut} and speak");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Whisper Lite")
        .run(|app, event| match event {
            // Cmd+Q while the settings window is focused arrives here rather than through the
            // tray menu, so it needs the same ordered teardown.
            tauri::RunEvent::ExitRequested { api, .. } => {
                api.prevent_exit();
                shutdown(app);
            }
            // Closing the last window must not quit a menu-bar app.
            tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::CloseRequested { .. },
                ..
            } => {}
            _ => {}
        });
}

/// Mirrors stdout/stderr into a log file.
///
/// The app is launched with `open` so macOS attributes microphone and Accessibility permissions
/// to whisper-lite itself rather than to whatever terminal spawned it — but that also leaves no
/// console to read. This gives one to look at.
fn redirect_logs() {
    let path = settings::dir().join("whisper-lite.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = std::fs::File::create(&path) {
        use std::os::unix::io::IntoRawFd;
        unsafe {
            let fd = file.into_raw_fd();
            libc::dup2(fd, libc::STDOUT_FILENO);
            libc::dup2(fd, libc::STDERR_FILENO);
        }
    }
}

/// Tears down in a defined order, then exits without unwinding.
///
/// Two things make ordinary teardown unsafe here. The CGEventTap runs on its own thread and
/// calls back into managed state, so it has to stop *before* that state is dropped or the
/// callback lands on freed memory. And whisper.cpp's Metal context plus cpal's CoreAudio stream
/// are FFI resources whose destructors run in an order Rust does not control — releasing them
/// during process teardown is what produced the crash reporter on quit.
///
/// `process::exit` skips remaining destructors deliberately. The OS reclaims everything anyway,
/// and there is nothing here that needs flushing: settings and history are written on change.
fn shutdown(app: &AppHandle) {
    let state = app.state::<App>();

    // Stop the tap first so no further callbacks can arrive.
    #[cfg(target_os = "macos")]
    {
        *state.special.lock().unwrap() = None;
    }

    // Release the microphone so the indicator clears immediately rather than at process death.
    if state.recording.swap(false, Ordering::SeqCst) {
        if let Ok(recorder) = state.recorder.lock() {
            let _ = recorder.stop();
        }
    }

    println!("[whisper-lite] shutting down");
    // Nothing here buffers state worth losing — settings and history are written as they change —
    // but stdout does, and neither exit path below flushes it.
    use std::io::Write;
    let _ = std::io::stdout().flush();

    // `_exit`, not `process::exit`.
    //
    // `process::exit` skips *Rust* destructors but still runs C++ static destructors and atexit
    // handlers. ggml's Metal backend registers one of those, and it asserts its residency set is
    // empty — `GGML_ASSERT([rsets->data count] == 0)` — which is what produced the crash reporter
    // on every quit. `_exit` terminates immediately without running any of them.
    //
    // Safe here because the process is ending: the kernel reclaims memory, file handles and the
    // audio device regardless.
    unsafe { libc::_exit(0) };
}

/// Called by the C runtime on any normal process exit.
///
/// Terminates immediately so nothing further runs.
extern "C" fn exit_immediately() {
    unsafe { libc::_exit(0) };
}

/// Stops the process before ggml's teardown can run.
///
/// The crash reporter on quit came from ggml's Metal backend asserting that its residency set is
/// empty while being destroyed. That teardown is a C++ static destructor, so it fires on *every*
/// normal exit — Cmd+Q, the Dock menu, an Apple Event — and Tauri's `ExitRequested` does not see
/// most of those, which is why intercepting at the app layer did not help.
///
/// Handlers run in reverse registration order, so registering ours *after* the Metal device has
/// been created puts it ahead of ggml's in the queue. It calls `_exit`, and nothing after it runs.
///
/// Safe because the process is ending: the kernel reclaims memory, file handles and the audio
/// device regardless, and settings and history are written as they change rather than on exit.
fn suppress_ggml_teardown() {
    unsafe {
        libc::atexit(exit_immediately);
    }
}

/// Arms `suppress_ggml_teardown` exactly once, after a model has actually loaded.
///
/// `atexit` handlers run in reverse registration order, so ours has to be installed *after* ggml
/// has created its Metal device in order to run *before* ggml's own teardown. That used to be
/// guaranteed by loading the model during startup; now that loading is lazy, the first successful
/// load is what arms it. An app that never loads a model never creates a Metal context, so there
/// is nothing to suppress.
static ARM_TEARDOWN: std::sync::Once = std::sync::Once::new();

/// Loads `id` into the engine slot, replacing whatever is there.
///
/// Blocking: ~190ms once the machine has compiled Metal shaders, several seconds the very first
/// time. Never call this on the main thread.
fn load_model(state: &App, id: &str) -> Result<(), String> {
    let path = engine::model_path(id);
    println!("[whisper-lite] loading '{id}' from {}", path.display());

    let started = Instant::now();
    let loaded = engine::Engine::load(id, &path).map_err(|e| format!("{e:#}"))?;
    println!("[whisper-lite] loaded '{id}' in {:?}", started.elapsed());

    ARM_TEARDOWN.call_once(suppress_ggml_teardown);
    *state.engine.lock().unwrap() = Some(loaded);
    Ok(())
}

/// Makes sure the configured model is loaded, loading it if not.
///
/// Blocking, for the same reason as `load_model`.
fn ensure_engine(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<App>();
    let _guard = state.engine_load.lock().unwrap();

    let wanted = state.settings.lock().unwrap().model.clone();
    if state
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|e| e.model_id() == wanted)
    {
        return Ok(());
    }

    load_model(state.inner(), &wanted)
}

fn init() -> Result<App> {
    redirect_logs();
    let settings = Settings::load();

    // The model is deliberately *not* loaded here. Startup must succeed on a machine that has
    // never downloaded one, because the Models UI is the only way to get one and it lives inside
    // the app. `ensure_engine` loads it on a background thread once the app is up.
    let recorder = audio::Recorder::open(&settings.input_device)?;

    Ok(App {
        recorder: Mutex::new(recorder),
        engine: Mutex::new(None),
        engine_load: Mutex::new(()),
        settings: Mutex::new(settings),
        bound: Mutex::new(None),
        #[cfg(target_os = "macos")]
        special: Mutex::new(None),
        busy: AtomicBool::new(false),
        recording: AtomicBool::new(false),
        pressed_at: Mutex::new(None),
        tray: Mutex::new(None),
        tray_status: Mutex::new(None),
        downloads: Mutex::new(Default::default()),
    })
}

/// Prefix for the tray's microphone items. The rest of the id is the device name, empty for
/// "System default", which is exactly what `Settings::input_device` stores.
const MIC_ITEM_PREFIX: &str = "mic:";

/// The microphone submenu, with the active device ticked.
///
/// Devices are enumerated when the menu is built, so one plugged in afterwards will not appear
/// until the tray is rebuilt (which a settings save does). The picker in Settings is the
/// authoritative list; this is the shortcut.
fn build_microphone_submenu(app: &AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let active = app
        .state::<App>()
        .settings
        .lock()
        .map(|s| s.input_device.clone())
        .unwrap_or_default();

    let default_item = CheckMenuItem::with_id(
        app,
        MIC_ITEM_PREFIX,
        "System default",
        true,
        active.is_empty(),
        None::<&str>,
    )?;

    let mut items: Vec<CheckMenuItem<tauri::Wry>> = Vec::new();
    for name in audio::input_devices() {
        items.push(CheckMenuItem::with_id(
            app,
            format!("{MIC_ITEM_PREFIX}{name}"),
            &name,
            true,
            active == name,
            None::<&str>,
        )?);
    }

    let submenu = Submenu::new(app, "Microphone", true)?;
    submenu.append(&default_item)?;
    for item in &items {
        submenu.append(item)?;
    }
    Ok(submenu)
}

/// Sets the Dock/activation policy from current state.
///
/// Regular (Dock icon, app menu) when the user asked for one *or* while a real window is open —
/// an Accessory app's window cannot properly become key. Accessory otherwise, which is what makes
/// this a menu-bar app. Centralised because three separate call sites used to flip the policy
/// directly, and adding "Show in Dock" to that meant closing a window switched the Dock icon off
/// again regardless of the setting.
#[cfg(target_os = "macos")]
fn apply_activation_policy(app: &AppHandle, window_open: bool) {
    let pinned = app
        .state::<App>()
        .settings
        .lock()
        .map(|s| s.show_in_dock)
        .unwrap_or(false);

    let _ = app.set_activation_policy(if window_open || pinned {
        tauri::ActivationPolicy::Regular
    } else {
        tauri::ActivationPolicy::Accessory
    });
}

/// Starts or stops dictation, for entry points that are a single event rather than a key
/// press and release — currently the menu bar.
fn toggle_dictation(app: &AppHandle) {
    let handle = app.clone();
    // Injection goes through HIToolbox APIs that abort the process off the main queue. Tray
    // events already arrive on main, but saying so explicitly costs nothing and survives a
    // future caller that does not.
    let _ = app.run_on_main_thread(move || {
        if handle.state::<App>().recording.load(Ordering::SeqCst) {
            finish(&handle);
        } else {
            begin(&handle);
        }
    });
}

/* Tray --------------------------------------------------------------------- */

/// Builds the tray menu from scratch, returning it alongside the status item.
///
/// Separate from `build_tray` because the menu has state in it — which microphone is ticked, what
/// the shortcut is — and the only way to update a `CheckMenuItem`'s tick reliably on macOS is to
/// hand the tray a freshly built menu.
///
/// The status item comes back because `App::set_status` holds onto it. A rebuilt menu contains a
/// *new* status item, so the stored handle has to be replaced or every later status update writes
/// to a detached item and the tray label silently freezes.
fn build_tray_menu(
    app: &AppHandle,
    shortcut: &str,
) -> tauri::Result<(Menu<tauri::Wry>, MenuItem<tauri::Wry>)> {
    let status = MenuItem::with_id(
        app,
        "status",
        format!("Ready — hold {shortcut}"),
        false,
        None::<&str>,
    )?;
    let toggle_item = MenuItem::with_id(app, "toggle", "Toggle Recording", true, None::<&str>)?;
    let history_item = MenuItem::with_id(app, "history", "History…", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, Some("cmd+,"))?;

    let mic_menu = build_microphone_submenu(app)?;
    // Disabled, so it reads as a label rather than something to click — the same trick the
    // status line above uses.
    let version = MenuItem::with_id(
        app,
        "version",
        format!("Version {}", app.package_info().version),
        false,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit Whisper Lite", true, Some("cmd+q"))?;
    let sep = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(
        app,
        &[
            &status,
            &sep,
            &toggle_item,
            &history_item,
            &settings_item,
            &sep,
            &mic_menu,
            &sep,
            &version,
            &quit,
        ],
    )?;

    Ok((menu, status))
}

/// Replaces the live tray menu, so tick marks and the status line reflect current settings.
fn refresh_tray_menu(app: &AppHandle, shortcut: &str) -> tauri::Result<()> {
    let (menu, status) = build_tray_menu(app, shortcut)?;
    let state = app.state::<App>();
    if let Some(tray) = state.tray.lock().unwrap().as_ref() {
        tray.set_menu(Some(menu))?;
    }
    *state.tray_status.lock().unwrap() = Some(status);
    Ok(())
}

fn build_tray(app: &AppHandle, shortcut: &str) -> tauri::Result<()> {
    let (menu, status) = build_tray_menu(app, shortcut)?;

    // A dedicated template glyph, not the app icon. Template images take their shape from the
    // alpha channel and are recoloured by macOS for light and dark menu bars — handing it the
    // full-colour app icon produced a solid white square.
    let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
        .expect("tray icon is a valid PNG");

    let click_records = app
        .state::<App>()
        .settings
        .lock()
        .map(|s| s.menubar_click_records)
        .unwrap_or(false);

    let tray = TrayIconBuilder::with_id("main")
        .icon(tray_icon)
        .icon_as_template(true)
        .menu(&menu)
        // When left-click records, the menu moves to right-click so nothing becomes unreachable.
        .show_menu_on_left_click(!click_records)
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            if !app
                .state::<App>()
                .settings
                .lock()
                .map(|s| s.menubar_click_records)
                .unwrap_or(false)
            {
                return;
            }
            // Fire on release, not press, so a click-and-drag on the menu bar does not record.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_dictation(app);
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => toggle_dictation(app),
            "settings" => open_settings_window(app),
            id if id.starts_with(MIC_ITEM_PREFIX) => {
                let device = id[MIC_ITEM_PREFIX.len()..].to_string();
                let changed = {
                    let state = app.state::<App>();
                    let mut cfg = state.settings.lock().unwrap();
                    let changed = cfg.input_device != device;
                    cfg.input_device = device.clone();
                    if changed {
                        let _ = cfg.save();
                    }
                    changed
                };
                if changed {
                    swap_input_device(app, device);
                }
                // Rebuild so the tick lands on the item the user just chose. macOS does not
                // manage radio behaviour for check items, so without this every device the user
                // has ever picked stays ticked.
                let shortcut = app.state::<App>().settings.lock().unwrap().shortcut.clone();
                let _ = refresh_tray_menu(app, &shortcut);
            }
            "history" => {
                open_settings_window(app);
                // The window may have only just been created, so the webview is not yet
                // listening. Emitting on the window itself once it exists is enough — the
                // settings frontend subscribes before it renders anything.
                if let Some(window) = app.get_webview_window("settings") {
                    let _ = window.emit("settings:goto", "tab-history");
                }
            }
            "quit" => shutdown(app),
            _ => {}
        })
        .build(app)?;

    let state = app.state::<App>();
    *state.tray.lock().unwrap() = Some(tray);
    *state.tray_status.lock().unwrap() = Some(status);
    Ok(())
}

/* Settings window ---------------------------------------------------------- */

fn open_settings_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let built = WebviewWindowBuilder::new(
        app,
        "settings",
        WebviewUrl::App("src/settings/index.html".into()),
    )
    .title("Whisper Lite Settings")
    .inner_size(860.0, 640.0)
    .min_inner_size(720.0, 520.0)
    .resizable(true)
    // Native chrome: real traffic lights, real rounded corners, real shadow. A borderless
    // transparent window looked wrong because macOS does not clip its corners for you — the
    // content rounded but the window itself stayed square.
    .title_bar_style(tauri::TitleBarStyle::Overlay)
    .hidden_title(true)
    .center()
    .build();

    match built {
        Ok(window) => {
            // Restore the menu-bar-only policy however the window goes away — the red traffic
            // light closes it directly and never reaches the close_settings command, which left
            // the app in Regular policy with a Dock icon and no window.
            let handle = app.clone();
            window.on_window_event(move |event| {
                if matches!(
                    event,
                    tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
                ) {
                    #[cfg(target_os = "macos")]
                    apply_activation_policy(&handle, false);
                }
            });

            // A settings window is a real window: it needs focus, unlike the HUD.
            let _ = window.set_focus();
            // Menu-bar apps run under ActivationPolicy::Accessory, which can leave their windows
            // floating above everything. Say explicitly that this one is ordinary.
            let _ = window.set_always_on_top(false);
            #[cfg(target_os = "macos")]
            apply_activation_policy(app, true);
        }
        Err(e) => eprintln!("[whisper-lite] could not open settings: {e}"),
    }
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    open_settings_window(&app);
}

/// Sound options for the picker, so the list lives in one place rather than being duplicated
/// in the frontend.
/* History ------------------------------------------------------------------ */

#[tauri::command]
fn list_history(app: AppHandle) -> Vec<history::Entry> {
    let days = app.state::<App>().settings.lock().unwrap().history_days;
    history::prune(history::load(), days)
}

#[tauri::command]
fn delete_history_entry(at: u64) -> Result<(), String> {
    history::delete(at).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn clear_history() -> Result<(), String> {
    history::clear().map_err(|e| format!("{e:#}"))
}

/// Puts a past transcript back on the clipboard and pastes it.
#[tauri::command]
fn reinsert(app: AppHandle, text: String) -> Result<(), String> {
    // Injection is a synthesised ⌘V, which lands in whatever is frontmost — and this command is
    // only ever triggered from the History list, so the frontmost window is the Settings window
    // itself. Hiding the app first hands focus back to whatever the user was in before, which is
    // where they meant the text to go.
    #[cfg(target_os = "macos")]
    {
        let _ = app.hide();
        // Focus handover is asynchronous. Pasting immediately still lands in the window that is
        // on its way out.
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let _ = &app;
    inject::insert(&text).map_err(|e| format!("{e:#}"))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct About {
    version: String,
    model: String,
    shortcut: String,
    data_dir: String,
    log_path: String,
    models_dir: String,
    disk_mb: u64,
}

#[tauri::command]
fn about(app: AppHandle) -> About {
    let cfg = app.state::<App>().settings.lock().unwrap().clone();
    let models_dir = models::models_dir();

    let disk_bytes: u64 = std::fs::read_dir(&models_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();

    About {
        version: env!("CARGO_PKG_VERSION").to_string(),
        model: models::spec(&cfg.model)
            .map(|m| m.name.to_string())
            .unwrap_or(cfg.model),
        shortcut: cfg.shortcut,
        data_dir: settings::dir().display().to_string(),
        log_path: settings::dir()
            .join("whisper-lite.log")
            .display()
            .to_string(),
        models_dir: models_dir.display().to_string(),
        disk_mb: disk_bytes / (1024 * 1024),
    }
}

#[tauri::command]
fn reveal(path: String) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open")
        .args(["-R", &path])
        .spawn();
}

/// Opens an external link in the default browser.
///
/// The scheme is checked rather than trusted. This shells out to `open`, which will just as
/// happily launch a `file://` path or hand a custom scheme to whatever app claims it, and the
/// string arrives from the webview.
#[tauri::command]
fn open_url(url: String) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        eprintln!("[whisper-lite] refusing to open non-web URL: {url}");
        return;
    }
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(&url)
        .spawn();
}

#[tauri::command]
fn sound_choices() -> Vec<(String, String)> {
    sound::CHOICES
        .iter()
        .map(|(id, label)| (id.to_string(), label.to_string()))
        .collect()
}

#[tauri::command]
fn preview_sound(name: String) {
    sound::play(&name);
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Settings {
    app.state::<App>().settings.lock().unwrap().clone()
}

/// Input devices for the microphone picker, with the system default first as an empty value.
#[tauri::command]
fn input_devices() -> Vec<String> {
    audio::input_devices()
}

/// Replaces the capture stream after the user picks a different microphone.
///
/// Hopped to the main thread rather than run on the command's worker: the SAFETY note on
/// `audio::Recorder` allows the cpal stream to be created and dropped only on the thread it has
/// always lived on, and swapping the recorder drops the old stream.
fn swap_input_device(app: &AppHandle, wanted: String) {
    let handle = app.clone();
    let hop = app.run_on_main_thread(move || {
        let state = handle.state::<App>();

        // Never yank the device out from under a dictation in progress. The next one picks up
        // the new microphone, which is a far better outcome than losing the current recording.
        if state.recording.load(Ordering::SeqCst) {
            println!("[audio] device change deferred — a recording is in progress");
            return;
        }

        match audio::Recorder::open(&wanted) {
            Ok(recorder) => *state.recorder.lock().unwrap() = recorder,
            Err(e) => eprintln!("[audio] could not open '{wanted}': {e:#}"),
        }
    });

    if let Err(e) = hop {
        eprintln!("[audio] could not reach the main thread to change device: {e}");
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let state = app.state::<App>();

    let previous = state.settings.lock().unwrap().clone();
    settings.save().map_err(|e| format!("{e:#}"))?;

    // The toggle previously only persisted a field that nothing read, so it reported "Saved"
    // while doing nothing at all.
    if previous.launch_at_login != settings.launch_at_login {
        use tauri_plugin_autostart::ManagerExt;
        let manager = app.autolaunch();
        let result = if settings.launch_at_login {
            manager.enable()
        } else {
            manager.disable()
        };
        if let Err(e) = result {
            return Err(format!("could not change launch-at-login: {e}"));
        }
    }

    let shortcut_changed = previous.shortcut != settings.shortcut;
    let hud_moved = previous.hud_position != settings.hud_position;
    let theme_changed = previous.theme != settings.theme;
    let device_changed = previous.input_device != settings.input_device;
    let dock_changed = previous.show_in_dock != settings.show_in_dock;
    let click_changed = previous.menubar_click_records != settings.menubar_click_records;
    let new_shortcut = settings.shortcut.clone();
    let new_device = settings.input_device.clone();
    let click_records = settings.menubar_click_records;
    let hud_position = settings.hud_position;

    *state.settings.lock().unwrap() = settings;

    if shortcut_changed {
        rebind(&app, &new_shortcut);
    }
    if hud_moved {
        hud::reposition(&app, hud_position);
    }
    if device_changed {
        swap_input_device(&app, new_device);
    }
    if click_changed {
        if let Some(tray) = state.tray.lock().unwrap().as_ref() {
            let _ = tray.set_show_menu_on_left_click(!click_records);
        }
    }
    #[cfg(target_os = "macos")]
    if dock_changed {
        // A settings window is open — this command came from it — so the policy stays Regular
        // either way right now. This makes the *next* close honour the new setting.
        apply_activation_policy(&app, true);
    }

    // The tray shows the shortcut and ticks the active microphone, so both changes reach it.
    if shortcut_changed || device_changed {
        let _ = refresh_tray_menu(&app, &new_shortcut);
    }
    // No theme event is emitted: the settings window applies it locally, and the HUD
    // deliberately does not follow the app theme — it has to stay readable over any window.
    let _ = theme_changed;

    Ok(())
}

#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.close();
    }
    // Back to a menu-bar-only app once the last real window is gone, or an empty Dock icon
    // and menu bar linger — unless the user asked to keep the Dock icon.
    #[cfg(target_os = "macos")]
    apply_activation_policy(&app, false);
}

/// Suspends the global hotkey while the settings window is capturing a new one.
///
/// Without this the OS consumes the currently-bound combination before the webview ever sees
/// the keystroke, so the user could never re-select or even observe their existing shortcut —
/// and pressing it would start a dictation instead of being recorded.
#[tauri::command]
fn set_capture_mode(app: AppHandle, capturing: bool) {
    let state = app.state::<App>();

    if capturing {
        if let Some(current) = state.bound.lock().unwrap().take() {
            let _ = app.global_shortcut().unregister(current);
        }
        // The tap has to stop too. With Fn or Caps Lock bound, pressing a key in the recorder
        // would otherwise start a dictation — and for Caps Lock the tap swallows the event, so
        // the recorder never saw it either.
        #[cfg(target_os = "macos")]
        {
            *state.special.lock().unwrap() = None;
        }
    } else {
        let accelerator = state.settings.lock().unwrap().shortcut.clone();
        rebind(&app, &accelerator);
    }
}

/* Models ------------------------------------------------------------------- */

#[tauri::command]
fn list_models(app: AppHandle) -> Vec<models::ModelInfo> {
    let active = app.state::<App>().settings.lock().unwrap().model.clone();
    models::list(&active)
}

#[tauri::command]
fn download_model(app: AppHandle, id: String) {
    let cancel = std::sync::Arc::new(AtomicBool::new(false));

    let state = app.state::<App>();
    {
        // Refuse a second download of the same model. Closing and reopening Settings loses the
        // frontend's progress map, so the row renders as "Download" again — and a second click
        // would start another thread writing the same .partial file. Two interleaved response
        // streams produce a corrupt model that still passes the size check on rename.
        let mut inflight = state.downloads.lock().unwrap();
        if inflight.contains_key(&id) {
            println!("[models] '{id}' is already downloading");
            return;
        }
        inflight.insert(id.clone(), cancel.clone());
    }

    // Downloads are hundreds of megabytes, so they run detached and report progress by event.
    // The settings window can be closed and reopened without interrupting one.
    std::thread::spawn(move || {
        let handle = app.clone();
        let result = models::download(&id, cancel, |progress| {
            let _ = handle.emit("model:progress", progress);
        });

        if let Err(e) = result {
            let _ = app.emit(
                "model:progress",
                models::Progress {
                    id: id.clone(),
                    received_mb: 0,
                    total_mb: 0,
                    done: false,
                    error: Some(format!("{e:#}")),
                },
            );
        }

        app.state::<App>().downloads.lock().unwrap().remove(&id);
    });
}

#[tauri::command]
fn cancel_download(app: AppHandle, id: String) {
    if let Some(flag) = app.state::<App>().downloads.lock().unwrap().get(&id) {
        flag.store(true, Ordering::SeqCst);
    }
}

#[tauri::command]
fn delete_model(app: AppHandle, id: String) -> Result<(), String> {
    if app.state::<App>().settings.lock().unwrap().model == id {
        return Err("That model is in use. Pick a different one first.".into());
    }
    models::delete(&id).map_err(|e| format!("{e:#}"))
}

/// Loads a different model and makes it active.
///
/// The engine is swapped only after the new one loads, so a failure leaves the app still working
/// with the previous model rather than in a state where dictation is broken.
#[tauri::command]
fn set_active_model(app: AppHandle, id: String) -> Result<(), String> {
    let state = app.state::<App>();

    {
        let _guard = state.engine_load.lock().unwrap();
        let already = state
            .engine
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|e| e.model_id() == id);

        // Load before persisting. A failure then leaves the app still working with whatever it
        // had, rather than pointing the settings at a model that would not load next start.
        if !already {
            load_model(state.inner(), &id)?;
        }
    }

    let mut settings = state.settings.lock().unwrap().clone();
    settings.model = id;
    settings.save().map_err(|e| format!("{e:#}"))?;
    *state.settings.lock().unwrap() = settings;

    Ok(())
}

/* Hotkey ------------------------------------------------------------------- */

/// Unregisters the previous binding before registering the new one, and reports failure loudly.
///
/// A silently failed binding is the worst outcome: the key falls through to the focused app and
/// types characters into the user's document.
fn rebind(app: &AppHandle, accelerator: &str) {
    let state = app.state::<App>();

    if let Some(old) = state.bound.lock().unwrap().take() {
        let _ = app.global_shortcut().unregister(old);
    }

    // Tear down any running tap before deciding on the new binding.
    #[cfg(target_os = "macos")]
    {
        *state.special.lock().unwrap() = None;

        // Fn and Caps Lock are modifier flags rather than keycodes, so Carbon cannot express
        // them. They take the event-tap path instead (see specialkey.rs).
        if let Some(key) = specialkey::SpecialKey::parse(accelerator) {
            let handle = app.clone();
            let monitor = specialkey::Monitor::start(key, move |pressed| {
                // The event tap delivers on its own thread, but the work downstream must run on
                // the main thread: text injection goes through HIToolbox's input-source APIs,
                // which assert they are on the main queue and abort the process otherwise.
                //
                // The Carbon hotkey path never hit this because it already dispatches on main.
                let inner = handle.clone();
                let _ = handle.run_on_main_thread(move || {
                    if pressed {
                        on_press(&inner);
                    } else {
                        on_release(&inner);
                    }
                });
            });
            *state.special.lock().unwrap() = Some(monitor);
            state.set_status(&state.idle_status());
            println!("[whisper-lite] bound {accelerator} (event tap)");
            return;
        }
    }

    let shortcut: Shortcut = match accelerator.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[whisper-lite] '{accelerator}' is not a valid shortcut: {e}");
            state.set_status("Invalid shortcut");
            return;
        }
    };

    match app.global_shortcut().register(shortcut) {
        Ok(()) => {
            *state.bound.lock().unwrap() = Some(shortcut);
            state.set_status(&state.idle_status());
            println!("[whisper-lite] bound {accelerator}");
        }
        Err(e) => {
            eprintln!("[whisper-lite] could not bind {accelerator}: {e}");
            eprintln!("  Another app is probably using it. Pick another in Settings.");
            state.set_status("Shortcut unavailable");
        }
    }
}

fn on_press(app: &AppHandle) {
    let state = app.state::<App>();
    *state.pressed_at.lock().unwrap() = Some(Instant::now());

    let activation = state.settings.lock().unwrap().activation;

    // A press while already recording ends the utterance in any mode that can latch. Previously
    // only Toggle did this, so in Both a quick tap latched recording on and the next tap could
    // not stop it — the user had to hold the key past the tap threshold instead, contradicting
    // what the mode advertises.
    if activation != Activation::Hold && state.recording.load(Ordering::SeqCst) {
        finish(app);
        return;
    }

    begin(app);
}

fn on_release(app: &AppHandle) {
    let state = app.state::<App>();

    let activation = state.settings.lock().unwrap().activation;
    let threshold = state.settings.lock().unwrap().tap_threshold_ms;
    let held = state
        .pressed_at
        .lock()
        .unwrap()
        .take()
        .map(|t| t.elapsed().as_millis() as u64)
        .unwrap_or(u64::MAX);

    match activation {
        // Toggle mode ignores release entirely — the next press stops it.
        Activation::Toggle => {}
        Activation::Hold => finish(app),
        Activation::Both => {
            // A quick tap latches into toggle mode; anything longer behaves as push-to-talk.
            if held >= threshold {
                finish(app);
            }
        }
    }
}

/// Leaves a message on screen briefly, then hides the HUD.
///
/// Errors that vanish instantly may as well not have been shown, but a HUD that stays up gets in
/// the way — so they linger just long enough to read.
/// Updates the HUD, unless the user has turned the indicator off.
///
/// `show_at` checked this but `set_state` did not, so someone with the indicator disabled still
/// got the pill for "Transcribing…", "No audio" and the error states.
fn hud_state(app: &AppHandle, state: &'static str, label: Option<String>) {
    let hidden =
        app.state::<App>().settings.lock().unwrap().hud_position == settings::HudPosition::Hidden;
    if hidden && state != "idle" {
        return;
    }
    hud::set_state(app, state, label);
}

fn clear_hud_after(app: &AppHandle, millis: u64) {
    let handle = app.clone();
    // Capture the current generation; if the HUD has moved on by the time this fires, another
    // dictation has started and hiding it would be wrong.
    let scheduled_at = hud::generation();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(millis));
        if hud::generation() == scheduled_at {
            hud::set_state(&handle, "idle", None);
        }
    });
}

fn begin(app: &AppHandle) {
    let state = app.state::<App>();

    if state.busy.load(Ordering::SeqCst) || state.recording.load(Ordering::SeqCst) {
        return;
    }

    // Refuse before recording rather than after. Letting someone talk for ten seconds and only
    // then saying there is no model wastes their time and loses what they said — and on a fresh
    // install this is the very first thing they will try.
    let model = state.settings.lock().unwrap().model.clone();
    if !models::is_installed(&model) {
        println!("[whisper-lite] no model installed — sending the user to Settings");
        state.set_status("No model — open Settings");
        hud_state(app, "error", Some("No model — opening Settings".into()));
        clear_hud_after(app, 2600);
        open_settings_window(app);
        return;
    }

    if let Err(e) = state.recorder.lock().unwrap().start() {
        eprintln!("[whisper-lite] could not start capture: {e:#}");
        state.set_status("Microphone error");
        hud_state(app, "error", Some("Microphone unavailable".into()));
        // Without this the pill stays on screen for good: it is always-on-top and click-through,
        // so there is nothing the user can do to dismiss it.
        clear_hud_after(app, 3000);
        return;
    }

    {
        let cfg = state.settings.lock().unwrap();
        if cfg.play_sounds {
            sound::play(&cfg.sound_start);
        }
    }

    state.recording.store(true, Ordering::SeqCst);
    println!("[whisper-lite] listening");
    state.set_status("Listening…");
    let position = state.settings.lock().unwrap().hud_position;
    hud::show_at(app, position, "listening", None);

    // Pump the meter to the HUD for as long as we are recording. Reading the level is a relaxed
    // atomic load, so this never touches the audio thread's path.
    let handle = app.clone();
    let meter = state.recorder.lock().unwrap().level_handle();
    std::thread::spawn(move || {
        loop {
            if !handle.state::<App>().recording.load(Ordering::SeqCst) {
                break;
            }
            let level = f32::from_bits(meter.load(Ordering::Relaxed));
            hud::set_tick(&handle, level);
            // 60fps. The meter is a relaxed atomic load, so this costs nothing beyond the IPC.
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
}

fn finish(app: &AppHandle) {
    let state = app.state::<App>();

    if !state.recording.swap(false, Ordering::SeqCst) {
        return;
    }
    if state.busy.swap(true, Ordering::SeqCst) {
        return;
    }

    let released_at = Instant::now();

    {
        let cfg = state.settings.lock().unwrap();
        if cfg.play_sounds {
            sound::play(&cfg.sound_stop);
        }
    }

    let samples = match state.recorder.lock().unwrap().stop() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[whisper-lite] could not stop capture: {e:#}");
            state.busy.store(false, Ordering::SeqCst);
            hud_state(app, "error", Some("Microphone error".into()));
            clear_hud_after(app, 3000);
            return;
        }
    };

    let seconds = samples.len() as f32 / audio::TARGET_RATE as f32;
    state.set_status("Transcribing…");
    hud_state(app, "working", Some("Transcribing…".into()));

    // Distinguish a dead microphone from a user who simply did not speak. Without this both
    // look identical: nothing appears, and there is no way to tell which went wrong.
    let peak = audio::peak(&samples);
    if peak < audio::SILENCE_PEAK {
        println!("[whisper-lite] no audio captured ({seconds:.1}s, peak {peak:.4})");
        state.busy.store(false, Ordering::SeqCst);
        state.set_status("No audio");
        hud_state(
            app,
            "error",
            Some("No audio — check your microphone".into()),
        );
        clear_hud_after(app, 2600);
        return;
    }

    let (remove_fillers, autocapitalize, language, accurate, dictionary, snippets) = {
        let cfg = state.settings.lock().unwrap();
        (
            cfg.remove_fillers,
            cfg.autocapitalize,
            cfg.language.clone(),
            cfg.accurate,
            cfg.dictionary.join(", "),
            cfg.snippets
                .iter()
                .map(|s| (s.trigger.clone(), s.replacement.clone()))
                .collect::<Vec<_>>(),
        )
    };
    // Everything from here runs off the main thread.
    //
    // Transcription blocks for hundreds of milliseconds, and Tauri delivers events to the webview
    // by scheduling `evaluateJavaScript` on the main queue. Running it here meant the "working"
    // state emitted just above could not be painted until *after* the work finished — so the
    // spinner never appeared at all, the pill sat in its listening state throughout, and every
    // HUD change arrived in one burst at the end.
    let handle = app.clone();
    std::thread::spawn(move || {
        let state = handle.state::<App>();

        // The model may still be warming up, or may have been downloaded after startup. This is
        // the last point at which loading it is still cheaper than losing the recording.
        let result = match ensure_engine(&handle) {
            Err(e) => Err(anyhow::anyhow!("{e}")),
            Ok(()) => {
                let guard = state.engine.lock().unwrap();
                match guard.as_ref() {
                    // `ensure_engine` just succeeded, so this is unreachable in practice.
                    None => Err(anyhow::anyhow!("no model loaded")),
                    Some(eng) => eng.transcribe(&samples, &language, accurate, &dictionary),
                }
            }
        };
        state.busy.store(false, Ordering::SeqCst);

        let outcome = match result {
            Err(e) => {
                eprintln!("[whisper-lite] transcription failed: {e:#}");
                Outcome::Failed
            }
            Ok(raw) => {
                let cleaned = text::expand_snippets(
                    &text::clean(&raw, remove_fillers, autocapitalize),
                    &snippets,
                );
                if cleaned.is_empty() {
                    // Audio was present but produced nothing usable — either genuinely
                    // unintelligible, or a silence hallucination the text filter discarded.
                    println!("[whisper-lite] nothing recognised ({seconds:.1}s, peak {peak:.3})");
                    Outcome::Nothing
                } else {
                    // File I/O, so it belongs out here rather than on the main thread.
                    let cfg = state.settings.lock().unwrap();
                    if cfg.history_enabled {
                        if let Err(e) = history::append(&cleaned, seconds, cfg.history_days) {
                            // Never let a history failure cost the user their text.
                            eprintln!("[history] could not record: {e:#}");
                        }
                    }
                    Outcome::Text(cleaned)
                }
            }
        };

        // One hop back for everything that touches the UI. Injection goes through HIToolbox's
        // input-source APIs, which abort the process off the main queue, and the tray menu item
        // has the same affinity — so the status and HUD updates ride along rather than racing it.
        let _ = handle.clone().run_on_main_thread(move || {
            let state = handle.state::<App>();
            let cleaned = match outcome {
                Outcome::Failed => {
                    state.set_status("Failed");
                    hud_state(&handle, "error", Some("Transcription failed".into()));
                    clear_hud_after(&handle, 3000);
                    return;
                }
                Outcome::Nothing => {
                    state.set_status(&state.idle_status());
                    hud_state(&handle, "error", Some("Didn't catch that".into()));
                    clear_hud_after(&handle, 1800);
                    return;
                }
                Outcome::Text(t) => t,
            };

            match inject::insert(&cleaned) {
                Ok(()) => {
                    // The number that matters: key release to text on screen.
                    println!(
                        "[whisper-lite] {seconds:.1}s audio → {:?} → \"{cleaned}\"",
                        released_at.elapsed()
                    );
                    state.set_status(&state.idle_status());
                    hud::set_state(&handle, "idle", None);
                }
                Err(e) => {
                    // Words are never lost — the text is on the clipboard either way.
                    eprintln!("[whisper-lite] injection failed: {e:#}");
                    state.set_status("Copied to clipboard");
                    hud_state(&handle, "error", Some("Copied — press ⌘V".into()));
                    clear_hud_after(&handle, 3000);
                }
            }
        });
    });
}

/// What transcription produced, carried from the worker thread back to the main thread.
enum Outcome {
    Text(String),
    /// The model itself errored.
    Failed,
    /// Audio was present but yielded nothing usable.
    Nothing,
}
