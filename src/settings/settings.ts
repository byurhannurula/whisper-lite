import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Mirrors `settings::Settings` in Rust. */
interface Settings {
  shortcut: string;
  activation: "hold" | "toggle" | "both";
  tapThresholdMs: number;
  removeFillers: boolean;
  language: string;
  hudPosition: HudPosition;
  theme: "system" | "light" | "dark";
  playSounds: boolean;
  launchAtLogin: boolean;
  model: string;
  accurate: boolean;
  dictionary: string[];
  snippets: Snippet[];
  soundStart: string;
  soundStop: string;
  inputDevice: string;
  autocapitalize: boolean;
  showInDock: boolean;
  menubarClickRecords: boolean;
  historyEnabled: boolean;
  historyDays: number;
}

interface Snippet {
  trigger: string;
  replacement: string;
}

interface ModelInfo {
  id: string;
  name: string;
  sizeMb: number;
  speed: string;
  accuracy: string;
  /** 1-5, for the comparison meters. */
  speedRank: number;
  accuracyRank: number;
  note: string;
  group: string;
  measured: boolean;
  installed: boolean;
  active: boolean;
}

interface ModelProgress {
  id: string;
  receivedMb: number;
  totalMb: number;
  done: boolean;
  error: string | null;
}

// Laid out three to a line on purpose: it mirrors the 3x3 anchor grid the user actually picks
// from, so the shape of the screen is visible in the type.
// prettier-ignore
type HudPosition =
  | "top-left" | "top-center" | "top-right"
  | "middle-left" | "center" | "middle-right"
  | "bottom-left" | "bottom-center" | "bottom-right"
  | "hidden";

const ANCHORS: { value: HudPosition; label: string }[] = [
  { value: "top-left", label: "Top left" },
  { value: "top-center", label: "Top centre" },
  { value: "top-right", label: "Top right" },
  { value: "middle-left", label: "Middle left" },
  { value: "center", label: "Centre" },
  { value: "middle-right", label: "Middle right" },
  { value: "bottom-left", label: "Bottom left" },
  { value: "bottom-center", label: "Bottom centre" },
  { value: "bottom-right", label: "Bottom right" },
];

/**
 * Combinations macOS already owns. Binding one means the key fires the system action as well as
 * ours — which is exactly what made Cmd+Option+Space open Finder.
 */
const SYSTEM_SHORTCUTS: Record<string, string> = {
  "Cmd+Space": "Spotlight",
  "Cmd+Option+Space": "Finder search",
  "Ctrl+Space": "previous input source",
  "Ctrl+Option+Space": "next input source",
  "Cmd+Ctrl+Space": "the emoji picker",
  "Cmd+Tab": "the app switcher",
  "Cmd+Q": "Quit",
  "Cmd+W": "Close window",
  "Cmd+H": "Hide",
  "Cmd+M": "Minimise",
};

/**
 * Combinations that must never be bound, as opposed to merely warned about. Binding Cmd+Q means
 * every dictation attempt quits the app; Cmd+W closes windows. A warning is not enough when the
 * consequence is destructive and immediate.
 */
const BLOCKED_SHORTCUTS: Record<string, string> = {
  "Cmd+Q": "quits apps",
  "Cmd+W": "closes windows",
  "Cmd+Tab": "switches apps",
  "Cmd+H": "hides apps",
  "Cmd+M": "minimises windows",
  "Cmd+Space": "opens Spotlight",
  "Cmd+Option+Escape": "opens Force Quit",
};

let settings: Settings;
let recording = false;

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

async function save(patch: Partial<Settings>) {
  // Re-read before merging rather than patching the cached copy.
  //
  // `save_settings` takes the whole struct, and this window is not the only thing that writes
  // one: the tray's microphone submenu changes the input device directly in Rust. Posting a
  // cache fetched at startup would silently revert whatever happened in between. On failure,
  // fall back to the cache — a stale write beats dropping the user's change entirely.
  try {
    settings = { ...(await invoke<Settings>("get_settings")), ...patch };
  } catch {
    settings = { ...settings, ...patch };
  }

  try {
    await invoke("save_settings", { settings });
    flash("Saved");
  } catch (e) {
    flash(`Could not save: ${e}`, "error");
  }
}

let toastTimer: number | undefined;

/**
 * Uses the native popover API so the toast lives in the browser's top layer. A plain
 * absolutely-positioned element would be clipped by the scrolling settings pane.
 */
function flash(message: string, tone: "ok" | "error" = "ok") {
  const toast = $<HTMLElement>("#toast");
  $("#toast-text").textContent = message;
  toast.dataset.tone = tone;
  $("#toast-icon").innerHTML =
    tone === "ok" ? '<path d="m5 13 4 4L19 7" />' : '<path d="M12 8v5M12 16.5v.5" />';

  // Re-showing an already-open popover throws, so close first.
  try {
    toast.hidePopover();
  } catch {
    /* not open */
  }
  toast.showPopover();

  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(
    () => {
      try {
        toast.hidePopover();
      } catch {
        /* already closed */
      }
    },
    tone === "ok" ? 1500 : 3200,
  );
}

/* Sidebar ------------------------------------------------------------------ */

let tabs: HTMLButtonElement[] = [];

function selectTab(tab: HTMLButtonElement) {
  for (const t of tabs) {
    const on = t === tab;
    t.setAttribute("aria-selected", String(on));
    document.getElementById(t.getAttribute("aria-controls")!)!.hidden = !on;
  }

  // Both sections are views over data that changes while the window is open.
  if (tab.id === "tab-history") void loadHistory();
  if (tab.id === "tab-home") renderHome();
}

/** Navigation from somewhere other than the sidebar — the Home checklist, mostly. */
function goTo(tabId: string) {
  const tab = document.getElementById(tabId) as HTMLButtonElement | null;
  if (tab) selectTab(tab);
}

function initTabs() {
  tabs = Array.from(document.querySelectorAll<HTMLButtonElement>('[role="tab"]'));

  tabs.forEach((tab) => {
    tab.addEventListener("click", () => selectTab(tab));
    tab.addEventListener("keydown", (e) => {
      const dir = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
      if (!dir) return;
      e.preventDefault();
      const next = tabs[(tabs.indexOf(tab) + dir + tabs.length) % tabs.length];
      next.focus();
      selectTab(next);
    });
  });
}

/* Sidebar collapse --------------------------------------------------------- */

/**
 * ⌘B hides the sidebar, as it does in Finder, Xcode and Mail.
 *
 * Kept in localStorage rather than the settings file: it is view state for this window, not a
 * preference the rest of the app has any use for, and round-tripping it through Rust would fire
 * a "Saved" toast every time the sidebar moved.
 */
function initSidebar() {
  const button = $<HTMLButtonElement>("#sidebar-toggle");

  const apply = (collapsed: boolean) => {
    document.body.dataset.sidebar = collapsed ? "collapsed" : "expanded";
    button.setAttribute("aria-label", collapsed ? "Show sidebar" : "Hide sidebar");
    localStorage.setItem("sidebar", collapsed ? "collapsed" : "expanded");
  };

  const toggle = () => apply(document.body.dataset.sidebar !== "collapsed");

  apply(localStorage.getItem("sidebar") === "collapsed");
  button.addEventListener("click", toggle);

  window.addEventListener("keydown", (e) => {
    // The shortcut recorder is capturing keystrokes; ⌘B belongs to it while it is open.
    if (recording) return;
    if (e.metaKey && e.key.toLowerCase() === "b") {
      e.preventDefault();
      toggle();
    }
  });
}

/* Shortcut recorder -------------------------------------------------------- */

const SYMBOL: Record<string, string> = {
  Cmd: "⌘",
  Ctrl: "⌃",
  Option: "⌥",
  Shift: "⇧",
  Fn: "🌐 Fn",
  CapsLock: "⇪ Caps Lock",
};

/**
 * Keys that are modifier *flags* rather than keycodes.
 *
 * Carbon's RegisterEventHotKey — what the normal path uses — takes a keycode plus modifiers, so
 * it cannot express these at all. They are handled by a CGEventTap instead, which is why they
 * are allowed to stand alone with no modifier.
 */
const SINGLE_KEYS: Record<string, { label: string; caveat: string }> = {
  Fn: {
    label: "🌐 Fn",
    caveat:
      "Set “Press 🌐 to: Do Nothing” in System Settings → Keyboard, or Fn will also open the " +
      "emoji picker.",
  },
  CapsLock: {
    label: "⇪ Caps Lock",
    caveat:
      "Caps Lock stops toggling capitals while Whisper Lite is running — the keypress is " +
      "consumed.",
  },
};

/** Named keys whose accelerator name differs from the physical `event.code`. */
const NAMED_KEYS: Record<string, string> = {
  Space: "Space",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  Escape: "Escape",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Minus: "Minus",
  Equal: "Equal",
  BracketLeft: "BracketLeft",
  BracketRight: "BracketRight",
  Semicolon: "Semicolon",
  Quote: "Quote",
  Backquote: "Backquote",
  Backslash: "Backslash",
  Comma: "Comma",
  Period: "Period",
  Slash: "Slash",
};

function pretty(accelerator: string): string {
  if (SINGLE_KEYS[accelerator]) return SINGLE_KEYS[accelerator].label;
  return accelerator
    .split("+")
    .map((part) => SYMBOL[part] ?? part)
    .join(" ");
}

function modifiersOf(e: KeyboardEvent): string[] {
  const mods: string[] = [];
  if (e.metaKey) mods.push("Cmd");
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Option");
  if (e.shiftKey) mods.push("Shift");
  return mods;
}

/** Physical key name for an accelerator, independent of keyboard layout and modifiers. */
function keyOf(code: string): string | null {
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  if (/^F\d{1,2}$/.test(code)) return code;
  if (code.startsWith("Numpad")) return code;
  return NAMED_KEYS[code] ?? null;
}

const MODIFIER_KEYS = new Set(["Meta", "Control", "Alt", "Shift"]);

function renderShortcut() {
  $(".recorder-keys").textContent = pretty(settings.shortcut);
  // The Home checklist shows the same binding, so it has to move with it.
  $("#task-shortcut").textContent = pretty(settings.shortcut);

  const note = $("#shortcut-note");
  const single = SINGLE_KEYS[settings.shortcut];
  const conflict = SYSTEM_SHORTCUTS[settings.shortcut];

  if (single) {
    note.textContent = single.caveat;
    note.classList.remove("warn");
  } else if (conflict) {
    note.textContent = `macOS already uses this for ${conflict} — both will fire. Pick another.`;
    note.classList.add("warn");
  } else {
    note.textContent = "Click, then press the keys you want to use — or a single key like 🌐 Fn.";
    note.classList.remove("warn");
  }
}

function initRecorder() {
  const button = $<HTMLButtonElement>("#shortcut");
  const keys = $(".recorder-keys");
  const note = $("#shortcut-note");

  const stop = async () => {
    if (!recording) return;
    recording = false;
    button.dataset.recording = "false";
    // Re-arm the real hotkey.
    await invoke("set_capture_mode", { capturing: false }).catch(() => {});
    renderShortcut();
  };

  const start = async () => {
    if (recording) return;
    recording = true;
    button.dataset.recording = "true";
    keys.textContent = "Press keys…";
    note.textContent = "Add ⌘, ⌃, ⌥ or ⇧, then a key. Esc to cancel.";
    note.classList.remove("warn");
    // Suspend our own global shortcut, otherwise the OS swallows it and the webview never sees
    // the keystroke — which made the current combination impossible to re-select.
    await invoke("set_capture_mode", { capturing: true }).catch(() => {});
  };

  button.addEventListener("click", () => void start());
  button.addEventListener("blur", () => void stop());

  // The browser never delivers a keydown for Fn — it is a hardware modifier that only changes
  // flag state — so it cannot be captured by listening. It gets a button instead.
  $("#use-fn").addEventListener("click", () => {
    void stop()
      .then(() => save({ shortcut: "Fn" }))
      .then(renderShortcut);
  });
  $("#use-capslock").addEventListener("click", () => {
    void stop()
      .then(() => save({ shortcut: "CapsLock" }))
      .then(renderShortcut);
  });

  window.addEventListener(
    "keydown",
    (e) => {
      if (!recording) return;
      e.preventDefault();
      e.stopPropagation();

      if (e.key === "Escape" && modifiersOf(e).length === 0) {
        void stop();
        return;
      }

      // Caps Lock reports as its own key. Accept it immediately — it is a valid single-key
      // binding and there is nothing further to wait for.
      if (e.code === "CapsLock") {
        void stop()
          .then(() => save({ shortcut: "CapsLock" }))
          .then(renderShortcut);
        return;
      }

      const mods = modifiersOf(e);

      // Show modifiers as they go down, so it is obvious the recorder is listening.
      if (MODIFIER_KEYS.has(e.key)) {
        keys.textContent = mods.length ? pretty(mods.join("+")) + " …" : "Press keys…";
        return;
      }

      const key = keyOf(e.code);

      if (!key) {
        note.textContent = "That key can't be used. Try a letter, number or F-key.";
        note.classList.add("warn");
        return;
      }

      if (mods.length === 0) {
        keys.textContent = key;
        note.textContent = "Add ⌘, ⌃, ⌥ or ⇧ — a plain key would fire while you type.";
        note.classList.add("warn");
        return;
      }

      const accelerator = [...mods, key].join("+");

      const blocked = BLOCKED_SHORTCUTS[accelerator];
      if (blocked) {
        keys.textContent = pretty(accelerator);
        note.textContent = `${pretty(accelerator)} ${blocked} — pick something else.`;
        note.classList.add("warn");
        return;
      }

      void stop()
        .then(() => save({ shortcut: accelerator }))
        .then(renderShortcut);
    },
    true, // capture phase, so nothing else swallows the keystroke first
  );

  // Repaint the pending modifiers as they are released.
  window.addEventListener("keyup", (e) => {
    if (!recording || !MODIFIER_KEYS.has(e.key)) return;
    const mods = modifiersOf(e);
    keys.textContent = mods.length ? pretty(mods.join("+")) + " …" : "Press keys…";
  });
}

/* Home --------------------------------------------------------------------- */

/**
 * Typing speed the "time saved" figure is measured against.
 *
 * 40wpm is the commonly cited average for sustained prose on a full keyboard. It is a stated
 * assumption rather than a measurement — we have no way to know what the user would have typed —
 * so the label says "saved", not "saved exactly".
 */
const TYPING_WPM = 40;

const DAYS_SHOWN = 7;

function wordCount(text: string): number {
  const trimmed = text.trim();
  return trimmed ? trimmed.split(/\s+/).length : 0;
}

function durationLabel(seconds: number): string {
  const minutes = Math.round(seconds / 60);
  if (minutes < 1) return "—";
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest ? `${hours}h ${rest}m` : `${hours} ${hours === 1 ? "hour" : "hours"}`;
}

/**
 * Rebuilds the Home dashboard from the transcript history.
 *
 * Deliberately computed here rather than in Rust: bucketing by *local* day needs the user's
 * timezone and DST rules, which the webview already has and which Rust would need a date library
 * to get right. The history is loaded for the History section anyway, so this is free.
 */
/** Days the stat tiles cover. 0 means everything the history still holds. */
let statRangeDays = 7;

function renderHome() {
  const now = new Date();
  const dayStart = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

  // Calendar arithmetic rather than subtracting 86400000, so the two days a year that are not
  // 24 hours long still fall on the right side of the boundary.
  const rangeStart =
    statRangeDays === 0
      ? 0
      : new Date(now.getFullYear(), now.getMonth(), now.getDate() - (statRangeDays - 1)).getTime();

  let words = 0;
  let seconds = 0;
  let count = 0;

  for (const entry of historyCache) {
    if (entry.at * 1000 < rangeStart) continue;
    words += wordCount(entry.text);
    seconds += entry.duration;
    count += 1;
  }

  // Speaking pace, not transcription throughput: words divided by how long the user talked.
  const wpm = seconds > 0 ? Math.round(words / (seconds / 60)) : 0;
  // What typing the same words would have cost, less the time actually spent speaking.
  const saved = Math.max(0, (words / TYPING_WPM) * 60 - seconds);

  $("#stat-wpm").textContent = wpm > 0 ? String(wpm) : "—";
  $("#stat-words").textContent = words > 0 ? words.toLocaleString() : "—";
  $("#stat-count").textContent = count > 0 ? String(count) : "—";
  $("#stat-saved").textContent = durationLabel(saved);

  // The chart is always the last seven days whatever the tiles are showing — it is a shape, and
  // rescaling it to "all time" would compress a year into seven bars and say nothing.
  const days = Array.from({ length: DAYS_SHOWN }, (_, i) => {
    const date = new Date(now.getFullYear(), now.getMonth(), now.getDate() - (DAYS_SHOWN - 1 - i));
    return { date, start: date.getTime(), words: 0 };
  });

  let weekWords = 0;
  for (const entry of historyCache) {
    const at = entry.at * 1000;
    if (at < days[0].start) continue;
    const spoken = wordCount(entry.text);
    const bucket = days.find((d) => d.start === dayStart(new Date(at)));
    if (bucket) {
      bucket.words += spoken;
      weekWords += spoken;
    }
  }

  $("#chart-total").textContent =
    weekWords > 0 ? `${weekWords.toLocaleString()} words` : "Nothing yet this week";

  const peak = Math.max(...days.map((d) => d.words), 1);
  const chart = $("#chart");
  chart.replaceChildren();

  const today = days[days.length - 1].start;

  for (const day of days) {
    const col = document.createElement("div");
    col.className = "chart-col";

    const bar = document.createElement("div");
    bar.className = "bar";
    bar.style.height = `${Math.max(3, (day.words / peak) * 100)}%`;
    bar.dataset.empty = String(day.words === 0);

    const label = document.createElement("div");
    label.className = "chart-day";
    label.textContent = day.date.toLocaleDateString(undefined, { weekday: "narrow" });
    label.dataset.today = String(day.start === today);

    col.append(bar, label);
    chart.append(col);
  }

  $("#chart-summary").textContent = days
    .map((d) => `${d.date.toLocaleDateString(undefined, { weekday: "long" })}: ${d.words} words`)
    .join(". ");

  // The numbers are only as complete as the history they come from, so say when it is off.
  $("#stats-off").hidden = settings.historyEnabled;
}

function initHome() {
  $<HTMLSelectElement>("#stat-range").addEventListener("change", (e) => {
    statRangeDays = Number((e.target as HTMLSelectElement).value);
    renderHome();
  });

  $("#task-dictate").addEventListener("click", () => goTo("tab-config"));
  $("#task-model").addEventListener("click", () => goTo("tab-models"));
  $("#task-words").addEventListener("click", () => goTo("tab-dictionary"));
  $("#task-hud").addEventListener("click", () => goTo("tab-config"));
}

/* Microphone --------------------------------------------------------------- */

/**
 * The picker appears twice — once in the toolbar, once in Sound — because it is worth reaching
 * from anywhere but also belongs with the other audio settings. Both are driven from here so
 * they can never disagree.
 */
const DEVICE_PICKERS = ["#inputDevice", "#inputDeviceMirror"];

async function initDevices() {
  const names = await invoke<string[]>("input_devices").catch(() => [] as string[]);

  for (const id of DEVICE_PICKERS) {
    const select = $<HTMLSelectElement>(id);
    select.replaceChildren();
    select.append(new Option("System default", ""));
    for (const name of names) select.append(new Option(name, name));

    // A saved device that is currently unplugged still has to appear, or the picker would show
    // "System default" while the settings file says otherwise — and silently adopt it on save.
    if (settings.inputDevice && !names.includes(settings.inputDevice)) {
      select.append(new Option(`${settings.inputDevice} (not connected)`, settings.inputDevice));
    }

    select.value = settings.inputDevice;
    select.addEventListener("change", () => {
      void save({ inputDevice: select.value }).then(syncDevices);
    });
  }
}

function syncDevices() {
  for (const id of DEVICE_PICKERS) $<HTMLSelectElement>(id).value = settings.inputDevice;
}

/* Appearance --------------------------------------------------------------- */

let lastVisibleAnchor: HudPosition = "bottom-right";

function initAnchors() {
  const grid = $(".anchor-grid");

  for (const anchor of ANCHORS) {
    const cell = document.createElement("button");
    cell.className = "anchor";
    cell.setAttribute("role", "radio");
    cell.setAttribute("aria-label", anchor.label);
    cell.setAttribute("aria-checked", "false");
    cell.dataset.value = anchor.value;
    cell.addEventListener("click", () => {
      void save({ hudPosition: anchor.value }).then(renderAnchors);
    });
    grid.appendChild(cell);
  }

  $<HTMLInputElement>("#hudVisible").addEventListener("change", (e) => {
    const visible = (e.target as HTMLInputElement).checked;
    // Remember the previous anchor so re-enabling restores it rather than jumping to a default.
    void save({ hudPosition: visible ? lastVisibleAnchor : "hidden" }).then(renderAnchors);
  });
}

function renderAnchors() {
  const hidden = settings.hudPosition === "hidden";
  if (!hidden) lastVisibleAnchor = settings.hudPosition;

  $<HTMLInputElement>("#hudVisible").checked = !hidden;
  $(".anchor-grid").setAttribute("aria-disabled", String(hidden));

  document.querySelectorAll<HTMLElement>(".anchor").forEach((cell) => {
    cell.setAttribute(
      "aria-checked",
      String(!hidden && cell.dataset.value === settings.hudPosition),
    );
  });
}

function applyTheme() {
  const root = document.documentElement;
  if (settings.theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", settings.theme);
}

/* Models ------------------------------------------------------------------- */

const downloading = new Map<string, ModelProgress>();

function sizeLabel(mb: number): string {
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb} MB`;
}

let modelFilter: "all" | "installed" = "all";
let firstModelRender = true;

/**
 * A five-segment bar for one axis of a model's trade-off.
 *
 * The words ("Good", "Very good") cannot be ranked against each other at a glance, which is the
 * only thing this list is for. They survive as the accessible name so the meaning is not lost to
 * anyone reading with a screen reader.
 */
function meter(kind: "speed" | "accuracy", rank: number, description: string): HTMLElement {
  const label = kind === "speed" ? "Speed" : "Accuracy";

  const wrap = document.createElement("div");
  wrap.className = "meter";
  wrap.dataset.kind = kind;

  const name = document.createElement("span");
  name.className = "meter-label";
  name.textContent = label;

  const track = document.createElement("div");
  track.className = "meter-track";
  track.setAttribute("role", "img");
  track.setAttribute("aria-label", `${label}: ${description}`);

  for (let i = 1; i <= 5; i++) {
    const segment = document.createElement("i");
    segment.dataset.on = String(i <= rank);
    track.append(segment);
  }

  wrap.append(name, track);
  return wrap;
}

function modelRow(m: ModelInfo): HTMLElement {
  const row = document.createElement("div");
  row.className = "model";
  row.dataset.modelId = m.id;

  const info = document.createElement("div");
  info.className = "model-info";

  const name = document.createElement("div");
  name.className = "model-name";
  name.append(m.name);
  if (m.active) {
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = "In use";
    name.append(badge);
  } else if (m.installed) {
    const badge = document.createElement("span");
    badge.className = "badge badge--muted";
    badge.textContent = "Downloaded";
    name.append(badge);
  }

  const note = document.createElement("div");
  note.className = "model-note";
  note.textContent = m.note;

  const meters = document.createElement("div");
  meters.className = "model-meters";

  const size = document.createElement("span");
  size.className = "model-size";
  // Megabytes trail the meters: nobody picks a model by file size, but it decides whether the
  // download is worth starting.
  size.textContent = sizeLabel(m.sizeMb) + (m.measured ? "" : " · speed estimated");

  meters.append(
    meter("speed", m.speedRank, m.speed),
    meter("accuracy", m.accuracyRank, `${m.accuracy} accuracy`),
    size,
  );

  info.append(name, note, meters);

  const actions = document.createElement("div");
  actions.className = "model-actions";

  const progress = downloading.get(m.id);

  if (progress && !progress.done) {
    const wrap = document.createElement("div");
    wrap.className = "progress";
    const track = document.createElement("div");
    track.className = "progress-track";
    const fill = document.createElement("div");
    fill.className = "progress-fill";
    const pct = progress.totalMb ? (progress.receivedMb / progress.totalMb) * 100 : 0;
    fill.style.width = `${pct}%`;
    track.append(fill);

    const text = document.createElement("span");
    text.className = "progress-text";
    text.textContent = `${progress.receivedMb} / ${progress.totalMb} MB`;

    const cancel = document.createElement("button");
    cancel.className = "btn btn--quiet";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => void invoke("cancel_download", { id: m.id }));

    wrap.append(track, text);
    actions.append(wrap, cancel);
  } else if (!m.installed) {
    actions.append(
      iconAction("download", `Download ${m.name}`, () => {
        downloading.set(m.id, {
          id: m.id,
          receivedMb: 0,
          totalMb: m.sizeMb,
          done: false,
          error: null,
        });
        void invoke("download_model", { id: m.id });
        void renderModels();
      }),
    );
  } else if (!m.active) {
    // "Use" is the only real decision in the row, so it is the only labelled control.
    const use = document.createElement("button");
    use.className = "btn btn--primary";
    use.textContent = "Use";
    use.addEventListener("click", async () => {
      use.disabled = true;
      use.textContent = "Loading…";
      try {
        await invoke("set_active_model", { id: m.id });
        settings.model = m.id;
        flash(`Now using ${m.name}`);
      } catch (e) {
        flash(String(e), "error");
      }
      await renderModels();
    });

    const del = iconAction("delete", `Delete ${m.name}`, async () => {
      try {
        await invoke("delete_model", { id: m.id });
        flash(`Deleted ${m.name}`);
      } catch (e) {
        flash(String(e), "error");
      }
      await renderModels();
    });
    del.classList.add("icon-action--danger");

    actions.append(use, del);
  }

  row.append(info, actions);
  return row;
}

const ACTION_GLYPHS: Record<string, string> = {
  download: '<circle cx="12" cy="12" r="9" /><path d="M12 7.6v7.2m0 0 3.1-3.1M12 14.8l-3.1-3.1" />',
  delete: '<path d="M4.5 6.8h15M9.6 6.8V4.9h4.8v1.9M6.8 6.8l.8 12.3h8.8l.8-12.3" />',
};

/**
 * A round glyph button for an action whose meaning does not need a word.
 *
 * The label still reaches assistive technology and the tooltip, so nothing is lost by dropping
 * the visible text — the icons carry it for sighted users and the row stops looking like a form.
 */
function iconAction(
  glyph: keyof typeof ACTION_GLYPHS | string,
  label: string,
  onClick: () => void,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.className = "icon-action";
  button.title = label;
  button.setAttribute("aria-label", label);
  button.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${ACTION_GLYPHS[glyph]}</svg>`;
  button.addEventListener("click", onClick);
  return button;
}

async function renderModels() {
  const list = $("#model-list");
  const models = await invoke<ModelInfo[]>("list_models");

  list.replaceChildren();

  // The Home checklist names the model in use, so it is obvious what is doing the work.
  const active = models.find((m) => m.active);
  $("#task-model-note").textContent = active
    ? `Using ${active.name} — ${active.speed}, ${active.accuracy} accuracy.`
    : "No model is loaded yet. Download one to start dictating.";

  const installed = models.filter((m) => m.installed);

  // A fresh install has no model, and nothing in the app works until it does. Open on the section
  // that fixes that rather than on a dashboard of dashes. Only on the first render, so choosing
  // to delete every model later does not yank the user out of whatever they were doing.
  if (firstModelRender) {
    firstModelRender = false;
    if (installed.length === 0) goTo("tab-models");
  }

  const onDisk = installed.reduce((total, m) => total + m.sizeMb, 0);
  $("#model-summary").textContent = installed.length
    ? `${installed.length} downloaded · ${sizeLabel(onDisk)} on disk`
    : "Nothing downloaded yet";

  const shown = modelFilter === "installed" ? installed : models;

  if (shown.length === 0) {
    const card = document.createElement("div");
    card.className = "card";
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = "No models downloaded yet.";
    card.append(empty);
    list.append(card);
    return;
  }

  // Registry order decides both the shelves and the order within them, so the list reads
  // smallest-to-largest without the frontend having to know the sizes.
  const groups: string[] = [];
  for (const m of shown) if (!groups.includes(m.group)) groups.push(m.group);

  for (const group of groups) {
    const heading = document.createElement("div");
    heading.className = "model-group";
    heading.textContent = group;

    const card = document.createElement("div");
    card.className = "card";
    for (const m of shown.filter((x) => x.group === group)) card.append(modelRow(m));

    list.append(heading, card);
  }
}

function initModels() {
  document.querySelectorAll<HTMLInputElement>('input[name="modelfilter"]').forEach((input) => {
    input.addEventListener("change", () => {
      if (!input.checked) return;
      modelFilter = input.value as typeof modelFilter;
      void renderModels();
    });
  });
}

void listen<ModelProgress>("model:progress", (e) => {
  const p = e.payload;
  if (p.error) {
    downloading.delete(p.id);
    flash(p.error === "cancelled" ? "Download cancelled" : p.error, "error");
    void renderModels();
    return;
  }
  if (p.done) {
    downloading.delete(p.id);
    flash("Download complete");
    void renderModels();
    return;
  }
  downloading.set(p.id, p);

  // A download started before this window opened is not in the local map, so the row is showing
  // a Download button rather than a progress bar. Re-render once to pick it up.
  const row = document.querySelector<HTMLElement>(`.model[data-model-id="${p.id}"]`);
  if (!row?.querySelector(".progress-fill")) {
    void renderModels();
    return;
  }

  // Update this row's bar in place. Re-rendering the whole list on every megabyte would fight
  // the user's clicks and reset focus — and a document-wide selector drove the *first* bar in
  // the DOM rather than this model's, so two downloads updated each other's progress.
  const fill = row.querySelector<HTMLElement>(".progress-fill");
  const text = row.querySelector<HTMLElement>(".progress-text");
  if (fill && p.totalMb) fill.style.width = `${(p.receivedMb / p.totalMb) * 100}%`;
  if (text) text.textContent = `${p.receivedMb} / ${p.totalMb} MB`;
});

/* Dictionary and snippets --------------------------------------------------- */

function renderWords() {
  const list = $("#word-list");
  list.replaceChildren();

  if (settings.dictionary.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = "No words yet. Add the names and jargon you dictate often.";
    list.append(empty);
    return;
  }

  for (const word of settings.dictionary) {
    const row = document.createElement("div");
    row.className = "entry";

    const main = document.createElement("div");
    main.className = "entry-main";
    main.textContent = word;

    const remove = document.createElement("button");
    remove.className = "entry-remove";
    remove.textContent = "×";
    remove.setAttribute("aria-label", `Remove ${word}`);
    remove.addEventListener("click", () => {
      void save({ dictionary: settings.dictionary.filter((w) => w !== word) }).then(renderWords);
    });

    row.append(main, remove);
    list.append(row);
  }
}

function addWords(raw: string) {
  // Comma-separated so a whole list can be pasted in one go.
  const incoming = raw
    .split(",")
    .map((w) => w.trim())
    .filter(Boolean);

  if (incoming.length === 0) return;

  const seen = new Set(settings.dictionary.map((w) => w.toLowerCase()));
  const merged = [...settings.dictionary];
  for (const word of incoming) {
    if (!seen.has(word.toLowerCase())) {
      seen.add(word.toLowerCase());
      merged.push(word);
    }
  }

  void save({ dictionary: merged }).then(renderWords);
}

function renderSnippets() {
  const list = $("#snippet-list");
  list.replaceChildren();

  if (settings.snippets.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = "No snippets yet. Add a phrase you say often and what it should become.";
    list.append(empty);
    return;
  }

  for (const snippet of settings.snippets) {
    const row = document.createElement("div");
    row.className = "entry";

    const main = document.createElement("div");
    main.className = "entry-main";

    const trigger = document.createElement("span");
    trigger.className = "entry-trigger";
    trigger.textContent = snippet.trigger;

    const arrow = document.createElement("span");
    arrow.className = "entry-arrow";
    arrow.textContent = "→";

    const value = document.createElement("span");
    value.className = "entry-value";
    value.textContent = snippet.replacement;

    main.append(trigger, arrow, value);

    const remove = document.createElement("button");
    remove.className = "entry-remove";
    remove.textContent = "×";
    remove.setAttribute("aria-label", `Remove ${snippet.trigger}`);
    remove.addEventListener("click", () => {
      void save({
        snippets: settings.snippets.filter((s) => s.trigger !== snippet.trigger),
      }).then(renderSnippets);
    });

    row.append(main, remove);
    list.append(row);
  }
}

function saveSnippet(trigger: string, replacement: string) {
  // Replace rather than duplicate when the trigger already exists.
  const rest = settings.snippets.filter((s) => s.trigger.toLowerCase() !== trigger.toLowerCase());

  void save({ snippets: [...rest, { trigger, replacement }] }).then(() => {
    renderSnippets();
    // Snippets live behind the second tab, so switching to it is the only way the user sees
    // what they just made.
    const tab = document.querySelector<HTMLInputElement>(
      'input[name="dictview"][value="snippets"]',
    );
    if (tab) {
      tab.checked = true;
      tab.dispatchEvent(new Event("change"));
    }
  });
}

/**
 * The dictionary composer: one field, two commit keys.
 *
 * Enter adds what you typed as a word the speller should know. Cmd+Enter treats the same text as
 * a trigger and reveals a second field for what it should expand to. Previously these were two
 * separate composers behind two tabs, which made adding a replacement a four-step detour.
 */
function initDictionary() {
  const term = $<HTMLInputElement>("#term-input");
  const replyRow = $("#replacement-row");
  const reply = $<HTMLInputElement>("#replacement-input");

  const commitWord = () => {
    addWords(term.value);
    term.value = "";
    closeReply();
  };

  const openReply = () => {
    const trigger = term.value.trim();
    if (!trigger) {
      flash("Type the phrase you want replaced first", "error");
      return;
    }
    replyRow.hidden = false;
    reply.placeholder = `Replace “${trigger}” with`;
    reply.focus();
  };

  function closeReply() {
    replyRow.hidden = true;
    reply.value = "";
  }

  const commitReply = () => {
    const trigger = term.value.trim();
    const replacement = reply.value.trim();
    if (!trigger || !replacement) {
      flash("A replacement needs both a phrase and what it becomes", "error");
      return;
    }
    saveSnippet(trigger, replacement);
    term.value = "";
    closeReply();
  };

  $("#term-add").addEventListener("click", commitWord);
  $("#term-replace").addEventListener("click", openReply);
  $("#replacement-save").addEventListener("click", commitReply);

  term.addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    if (e.metaKey) openReply();
    else commitWord();
  });

  reply.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      commitReply();
    } else if (e.key === "Escape") {
      // Stop here rather than letting the window-level handler close the whole window.
      e.preventDefault();
      e.stopPropagation();
      closeReply();
      term.focus();
    }
  });

  document.querySelectorAll<HTMLInputElement>('input[name="dictview"]').forEach((input) => {
    input.addEventListener("change", () => {
      $("#view-words").hidden = input.value !== "words";
      $("#view-snippets").hidden = input.value !== "snippets";
    });
  });
}

/* Sounds -------------------------------------------------------------------- */

async function initSounds() {
  const choices = await invoke<[string, string][]>("sound_choices");

  for (const id of ["#soundStart", "#soundStop"]) {
    const select = $<HTMLSelectElement>(id);
    select.replaceChildren();
    for (const [value, label] of choices) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = label;
      select.append(option);
    }
  }

  $<HTMLSelectElement>("#soundStart").addEventListener("change", (e) => {
    const name = (e.target as HTMLSelectElement).value;
    void invoke("preview_sound", { name });
    void save({ soundStart: name });
  });

  $<HTMLSelectElement>("#soundStop").addEventListener("change", (e) => {
    const name = (e.target as HTMLSelectElement).value;
    void invoke("preview_sound", { name });
    void save({ soundStop: name });
  });

  $("#previewStart").addEventListener("click", () =>
    invoke("preview_sound", { name: settings.soundStart }),
  );
  $("#previewStop").addEventListener("click", () =>
    invoke("preview_sound", { name: settings.soundStop }),
  );
}

/* History ------------------------------------------------------------------- */

interface HistoryEntry {
  at: number;
  text: string;
  duration: number;
}

let historyCache: HistoryEntry[] = [];

/** "TODAY" / "YESTERDAY" / "3 AUGUST" — matching how people actually scan a log. */
function dayLabel(at: number): string {
  const date = new Date(at * 1000);
  const today = new Date();
  const startOf = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

  const days = Math.round((startOf(today) - startOf(date)) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return date.toLocaleDateString(undefined, { day: "numeric", month: "long" });
}

function timeLabel(at: number): string {
  return new Date(at * 1000).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
}

function renderHistory() {
  const list = $("#history-list");
  const query = $<HTMLInputElement>("#history-search").value.trim().toLowerCase();

  const matches = query
    ? historyCache.filter((e) => e.text.toLowerCase().includes(query))
    : historyCache;

  list.replaceChildren();

  if (matches.length === 0) {
    const card = document.createElement("div");
    card.className = "card";
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = query
      ? `Nothing matches “${query}”.`
      : historyCache.length === 0
        ? "Nothing dictated yet."
        : "";
    card.append(empty);
    list.append(card);
    return;
  }

  let currentDay = "";
  let card: HTMLElement | null = null;

  for (const entry of matches) {
    const label = dayLabel(entry.at);
    if (label !== currentDay) {
      currentDay = label;
      const heading = document.createElement("div");
      heading.className = "day-label";
      heading.textContent = label;
      list.append(heading);

      card = document.createElement("div");
      card.className = "card";
      list.append(card);
    }

    const row = document.createElement("div");
    row.className = "transcript";

    const time = document.createElement("span");
    time.className = "transcript-time";
    time.textContent = timeLabel(entry.at);

    const text = document.createElement("div");
    text.className = "transcript-text";
    text.textContent = entry.text;

    const actions = document.createElement("div");
    actions.className = "transcript-actions";

    const insert = document.createElement("button");
    insert.className = "btn";
    insert.textContent = "Insert";
    insert.title = "Paste into the app you were last using";
    insert.addEventListener("click", async () => {
      try {
        await invoke("reinsert", { text: entry.text });
        flash("Inserted");
      } catch (e) {
        flash(String(e), "error");
      }
    });

    const copy = document.createElement("button");
    copy.className = "btn btn--quiet";
    copy.textContent = "Copy";
    copy.addEventListener("click", async () => {
      await navigator.clipboard.writeText(entry.text);
      flash("Copied");
    });

    const remove = document.createElement("button");
    remove.className = "entry-remove";
    remove.textContent = "×";
    remove.setAttribute("aria-label", "Delete this transcript");
    remove.addEventListener("click", async () => {
      await invoke("delete_history_entry", { at: entry.at });
      historyCache = historyCache.filter((e) => e.at !== entry.at);
      renderHistory();
      renderHome();
    });

    actions.append(insert, copy, remove);
    row.append(time, text, actions);
    (card ?? list).append(row);
  }
}

async function loadHistory() {
  historyCache = await invoke<HistoryEntry[]>("list_history");
  renderHistory();
  // Home is a view over the same data, so it must not be left showing a stale week.
  renderHome();
}

function initHistory() {
  $<HTMLInputElement>("#history-search").addEventListener("input", renderHistory);

  $("#history-clear").addEventListener("click", async () => {
    if (historyCache.length === 0) return;
    await invoke("clear_history");
    historyCache = [];
    renderHistory();
    renderHome();
    flash("History cleared");
  });

  bindCheckbox("#historyEnabled", (v) => void save({ historyEnabled: v }).then(renderHome));

  $<HTMLSelectElement>("#historyDays").addEventListener("change", (e) => {
    void save({ historyDays: Number((e.target as HTMLSelectElement).value) }).then(loadHistory);
  });
}

/* About --------------------------------------------------------------------- */

interface AboutInfo {
  version: string;
  model: string;
  shortcut: string;
  dataDir: string;
  logPath: string;
  modelsDir: string;
  diskMb: number;
}

async function renderAbout() {
  const info = await invoke<AboutInfo>("about");

  $("#about-version").textContent = `Version ${info.version}`;
  $("#brand-version").textContent = info.version;
  $("#about-model").textContent = info.model;
  $("#about-shortcut").textContent = pretty(info.shortcut);
  $("#about-disk").textContent =
    info.diskMb >= 1024 ? `${(info.diskMb / 1024).toFixed(1)} GB` : `${info.diskMb} MB`;
  $("#about-data-dir").textContent = info.dataDir;

  $("#open-data").addEventListener("click", () => invoke("reveal", { path: info.modelsDir }));
  $("#open-log").addEventListener("click", () => invoke("reveal", { path: info.logPath }));
}

/**
 * External links go through the shell rather than being `<a href>`s.
 *
 * A real anchor inside a Tauri webview navigates the window away from the app and there is no
 * back button to return with. Rust also checks the scheme before handing anything to `open`.
 */
function initLinks() {
  document.querySelectorAll<HTMLElement>("[data-url]").forEach((element) => {
    element.addEventListener("click", () => {
      void invoke("open_url", { url: element.dataset.url });
    });
  });

  // Built from the clock rather than hardcoded, so it does not quietly go stale.
  $("#about-copyright").textContent = `© ${new Date().getFullYear()}`;
}

/* Wiring ------------------------------------------------------------------- */

function bindCheckbox(id: string, apply: (value: boolean) => void) {
  $<HTMLInputElement>(id).addEventListener("change", (e) =>
    apply((e.target as HTMLInputElement).checked),
  );
}

function render() {
  renderShortcut();
  renderAnchors();
  applyTheme();
  syncDevices();

  $<HTMLSelectElement>("#activation").value = settings.activation;
  $<HTMLSelectElement>("#language").value = settings.language;
  $<HTMLInputElement>("#removeFillers").checked = settings.removeFillers;
  $<HTMLInputElement>("#playSounds").checked = settings.playSounds;
  $<HTMLInputElement>("#launchAtLogin").checked = settings.launchAtLogin;
  $<HTMLInputElement>("#accurate").checked = settings.accurate;
  $<HTMLInputElement>("#autocapitalize").checked = settings.autocapitalize;
  $<HTMLInputElement>("#showInDock").checked = settings.showInDock;
  $<HTMLInputElement>("#menubarClickRecords").checked = settings.menubarClickRecords;
  $<HTMLInputElement>("#historyEnabled").checked = settings.historyEnabled;
  $<HTMLSelectElement>("#historyDays").value = String(settings.historyDays);
  $<HTMLSelectElement>("#soundStart").value = settings.soundStart;
  $<HTMLSelectElement>("#soundStop").value = settings.soundStop;

  // The sound pickers are meaningless when cues are off.
  const soundsOn = settings.playSounds;
  $("#row-sound-start").style.opacity = soundsOn ? "1" : "0.45";
  $("#row-sound-stop").style.opacity = soundsOn ? "1" : "0.45";
  $<HTMLSelectElement>("#soundStart").disabled = !soundsOn;
  $<HTMLSelectElement>("#soundStop").disabled = !soundsOn;

  const theme = document.querySelector<HTMLInputElement>(
    `input[name="theme"][value="${settings.theme}"]`,
  );
  if (theme) theme.checked = true;
}

async function main() {
  settings = await invoke<Settings>("get_settings");

  // Cmd+W and Escape should close a settings window, as they do anywhere else on macOS.
  window.addEventListener("keydown", (e) => {
    if (recording) return;
    if (e.key === "Escape" || (e.metaKey && e.key === "w")) {
      e.preventDefault();
      void invoke("close_settings");
    }
  });

  // The tray's "History…" item opens this window and then asks for a section.
  void listen<string>("settings:goto", (e) => goTo(e.payload));

  initTabs();
  initSidebar();
  initLinks();
  initHome();
  initModels();
  initRecorder();
  initAnchors();
  initDictionary();
  initHistory();
  await initSounds();
  await initDevices();

  $<HTMLSelectElement>("#activation").addEventListener("change", (e) => {
    void save({ activation: (e.target as HTMLSelectElement).value as Settings["activation"] });
  });

  $<HTMLSelectElement>("#language").addEventListener("change", (e) => {
    void save({ language: (e.target as HTMLSelectElement).value });
  });

  document.querySelectorAll<HTMLInputElement>('input[name="theme"]').forEach((input) => {
    input.addEventListener("change", () => {
      if (input.checked) void save({ theme: input.value as Settings["theme"] }).then(applyTheme);
    });
  });

  bindCheckbox("#removeFillers", (v) => void save({ removeFillers: v }));
  bindCheckbox("#playSounds", (v) => void save({ playSounds: v }).then(render));
  bindCheckbox("#launchAtLogin", (v) => void save({ launchAtLogin: v }));
  bindCheckbox("#accurate", (v) => void save({ accurate: v }));
  bindCheckbox("#autocapitalize", (v) => void save({ autocapitalize: v }));
  bindCheckbox("#showInDock", (v) => void save({ showInDock: v }));
  bindCheckbox("#menubarClickRecords", (v) => void save({ menubarClickRecords: v }));

  render();
  renderWords();
  renderSnippets();
  void renderModels();
  void renderAbout();
  void loadHistory();
}

void main();
