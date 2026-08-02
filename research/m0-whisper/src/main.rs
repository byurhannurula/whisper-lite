//! M0 addendum: Whisper via whisper.cpp, for comparison against Parakeet.
//!
//! Motivated by a real data point rather than a hunch — OpenWhispr recommends Whisper Base
//! (141MB) by default and it is reportedly accurate enough for daily non-native-English
//! dictation. Parakeet's problem in M0 was not speed but **1.47GB resident**, so a model an
//! order of magnitude smaller is worth measuring before picking a default.
//!
//! Separate crate again: whisper.cpp links its own Metal/GGML runtime.
//! Reads the same clips as m0-bench so every number is directly comparable.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const AUDIO_DIR: &str = "../m0-bench/audio";

const CLIPS: &[(&str, &str)] = &[
    ("short", "let's ship the parakeet engine first"),
    (
        "mid",
        "let's ship the parakeet engine first and benchmark it properly on the laptop \
         before we commit to anything",
    ),
    (
        "long",
        "the whole point of this spike is to find out whether the decode step is fast \
         enough to hide behind a natural pause, because if it is not then the entire \
         latency argument falls apart and we should go back to whisper instead",
    ),
    (
        "xlong",
        "the whole point of this spike is to find out whether the decode step is fast \
         enough to hide behind a natural pause, because if it is not then the entire \
         latency argument falls apart and we should go back to whisper instead. \
         a thirty second clip is deliberately longer than the six second force cut, \
         so it tells us what would happen if the voice activity detector never found a \
         gap to split on, which is the failure mode the force cut exists to prevent \
         in the first place",
    ),
];

fn main() -> Result<()> {
    let model_name = std::env::args().nth(1).unwrap_or_else(|| "base".into());
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let model_path = PathBuf::from(format!("models/ggml-{model_name}.bin"));
    if !model_path.exists() {
        bail!("missing {}", model_path.display());
    }
    let model_mb = std::fs::metadata(&model_path)?.len() as f64 / (1024.0 * 1024.0);

    println!("== whisper.cpp / ggml-{model_name} ({model_mb:.0}MB on disk), Metal ==\n");

    let load_start = Instant::now();
    let ctx = WhisperContext::new_with_params(
        model_path.to_str().unwrap(),
        WhisperContextParameters::default(),
    )
    .context("loading whisper model")?;
    let mut state = ctx.create_state().context("creating state")?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

    println!("model load: {load_ms:.0}ms\n");
    println!(
        "  {:<7} {:>7} {:>10} {:>9} {:>7}",
        "clip", "audio", "median", "realtime", "WER"
    );
    println!("  {}", "-".repeat(46));

    for (name, reference) in CLIPS {
        let path = Path::new(AUDIO_DIR).join(format!("{name}.wav"));
        let (samples, duration) = load_wav(&path)?;

        let mut run = |state: &mut whisper_rs::WhisperState| -> Result<String> {
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            // Match how a dictation app would actually call this: English, no timestamps
            // printed, 4 threads to mirror the Parakeet run, and all the console chatter off.
            params.set_language(Some("en"));
            params.set_n_threads(4);
            params.set_translate(false);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);

            state.full(params, &samples).context("whisper full()")?;

            let n = state.full_n_segments();
            let mut text = String::new();
            for i in 0..n {
                if let Some(seg) = state.get_segment(i) {
                    text.push_str(&seg.to_str_lossy().unwrap_or_default());
                }
            }
            Ok(text)
        };

        // Untimed warm-up, same protocol as the other two crates.
        let _ = run(&mut state)?;

        let mut times = Vec::with_capacity(iters);
        let mut text = String::new();
        for _ in 0..iters {
            let start = Instant::now();
            text = run(&mut state)?;
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }

        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = times[times.len() / 2];

        println!(
            "  {:<7} {:>6.1}s {:>9.0}ms {:>8.1}x {:>6.0}%",
            name,
            duration,
            median,
            duration as f64 / (median / 1000.0),
            wer(reference, &text) * 100.0
        );
        println!("          \u{201c}{}\u{201d}", text.trim());
    }

    println!("\npeak RSS: {:.0}MB", peak_rss_mb());
    Ok(())
}

fn load_wav(path: &Path) -> Result<(Vec<f32>, f32)> {
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
    Ok((samples, duration))
}

fn peak_rss_mb() -> f64 {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return 0.0;
        }
        usage.ru_maxrss as f64 / (1024.0 * 1024.0)
    }
}

fn wer(reference: &str, hypothesis: &str) -> f32 {
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
