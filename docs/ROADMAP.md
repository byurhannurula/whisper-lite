# Roadmap

Where Whisper Lite is, and what is next. macOS only for now.

## Working

- Hotkey → record → transcribe → insert, driven from the menu bar
- Hold-to-talk, tap-to-toggle, or both; any shortcut including bare `🌐 Fn` and `⇪ Caps Lock`
- Floating indicator with a live waveform, nine positions or off
- 14 Whisper models, downloaded on demand and hot-swappable
- Dictionary and spoken replacements
- History, searchable, text-only, with retention limits
- Autocapitalise, filler-word removal, sound cues, microphone picker
- Usage stats — words, speaking pace, time saved

## Next

1. **Lazy model loading.** The app currently fails to launch when no model is downloaded, which
   blocks first-run onboarding and makes fresh installs unusable. Everything below marked ↳
   depends on it.
2. **Onboarding** ↳ — permissions, then pick and download a model, then set a shortcut.
3. **Unload the model when idle** ↳ — reclaims ~300MB between dictations; reloading is ~190ms.
4. **Accessibility-API insertion** with a paste fallback, so dictating stops touching the
   clipboard.
5. **Esc to cancel** mid-dictation.
6. **Live text in the indicator** as you pause, instead of only at the end.

## Later

- Windows, then Linux — text injection is the platform-specific part
- Auto-update, and a signed release
- A second engine (Parakeet), which needs an engine abstraction first
- Per-app profiles: different model, language or prompt depending on where you are typing
- A command hook, so behaviour can be extended without changing the app

## Not planned

Meeting mode, speaker identification, cloud accounts, sync, telemetry.
