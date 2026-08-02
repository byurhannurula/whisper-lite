//! The measurement itself.
//!
//! One process per execution provider. That is deliberate: peak RSS is a high-water mark for the
//! whole process, so running every EP in one process would report the first EP's peak for all of
//! them. `main.rs` re-spawns this binary once per EP and aggregates the JSON.

use parakeet_rs::{
    CoreMLComputeUnits, ExecutionConfig, ExecutionProvider, ParakeetTDT, TimestampMode, Transcriber,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ep {
    /// Plain CPU. The crate's default and, per its own source comments, likely the fastest here.
    Cpu,
    /// CoreML restricted to CPU+GPU (the crate's default compute units).
    Coreml,
    /// CoreML allowed to use the Neural Engine. The original plan assumed this would win.
    CoremlAne,
    /// WebGPU / Metal. The crate warns this is experimental and may be numerically wrong,
    /// which is why every run is checked against a reference transcript.
    Webgpu,
}

impl Ep {
    pub fn label(&self) -> &'static str {
        match self {
            Ep::Cpu => "CPU",
            Ep::Coreml => "CoreML (CPU+GPU)",
            Ep::CoremlAne => "CoreML (CPU+ANE)",
            Ep::Webgpu => "WebGPU/Metal",
        }
    }

    pub fn all() -> &'static [Ep] {
        &[Ep::Cpu, Ep::Coreml, Ep::CoremlAne, Ep::Webgpu]
    }

    fn config(&self, cache_dir: &Path) -> ExecutionConfig {
        // 4 intra-op threads: the M1 Pro has 8 performance cores, but a dictation app must stay
        // responsive and off the efficiency cores while decoding. Saturating every core would
        // flatter the benchmark and misrepresent real use.
        let base = ExecutionConfig::new()
            .with_intra_threads(4)
            .with_inter_threads(1);

        match self {
            Ep::Cpu => base.with_execution_provider(ExecutionProvider::Cpu),
            Ep::Coreml => base
                .with_execution_provider(ExecutionProvider::CoreML)
                .with_coreml_compute_units(CoreMLComputeUnits::CpuAndGpu)
                .with_coreml_cache_dir(cache_dir),
            Ep::CoremlAne => base
                .with_execution_provider(ExecutionProvider::CoreML)
                .with_coreml_compute_units(CoreMLComputeUnits::CpuAndNeuralEngine)
                .with_coreml_cache_dir(cache_dir),
            Ep::Webgpu => base.with_execution_provider(ExecutionProvider::WebGPU),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipResult {
    pub clip: String,
    pub audio_secs: f32,
    /// Median decode wall time. For a segment of this length, this *is* the tail the user waits on.
    pub median_ms: f64,
    pub p95_ms: f64,
    /// audio_secs / decode_secs. Above 1.0 means faster than real time.
    pub rtf: f64,
    pub wer: f32,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EpResult {
    pub ep: Ep,
    pub label: String,
    pub load_ms: f64,
    pub peak_rss_mb: f64,
    pub clips: Vec<ClipResult>,
    pub error: Option<String>,
}

/// Peak resident set size for this process. On macOS `ru_maxrss` is bytes (on Linux it is KB).
pub fn peak_rss_mb() -> f64 {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return 0.0;
        }
        let bytes = if cfg!(target_os = "macos") {
            usage.ru_maxrss as f64
        } else {
            usage.ru_maxrss as f64 * 1024.0
        };
        bytes / (1024.0 * 1024.0)
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

pub fn run(ep: Ep, model_dir: &Path, audio_dir: &Path, iters: usize) -> EpResult {
    let label = ep.label().to_string();
    let cache_dir = std::path::PathBuf::from("cache").join(format!("{ep:?}").to_lowercase());
    std::fs::create_dir_all(&cache_dir).ok();

    let load_start = Instant::now();
    let model = ParakeetTDT::from_pretrained(model_dir, Some(ep.config(&cache_dir)));

    let mut model = match model {
        Ok(m) => m,
        Err(e) => {
            return EpResult {
                ep,
                label,
                load_ms: 0.0,
                peak_rss_mb: peak_rss_mb(),
                clips: vec![],
                error: Some(format!("model load failed: {e}")),
            }
        }
    };
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

    let mut clips = Vec::new();
    let mut fatal = None;

    for clip in crate::audio::CLIPS {
        let path = crate::audio::clip_path(audio_dir, clip.name);
        let loaded = match crate::audio::load(&path) {
            Ok(l) => l,
            Err(e) => {
                fatal = Some(format!("loading {}: {e}", path.display()));
                break;
            }
        };

        // One untimed warm-up. The first inference pays lazy graph/kernel initialisation, and a
        // real app pays that once at startup, not on every dictation.
        if let Err(e) = model.transcribe_samples(
            loaded.samples.clone(),
            loaded.sample_rate,
            loaded.channels,
            None,
        ) {
            fatal = Some(format!("warm-up failed on {}: {e}", clip.name));
            break;
        }

        let mut times = Vec::with_capacity(iters);
        let mut text = String::new();

        for _ in 0..iters {
            let start = Instant::now();
            let result = model.transcribe_samples(
                loaded.samples.clone(),
                loaded.sample_rate,
                loaded.channels,
                Some(TimestampMode::Sentences),
            );
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;

            match result {
                Ok(r) => {
                    text = r.text;
                    times.push(elapsed);
                }
                Err(e) => {
                    fatal = Some(format!("transcribe failed on {}: {e}", clip.name));
                    break;
                }
            }
        }

        if fatal.is_some() {
            break;
        }

        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = percentile(&times, 0.5);
        let p95 = percentile(&times, 0.95);

        clips.push(ClipResult {
            clip: clip.name.to_string(),
            audio_secs: loaded.duration,
            median_ms: median,
            p95_ms: p95,
            rtf: (loaded.duration as f64) / (median / 1000.0),
            wer: crate::audio::wer(clip.text, &text),
            text,
        });
    }

    EpResult {
        ep,
        label,
        load_ms,
        peak_rss_mb: peak_rss_mb(),
        clips,
        error: fatal,
    }
}
