//! M0, second half: the same measurement through `sherpa-rs` instead of `parakeet-rs`.
//!
//! Separate crate on purpose. `sherpa-rs` links sherpa-onnx, which bundles its own ONNX Runtime;
//! `m0-bench` links `ort`. Putting both in one binary invites duplicate-symbol problems that
//! would waste more time than the extra crate costs.
//!
//! Reads the same WAV clips as m0-bench so the two sets of numbers are directly comparable.

use anyhow::{bail, Context, Result};
use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};
use std::path::{Path, PathBuf};
use std::time::Instant;

const MODEL_DIR: &str = "models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
/// m0-bench generates these; run `m0-bench audio` first.
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
    let provider = std::env::args().nth(1).unwrap_or_else(|| "cpu".into());
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    // Parakeet TDT v3 uses 128 mel bins, not the 80 typical of older NeMo transducers. Getting
    // this wrong feeds the encoder malformed features, which shows up as dropped words rather
    // than an outright error — so it is worth being able to A/B it.
    let feature_dim: i32 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    let debug = std::env::var("SHERPA_DEBUG").is_ok();

    let model = PathBuf::from(MODEL_DIR);
    if !model.is_dir() {
        bail!("model dir missing: {}", model.display());
    }

    let p = |f: &str| model.join(f).to_string_lossy().to_string();

    println!(
        "== sherpa-rs / Parakeet TDT v3 int8, provider={provider}, feature_dim={feature_dim} ==\n"
    );

    let load_start = Instant::now();
    let mut rec = TransducerRecognizer::new(TransducerConfig {
        encoder: p("encoder.int8.onnx"),
        decoder: p("decoder.int8.onnx"),
        joiner: p("joiner.int8.onnx"),
        tokens: p("tokens.txt"),
        // Matches m0-bench's intra_threads=4 so the comparison is like-for-like.
        num_threads: 4,
        sample_rate: 16000,
        feature_dim,
        decoding_method: "greedy_search".into(),
        model_type: "nemo_transducer".into(),
        provider: Some(provider.clone()),
        debug,
        ..Default::default()
    })
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("creating recognizer")?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

    println!("model load: {load_ms:.0}ms\n");
    println!(
        "  {:<7} {:>7} {:>10} {:>9} {:>7}",
        "clip", "audio", "median", "realtime", "WER"
    );
    println!("  {}", "-".repeat(46));

    for (name, reference) in CLIPS {
        let path = Path::new(AUDIO_DIR).join(format!("{name}.wav"));
        let (samples, sample_rate, duration) = load_wav(&path)?;

        // One untimed warm-up, same as m0-bench.
        let _ = rec.transcribe(sample_rate, &samples);

        let mut times = Vec::with_capacity(iters);
        let mut text = String::new();
        for _ in 0..iters {
            let start = Instant::now();
            text = rec.transcribe(sample_rate, &samples);
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

fn load_wav(path: &Path) -> Result<(Vec<f32>, u32, f32)> {
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
    Ok((samples, spec.sample_rate, duration))
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
