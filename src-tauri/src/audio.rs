//! Microphone capture.
//!
//! Two decisions are load-bearing here:
//!
//! 1. **The stream is built at launch but only started on hotkey-down.** Building a cpal stream
//!    is the slow part (device open, format negotiation); starting it is fast. This keeps the
//!    macOS orange mic indicator off while idle, which every shipping app in this category does,
//!    while still avoiding the 100-300ms first-word clip that a cold open would cause.
//!
//! 2. **Everything is resampled to 16kHz mono**, whatever the device gives us. Whisper requires
//!    it, and it also normalises away the Bluetooth/AirPods case where the device switches rate
//!    mid-session.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub const TARGET_RATE: u32 = 16_000;

/// Peak amplitude below which a recording is treated as containing no speech at all.
///
/// Room tone from a working microphone sits well above this; a muted or broken input sits at or
/// very near zero. The gap is wide, so this cleanly separates "you did not speak" from "the
/// microphone is not working".
pub const SILENCE_PEAK: f32 = 0.008;

/// Largest absolute sample, for deciding whether anything was captured.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Hard cap on a single dictation. Whisper handles 26s in ~590ms so this is not a latency
/// guard — it is a runaway guard, in case the hotkey release event is ever missed.
const MAX_SECONDS: usize = 120;

pub struct Recorder {
    stream: Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    /// Most recent input level, 0.0-1.0, as f32 bits. Written by the realtime audio callback and
    /// read by the HUD, so it must be lock-free — blocking that thread causes audible glitches.
    level: Arc<AtomicU32>,
    source_rate: u32,
    source_channels: u16,
}

// SAFETY: cpal's `Stream` is `!Send`/`!Sync` because it owns a `Box<dyn FnMut()>` callback. On
// CoreAudio that callback is only ever invoked by the audio unit's own realtime thread, never by
// us, and `play`/`pause` map to `AudioOutputUnitStart`/`Stop`, which are documented as
// thread-safe. The buffer the callback writes to is behind its own `Mutex`.
//
// The remaining hazard is dropping the stream from a different thread than created it, so the
// `Recorder` is owned by Tauri's managed state for the whole process lifetime and is never moved
// or dropped early. Callers additionally serialise `start`/`stop` behind a `Mutex<Recorder>`.
unsafe impl Send for Recorder {}
unsafe impl Sync for Recorder {}

/// Every input device macOS currently offers, for the picker.
///
/// Names only. cpal has no stable device identifier, and the name is what the user recognises.
pub fn input_devices() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

impl Recorder {
    /// Opens `wanted` by name, or the system default when it is empty or no longer present,
    /// and builds (but does not start) the capture stream.
    ///
    /// Falling back rather than failing is deliberate: a saved device disappears every time a
    /// headset is unplugged, and refusing to record until the user revisits settings would be a
    /// far worse failure than quietly using the built-in microphone.
    pub fn open(wanted: &str) -> Result<Self> {
        let host = cpal::default_host();

        let chosen = if wanted.is_empty() {
            None
        } else {
            host.input_devices().ok().and_then(|mut devices| {
                devices.find(|d| d.name().map(|n| n == wanted).unwrap_or(false))
            })
        };

        if !wanted.is_empty() && chosen.is_none() {
            eprintln!("[audio] input '{wanted}' is not available — using the system default");
        }

        let device: Device = chosen
            .or_else(|| host.default_input_device())
            .ok_or_else(|| anyhow!("no input device available"))?;

        let supported = device
            .default_input_config()
            .context("querying default input config")?;

        let source_rate = supported.sample_rate().0;
        let source_channels = supported.channels();
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let level: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
        let max_samples = MAX_SECONDS * source_rate as usize * source_channels as usize;

        let err_fn = |e| eprintln!("[audio] stream error: {e}");

        let sink = buffer.clone();
        let meter = level.clone();
        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    measure(&meter, data);
                    append(&sink, data, max_samples)
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let f: Vec<f32> = data.iter().map(|s| *s as f32 / 32768.0).collect();
                    measure(&meter, &f);
                    append(&sink, &f, max_samples)
                },
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let f: Vec<f32> = data
                        .iter()
                        .map(|s| (*s as f32 - 32768.0) / 32768.0)
                        .collect();
                    measure(&meter, &f);
                    append(&sink, &f, max_samples)
                },
                err_fn,
                None,
            ),
            other => return Err(anyhow!("unsupported sample format: {other:?}")),
        }
        .context("building input stream")?;

        println!(
            "[audio] device ready: {} — {}Hz, {} channel(s), {:?}",
            device.name().unwrap_or_else(|_| "unknown".into()),
            source_rate,
            source_channels,
            sample_format
        );

        Ok(Self {
            stream,
            buffer,
            level,
            source_rate,
            source_channels,
        })
    }

    /// Shared handle to the live input level, 0.0-1.0.
    ///
    /// Handed out so the HUD ticker can read the meter without locking the recorder. Going
    /// through the mutex meant a failed `try_lock` silently reported silence, which showed up
    /// as dropped frames in the waveform.
    pub fn level_handle(&self) -> Arc<AtomicU32> {
        self.level.clone()
    }

    /// Begins capture. This is the point the macOS mic indicator lights up.
    pub fn start(&self) -> Result<()> {
        self.buffer.lock().unwrap().clear();
        self.level.store(0, Ordering::Relaxed);
        self.stream.play().context("starting capture")?;
        Ok(())
    }

    /// Stops capture and returns the utterance as 16kHz mono.
    pub fn stop(&self) -> Result<Vec<f32>> {
        self.stream.pause().context("stopping capture")?;
        let raw = std::mem::take(&mut *self.buffer.lock().unwrap());
        Ok(to_mono_16k(&raw, self.source_rate, self.source_channels))
    }
}

/// RMS of the block, scaled so ordinary speech fills most of the meter.
///
/// Runs on the realtime audio thread, so it is allocation-free and lock-free.
fn measure(meter: &Arc<AtomicU32>, data: &[f32]) {
    if data.is_empty() {
        return;
    }
    let sum: f32 = data.iter().map(|s| s * s).sum();
    let rms = (sum / data.len() as f32).sqrt();

    // Speech RMS sits around 0.02-0.2, so a linear meter would barely move. sqrt expands the
    // quiet end, and the gain puts normal talking near full scale.
    let target = (rms * 6.0).sqrt().clamp(0.0, 1.0);

    // Asymmetric smoothing: jump to a rising level almost immediately so the meter feels
    // instant, but fall off gently so it does not flicker between syllables. A symmetric filter
    // reads as lag; this reads as responsive.
    let previous = f32::from_bits(meter.load(Ordering::Relaxed));
    let smoothed = if target > previous {
        previous + (target - previous) * 0.6
    } else {
        previous + (target - previous) * 0.18
    };

    meter.store(smoothed.to_bits(), Ordering::Relaxed);
}

fn append(sink: &Arc<Mutex<Vec<f32>>>, data: &[f32], max_samples: usize) {
    if let Ok(mut buf) = sink.lock() {
        if buf.len() < max_samples {
            buf.extend_from_slice(data);
        }
    }
}

/// Downmix to mono, then resample to 16kHz by linear interpolation.
///
/// Linear interpolation is not the highest-quality resampler available, but speech at 16kHz has
/// plenty of headroom below Nyquist and Whisper's own front-end is tolerant. Worth revisiting
/// only if accuracy on real recordings turns out to be worse than M0's numbers suggest.
fn to_mono_16k(input: &[f32], rate: u32, channels: u16) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }

    let ch = channels.max(1) as usize;
    let mono: Vec<f32> = if ch == 1 {
        input.to_vec()
    } else {
        input
            .chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };

    if rate == TARGET_RATE {
        return mono;
    }

    let ratio = rate as f64 / TARGET_RATE as f64;
    let out_len = (mono.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = mono[idx];
        let b = *mono.get(idx + 1).unwrap_or(&a);
        out.push(a + (b - a) * frac);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_stereo_to_mono() {
        // Two frames of [left, right]; each should average to its midpoint.
        let stereo = vec![0.0, 1.0, 0.5, 0.5];
        let out = to_mono_16k(&stereo, TARGET_RATE, 2);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn passes_through_at_target_rate() {
        let mono = vec![0.1, 0.2, 0.3];
        assert_eq!(to_mono_16k(&mono, TARGET_RATE, 1), mono);
    }

    #[test]
    fn resamples_48k_to_16k_by_a_third() {
        let input: Vec<f32> = (0..300).map(|i| i as f32).collect();
        let out = to_mono_16k(&input, 48_000, 1);
        assert_eq!(out.len(), 100);
        // First sample is untouched; the second lands three input samples along.
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 3.0).abs() < 1e-4);
    }

    #[test]
    fn handles_empty_input() {
        assert!(to_mono_16k(&[], 48_000, 2).is_empty());
    }
}
