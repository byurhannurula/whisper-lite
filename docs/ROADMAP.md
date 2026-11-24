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
- Unloads the model after 10 minutes idle, and reloads it while you speak
- Model downloads resume after a failure and are checked against a pinned SHA-256
- Starts with no model downloaded, warms one in the background, and sends you to Models when
  there is nothing to dictate with

## Next

1. **Onboarding** — permissions, then pick and download a model, then set a shortcut. The plumbing
   is in place now that startup no longer needs a model.
2. **Accessibility-API insertion** with a paste fallback, so dictating stops touching the
   clipboard.
3. **Esc to cancel** mid-dictation.
4. **Live text in the indicator** as you pause, instead of only at the end.

## Known issues

None open.

## Later

- Windows, then Linux — text injection is the platform-specific part
- Auto-update, and a signed release
- A second engine (Parakeet), which needs an engine abstraction first
- Per-app profiles: different model, language or prompt depending on where you are typing
- A command hook, so behaviour can be extended without changing the app

## Not planned

Meeting mode, speaker identification, cloud accounts, sync, telemetry.
