<div align="center">

<img src="docs/images/icon.png" width="128" height="128" alt="">

# Whisper Lite

**Hold a key, talk, let go — your words are in the box.**

Local dictation for macOS. Nothing leaves your Mac.

</div>

---

Whisper Lite lives in the menu bar. Hold your shortcut, speak, release, and the text lands in
whatever you were typing into — an editor, a chat box, a terminal, a browser field. There is no
account, no cloud, and no telemetry. Speech recognition runs on-device via
[whisper.cpp](https://github.com/ggerganov/whisper.cpp) with Metal.

## Install

**Homebrew** (recommended):

```sh
brew install --cask byurhannurula/tap/whisper-lite
```

**Or** download the `.dmg` from [Releases](https://github.com/byurhannurula/whisper-lite/releases)
and drag the app to `/Applications`.

> [!IMPORTANT]
> Builds are **not code-signed or notarized** — there is no paid Apple Developer account behind
> this. macOS will refuse the first launch. Either open **System Settings → Privacy & Security**
> and click **Open Anyway**, or run:
>
> ```sh
> xattr -dr com.apple.quarantine /Applications/Whisper\ Lite.app
> ```

On first run macOS asks for two permissions:

| Permission        | Why                                                                   |
| ----------------- | --------------------------------------------------------------------- |
| **Microphone**    | To hear you. The recording is discarded as soon as it is transcribed. |
| **Accessibility** | To type the result into the app you were using.                       |

Then open **Models** and download one — no model ships with the app.

## Models

Every model runs locally and is downloaded on demand. Start with **Base**; move up if accuracy
matters more than the wait.

|                      | Size   | Speed    | Accuracy  |
| -------------------- | ------ | -------- | --------- |
| Tiny                 | 74 MB  | Fastest  | Low       |
| **Base** _(default)_ | 141 MB | Fast     | Fair      |
| Small                | 465 MB | Fast     | Good      |
| Medium               | 1.5 GB | Slow     | Very good |
| Large v3 Turbo       | 1.5 GB | Moderate | Best      |
| Large v3             | 3.0 GB | Slowest  | Best      |

Plus **English-only** builds, which beat their multilingual counterpart at the same size, and
**compressed** builds — `Large v3 Turbo (compressed)` is 547 MB against 1.5 GB for very little
accuracy lost, and is the best all-rounder in the list.

## What it does

- **Nine HUD positions** — a floating pill with a live waveform, or off entirely
- **Hold-to-talk, tap-to-toggle, or both**, with any shortcut including bare `🌐 Fn` / `⇪ Caps Lock`
- **Dictionary** — names and acronyms fed to the decoder so it spells them right
- **Replacements** — say a phrase, get the expansion
- **History** — searchable, text-only, with retention limits
- **Autocapitalise** and filler-word removal
- **Usage stats** — words, speaking pace, and time saved against typing

## Privacy

Audio never leaves the machine and is never written to disk. Transcripts are stored locally only
if you leave History on, and can be cleared or disabled at any time. There is no telemetry, no
crash reporting, and no network traffic except model downloads from Hugging Face.

## Build from source

Requires Rust, Node 22+ and [pnpm](https://pnpm.io).

```sh
git clone https://github.com/byurhannurula/whisper-lite
cd whisper-lite
pnpm install
pnpm tauri dev       # run it
pnpm tauri build     # produce a .dmg
pnpm ship            # build, ad-hoc sign, install to /Applications
```

`pnpm ship` signs with a stable ad-hoc identity, which is what makes macOS remember the
microphone and Accessibility grants between builds.

```sh
pnpm build           # typecheck + bundle
pnpm test            # Rust suite
```

## Releasing

```sh
pnpm release patch --dry-run    # preview
pnpm release patch              # bump, gate, tag, push
```

The script bumps the version in `package.json`, `tauri.conf.json` and `Cargo.toml` together,
runs every gate CI runs, then tags and pushes. The tag triggers a build of the universal DMG
and a **draft** release; publishing that draft is manual, and is also what updates the
Homebrew cask.

## Layout

```
src/               HUD + settings (TypeScript, no framework)
src-tauri/src/     audio, engine, text, inject, hotkey, hud, history, models
scripts/           install.sh (local install), release.mjs (cut a release)
docs/              ROADMAP.md, M0-RESULTS.md — the engine benchmarks
research/          the benchmark harnesses behind those numbers
```

## Status

Daily-driven on macOS. Windows and Linux are not built yet — text injection is the
platform-specific part, and only the macOS path exists.

See the [roadmap](docs/ROADMAP.md) for what works and what is next.
[M0-RESULTS](docs/M0-RESULTS.md) has the measurements that chose Whisper over Parakeet.

## Licence

[AGPL-3.0-or-later](LICENSE) · © 2026 Byurhan Nurula
