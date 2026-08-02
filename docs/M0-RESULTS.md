# M0 results

**Machine:** Apple M1 Pro, 16GB, macOS 26.4.1
**Model:** Parakeet TDT 0.6B v3, ONNX int8 (639MB on disk — the ~640MB estimate was right)
**Method:** 5 timed iterations per clip after one untimed warm-up. One process per execution
provider so peak RSS isn't contaminated across runs. 4 intra-op threads throughout.

Clips are macOS `say` TTS, so **absolute WER here is optimistic** — it exists to catch a backend
returning garbage, not to publish an accuracy figure.

---

## Q1: which execution provider? — **CPU, decisively**

`parakeet-rs` 0.3.7 (ONNX Runtime), median decode time:

| Provider         | 2.1s      | 5.5s      | 12.1s     | 26.0s      | Peak RSS   | Model load |
| ---------------- | --------- | --------- | --------- | ---------- | ---------- | ---------- |
| **CPU**          | **123ms** | **257ms** | **516ms** | **1150ms** | **1468MB** | 1643ms     |
| CoreML (CPU+ANE) | 185ms     | 380ms     | 793ms     | 1839ms     | 6428MB     | 6977ms     |
| CoreML (CPU+GPU) | 205ms     | 414ms     | 859ms     | 2086ms     | 6491MB     | 5032ms     |
| WebGPU/Metal     | 400ms     | 758ms     | 1281ms    | 2589ms     | 1417MB     | 1594ms     |

All four produced correct transcripts (WER 0%), so nothing here is numerically broken — the
differences are pure speed and memory.

**CPU is ~21x realtime and wins on every axis.** CoreML is 1.5x slower _and_ uses 4.4x the
memory. WebGPU is 3x slower and drags in a `libwebgpu_dawn.dylib` that would have to ship in the
bundle.

This closes the original Neural Engine thesis with data. The `parakeet-rs` source explains why:
the ONNX graphs have dynamic input shapes, so CoreML claims the nodes but executes them on CPU
anyway, paying transfer overhead for nothing.

## Q2: which crate? — **`parakeet-rs`, by 2x**

Same model, same threads, same clips, CPU both:

| Crate                   | 2.1s      | 5.5s      | 12.1s     | 26.0s      | Realtime | Peak RSS   | WER (mid) |
| ----------------------- | --------- | --------- | --------- | ---------- | -------- | ---------- | --------- |
| **`parakeet-rs` 0.3.7** | **123ms** | **257ms** | **516ms** | **1150ms** | **21x**  | **1468MB** | **0%**    |
| `sherpa-rs` 0.6.8       | 221ms     | 525ms     | 1109ms    | 2389ms     | 10.5x    | 2399MB     | 6%        |

`parakeet-rs` is twice as fast, uses 900MB less, and was more accurate. sherpa dropped words on
the short clips — "Let's **shop** the parakeet engine", and "Let's _ the parakeet engine" with
"ship" missing entirely. Not a misconfiguration: `feature_dim=128` (correct for v3) was faster
than 80 but produced identical text, so the drops are real behaviour.

Worth noting `sherpa-rs` would also have given us VAD and punctuation models in one dependency.
Not worth 2x the latency — Silero VAD is available separately.

## Q2b: Whisper vs Parakeet — **Whisper Base wins, and it isn't close**

Added after the report that OpenWhispr's default (Whisper Base, 141MB) is accurate enough for
daily non-native-English dictation. Same clips, same 4 threads, whisper.cpp with Metal.

| Engine                        | 2.1s      | 5.5s      | 12.1s     | 26.0s     | **Peak RSS** | Warm load | Disk      |
| ----------------------------- | --------- | --------- | --------- | --------- | ------------ | --------- | --------- |
| **Whisper Base**              | 146ms     | **193ms** | **322ms** | **588ms** | **294MB**    | **187ms** | **141MB** |
| Parakeet (`parakeet-rs`, CPU) | **123ms** | 257ms     | 516ms     | 1150ms    | 1468MB       | 1643ms    | 639MB     |
| Whisper Small                 | 386ms     | 437ms     | 601ms     | 1116ms    | 693MB        | 574ms     | 465MB     |
| Parakeet (`sherpa-rs`, CPU)   | 221ms     | 525ms     | 1109ms    | 2389ms    | 2399MB       | 3146ms    | 643MB     |

Whisper Base beats Parakeet on **memory (5x), speed beyond 2s, download size (4.5x), and load
time (9x)**. The only thing Parakeet wins is the very shortest clip.

**Accuracy was effectively a tie.** Whisper Base's errors were entirely the proper noun
"parakeet" — "pair of heat", "Perikete". Everything else scored 0–2%, identical to Parakeet.
A domain word missed by an ASR model is exactly what §6.4's custom dictionary exists for, and
notably Whisper _Small_ got it no better ("Periket"), so paying 3x the memory doesn't fix it.

### The structural difference that matters

Whisper's realtime factor **improves** with length — 14.6x at 2.1s, 44.2x at 26s — because it
always pads to a fixed 30-second window. So a Whisper call costs **~150ms floor regardless of how
short the audio is**. Parakeet scales linearly instead.

Consequences for segmentation:

- **Whisper's tail after key release is ~150–200ms and nearly constant** up to 30s of speech.
  Even with _no_ segmentation, a 26s utterance finishes in 588ms.
- The 4s force-cut therefore stops being a latency mechanism for Whisper and becomes purely a
  _progressive-text_ mechanism — it exists so words appear as you pause, not to keep the tail small.
- With Parakeet the opposite holds: short segments are genuinely cheaper, so segmentation is
  load-bearing for latency.

### The honest caveat

These clips are macOS `say` TTS — clean, unaccented, studio-quality. That plausibly flatters
Whisper Base more than Parakeet, since Parakeet's headline advantage on the Open ASR Leaderboard
(6.32% vs 7.44% WER) is measured on real speech. **Before this is settled, record ~10 real clips
in your own voice and re-run both.** That is the one measurement that would change the
recommendation, and it takes ten minutes.

## Q3: does the Accessibility grant survive a rebuild? — **not with ad-hoc signing**

TCC keys the grant on the code signature's _designated requirement_, so this is testable without
granting anything: build twice, diff the DR.

Ad-hoc (what an unsigned build gets):

```
build 1 designated requirement:  cdhash H"9366259413e7fb36d2cdda171fbc5941d7680c91"
build 2 designated requirement:  cdhash H"2ecf02b17a8a152b9e314a8c83decca60dd9b33b"
```

The DR _is_ the binary hash. Every build is a different app as far as TCC is concerned, so
**every auto-update silently revokes Accessibility** and the user must re-grant by hand. That is
the worst-case outcome anticipated for signing, and it is confirmed.

### Self-signed certificate — **STABLE. This is the fix.**

Two builds with different binaries produced an identical designated requirement:

```
build 1 cdhash:  08fb2af3e384561c1e5d69fe91ee77dea89ce4d5
build 2 cdhash:  c830d01a3b09bfe833e1c5a275babb371d88810f

both DRs:  identifier "com.byrhn.whisperlite.tccprobe"
           and certificate leaf = H"48ec0eed29682ba39795b766690f45435c7c6e8c"
```

The DR pins to **bundle identifier + certificate**, not the binary hash. Accessibility survives
rebuilds and therefore survives auto-updates. **A free Keychain Access certificate is sufficient;
the $99/yr Developer ID is not required for this.**

Two operational consequences:

- **The certificate becomes critical infrastructure.** Lose it and the DR changes, which forces
  every existing user to re-grant Accessibility. Back it up as a `.p12` outside the machine.
- **CI must sign with the same cert.** Export the `.p12`, store it as a GitHub secret, import it
  into the keychain during the release workflow. Without this, CI builds get ad-hoc signatures
  and the whole benefit is lost. This is a change to jotter's `release.yml`, which signs nothing.

Note `security find-identity -v -p codesigning` reports "0 valid identities" for a self-signed
root — it isn't trusted for the codesigning _policy_ — but `codesign --sign` uses it happily and
that is all TCC cares about. Don't let that message mislead you into thinking it failed.

---

## What this changes

**1. The 6s force-cut is too long.** Decode scales linearly at ~43ms per second of audio plus
~33ms fixed:

| Force-cut | Decode    | Total perceived (incl. capture, post-processing, injection) |
| --------- | --------- | ----------------------------------------------------------- |
| 6s        | 291ms     | 361–441ms — fails on the clipboard-fallback path            |
| **4s**    | **205ms** | **275–355ms — fits with margin**                            |
| 3s        | 162ms     | 232–312ms                                                   |

**Cut at 4s, not 6s.** The cost is more frequent segment boundaries, which M2 needs to measure
for WER impact.

**2. Whisper Base should be the default model, not Parakeet.** This reverses the original engine
choice. At **294MB resident and a ~190ms tail**, it dissolves the memory problem that Parakeet's
1.47GB created — a background utility can justify 294MB; it cannot really justify 9% of a 16GB
machine. Parakeet stays in the picker as the "higher accuracy" option, which is where the §6.4
provider tabs already put it.

Whisper Base also happens to fit the app better in two ways nobody planned:

- **99 languages vs Parakeet's 25** — better for a non-native speaker who may dictate in more
  than one language.
- **187ms warm load**, so the unload-when-idle strategy becomes cheap. Parakeet's 1.6s load made
  that trade painful; at 187ms it is nearly free.

Caveat carried forward: the accuracy comparison rests on TTS clips. Re-run on real voice before
locking it in.

_(One-time cost: Whisper's **first** load took 11.8s while Metal compiled shaders. Cached
afterwards at 187ms. Onboarding should absorb that behind the model-download step so nobody's
first dictation waits on it.)_

**3. Latency thesis holds.** 21x realtime at 0% WER on CPU, with a 4s cut, lands the tail at
~205ms. The 400ms p50 target in §11 is achievable.

**4. Model load is 1.6s.** Pay it at app launch, not on first hotkey.

---

## Reproducing

```
cd m0-bench   && cargo run --release -- fetch && cargo run --release -- all
cd m0-sherpa  && cargo run --release -- cpu 5 128
cd m0-tcc     && ./tcc-test.sh compare adhoc
```

Both bench crates need `.cargo/config.toml` adding `@executable_path` to the rpath — neither
`ort`'s Dawn dylib nor sherpa-onnx's bundled ONNX Runtime emit one, so the binaries won't launch
without it. That's a packaging note for the real app too.
