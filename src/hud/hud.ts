import { listen } from "@tauri-apps/api/event";

type State = "idle" | "listening" | "working" | "error";

interface StatePayload {
  state: State;
  label?: string;
}

interface TickPayload {
  level: number;
}

/** Sets the width of the whole pill: bars and gaps are fixed, so this is the only lever. */
const BARS = 16;

const pill = document.getElementById("pill")!;
const label = document.getElementById("label")!;
const wave = document.getElementById("wave")!;

for (let i = 0; i < BARS; i++) wave.appendChild(document.createElement("i"));
const bars = Array.from(wave.querySelectorAll("i"));

/**
 * Levels scroll right-to-left so the newest sample is always at the leading edge. That reads as
 * "this is live" far better than bars jittering in place.
 */
const levels: number[] = new Array(BARS).fill(0);

/** Must match the `.wave i` height in hud.css — every scale below is derived from it. */
const BAR_H = 20;
/** At rest the bars settle into a row of dots rather than collapsing to nothing. */
const MIN_SCALE = 3 / BAR_H;

/**
 * How much each bar is dimmed by its age, oldest first.
 *
 * Opacity only. An earlier version also scaled the bars down with age, which turned out to be the
 * whole reason the waveform never looked like one: the level arriving from Rust is heavily
 * smoothed, so over a short window it is close to constant, and a constant level times a rising
 * taper is a smooth ramp. Height now shows the signal and nothing else.
 *
 * Index-dependent only, so it is set once here and the per-frame paint touches just `transform`.
 */
bars.forEach((bar, i) => {
  bar.style.setProperty("--fade", (0.38 + 0.62 * (i / (BARS - 1))).toFixed(3));
});

/**
 * Frames of input each bar represents.
 *
 * The meter has a fast attack and a slow release, so one sample per frame gave the sixteen bars a
 * ~270ms window — too short for a smoothed signal to vary across. Holding the peak over three
 * frames widens it to ~800ms, which is long enough for individual syllables to show up.
 */
const FRAMES_PER_BAR = 3;

/**
 * Repaint on the display's own cadence rather than on IPC arrival.
 *
 * Events land at ~60/s but not evenly spaced, and painting directly from them makes the jitter
 * visible. Buffering the newest level and flushing in rAF decouples the two, so the bars move at
 * a steady frame rate regardless of when messages arrive.
 */
let pending: number | null = null;
let framesHeld = 0;
let frame = 0;

function paint() {
  frame = 0;

  // Commit a bar only every FRAMES_PER_BAR frames, carrying the loudest sample seen in between.
  // Peak rather than last, so a short transient between commits is never dropped.
  if (pending !== null && ++framesHeld >= FRAMES_PER_BAR) {
    levels.shift();
    levels.push(pending);
    pending = null;
    framesHeld = 0;
  }

  for (let i = 0; i < BARS; i++) {
    bars[i].style.transform = `scaleY(${Math.max(MIN_SCALE, levels[i]).toFixed(3)})`;
  }
}

function pushLevel(level: number) {
  pending = pending === null ? level : Math.max(pending, level);
  if (!frame) frame = requestAnimationFrame(paint);
}

function resetWave() {
  levels.fill(0);
  pending = null;
  framesHeld = 0;
  for (const bar of bars) bar.style.transform = `scaleY(${MIN_SCALE})`;
}

function setState({ state, label: text }: StatePayload) {
  pill.dataset.state = state;
  label.textContent = text ?? "";

  if (state !== "listening") resetWave();
}

void listen<StatePayload>("hud:state", (e) => setState(e.payload));

void listen<TickPayload>("hud:tick", (e) => pushLevel(e.payload.level));

resetWave();
