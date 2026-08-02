//! Test clips.
//!
//! Generated with macOS `say` + `afconvert` so the corpus is reproducible on any Mac with no
//! downloads. Synthetic TTS is fine for the thing M0 actually measures — decode time per second
//! of audio — but it is *not* a fair accuracy benchmark: TTS is cleaner than a real microphone,
//! so absolute WER here will be optimistic. It is still useful *relatively*: an execution
//! provider that returns garbage (the crate warns WebGPU might) will show up immediately.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Clip {
    pub name: &'static str,
    pub text: &'static str,
}

/// Lengths chosen around the planned segmentation model: `mid` is the 6s force-cut, which is
/// the worst-case tail the user actually waits on, so it is the number that decides the product.
pub const CLIPS: &[Clip] = &[
    Clip {
        name: "short",
        text: "let's ship the parakeet engine first",
    },
    Clip {
        name: "mid",
        text: "let's ship the parakeet engine first and benchmark it properly on the laptop \
               before we commit to anything",
    },
    Clip {
        name: "long",
        text: "the whole point of this spike is to find out whether the decode step is fast \
               enough to hide behind a natural pause, because if it is not then the entire \
               latency argument falls apart and we should go back to whisper instead",
    },
    Clip {
        name: "xlong",
        text: "the whole point of this spike is to find out whether the decode step is fast \
               enough to hide behind a natural pause, because if it is not then the entire \
               latency argument falls apart and we should go back to whisper instead. \
               a thirty second clip is deliberately longer than the six second force cut, \
               so it tells us what would happen if the voice activity detector never found a \
               gap to split on, which is the failure mode the force cut exists to prevent \
               in the first place",
    },
];

pub fn audio_dir() -> PathBuf {
    PathBuf::from("audio")
}

pub fn clip_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.wav"))
}

pub fn ensure(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;

    for clip in CLIPS {
        let wav = clip_path(dir, clip.name);
        if wav.exists() {
            let dur = duration_secs(&wav)?;
            println!("  ✓ {}.wav ({dur:.1}s)", clip.name);
            continue;
        }

        let aiff = dir.join(format!("{}.aiff", clip.name));

        // Rate 175 wpm is close to natural dictation pace; the default 200 sounds rushed and
        // compresses the clip lengths we are trying to hit.
        let say = Command::new("say")
            .args(["-r", "175", "-o"])
            .arg(&aiff)
            .arg(clip.text)
            .status()
            .context("running `say` (macOS only)")?;
        if !say.success() {
            bail!("`say` failed for clip {}", clip.name);
        }

        // 16kHz mono signed 16-bit LE — what every ASR model here expects.
        let conv = Command::new("afconvert")
            .arg(&aiff)
            .arg(&wav)
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .status()
            .context("running `afconvert`")?;
        if !conv.success() {
            bail!("`afconvert` failed for clip {}", clip.name);
        }

        std::fs::remove_file(&aiff).ok();
        let dur = duration_secs(&wav)?;
        println!("  ✓ {}.wav ({dur:.1}s, generated)", clip.name);
    }

    Ok(())
}

pub struct Loaded {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration: f32,
}

pub fn load(path: &Path) -> Result<Loaded> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|s| s as f32 / 32768.0))
            .collect::<Result<Vec<_>, _>>()?,
    };

    let duration = samples.len() as f32 / (spec.sample_rate as f32 * spec.channels as f32);

    Ok(Loaded {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        duration,
    })
}

pub fn duration_secs(path: &Path) -> Result<f32> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let frames = reader.len() as f32 / spec.channels as f32;
    Ok(frames / spec.sample_rate as f32)
}

/// Word error rate against the reference, after light normalisation.
///
/// Standard Levenshtein over word tokens. Used here only to catch an execution provider that
/// silently produces wrong output — not to publish an accuracy figure.
pub fn wer(reference: &str, hypothesis: &str) -> f32 {
    let norm = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '\'' {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };

    let r = norm(reference);
    let h = norm(hypothesis);

    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }

    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut curr = vec![0usize; h.len() + 1];

    for i in 1..=r.len() {
        curr[0] = i;
        for j in 1..=h.len() {
            let cost = if r[i - 1] == h[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[h.len()] as f32 / r.len() as f32
}
