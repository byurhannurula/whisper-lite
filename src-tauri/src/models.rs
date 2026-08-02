//! Model registry, downloads, and install state.
//!
//! Models are never bundled — nothing ships with weights inside it. The registry is compile-time
//! for now; serving it as a signed remote manifest is a later refinement, and hardcoding the list
//! is not the part that needs solving first.
//!
//! Speed figures come from M0, measured on this machine for base and small. The rest are
//! extrapolated from their parameter counts and marked as estimates in the UI.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Which shelf a model sits on in the picker.
///
/// Three axes would be a matrix; one grouping that answers "what am I choosing between" is what
/// the list actually needs. English-only and compressed variants are genuinely different trade
/// offs rather than more sizes, so they get their own shelves.
pub const GROUP_GENERAL: &str = "General";
pub const GROUP_ENGLISH: &str = "English only";
pub const GROUP_COMPRESSED: &str = "Compressed";

pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub file: &'static str,
    pub size_mb: u32,
    /// Rough seconds to transcribe a typical utterance on Apple Silicon.
    pub speed: &'static str,
    pub accuracy: &'static str,
    /// 1-5, for the comparison meters. Prose alone makes two models impossible to rank at a
    /// glance, which is the whole job of this list.
    pub speed_rank: u8,
    pub accuracy_rank: u8,
    pub note: &'static str,
    pub group: &'static str,
    pub measured: bool,
}

/// Sizes are the real `content-length` of each file on Hugging Face, not estimates — a wrong
/// figure here makes `is_installed` reject a perfectly good download.
pub const REGISTRY: &[ModelSpec] = &[
    ModelSpec {
        id: "tiny",
        name: "Tiny",
        file: "ggml-tiny.bin",
        size_mb: 74,
        speed: "~0.1s",
        accuracy: "Low",
        speed_rank: 5,
        accuracy_rank: 1,
        note: "Fastest. Struggles with accents and technical words.",
        group: GROUP_GENERAL,
        measured: false,
    },
    ModelSpec {
        id: "base",
        name: "Base",
        file: "ggml-base.bin",
        size_mb: 141,
        speed: "~0.2s",
        accuracy: "Fair",
        speed_rank: 5,
        accuracy_rank: 2,
        note: "Good for short, clear dictation. The default.",
        group: GROUP_GENERAL,
        measured: true,
    },
    ModelSpec {
        id: "small",
        name: "Small",
        file: "ggml-small.bin",
        size_mb: 465,
        speed: "~0.4s",
        accuracy: "Good",
        speed_rank: 4,
        accuracy_rank: 3,
        note: "Noticeably better on long sentences and accents.",
        group: GROUP_GENERAL,
        measured: true,
    },
    ModelSpec {
        id: "medium",
        name: "Medium",
        file: "ggml-medium.bin",
        size_mb: 1462,
        speed: "~1.2s",
        accuracy: "Very good",
        speed_rank: 2,
        accuracy_rank: 4,
        note: "Strong accuracy, but you will feel the wait.",
        group: GROUP_GENERAL,
        measured: false,
    },
    ModelSpec {
        id: "large-v3-turbo",
        name: "Large v3 Turbo",
        file: "ggml-large-v3-turbo.bin",
        size_mb: 1549,
        speed: "~0.8s",
        accuracy: "Best",
        speed_rank: 3,
        accuracy_rank: 5,
        note: "Best accuracy per second of wait. Needs ~2GB of memory.",
        group: GROUP_GENERAL,
        measured: false,
    },
    ModelSpec {
        id: "large-v3",
        name: "Large v3",
        file: "ggml-large-v3.bin",
        size_mb: 2951,
        speed: "~2.5s",
        accuracy: "Best",
        speed_rank: 1,
        accuracy_rank: 5,
        note: "The most accurate Whisper there is, and the slowest. ~4GB of memory.",
        group: GROUP_GENERAL,
        measured: false,
    },
    // English-only builds. Same architecture, trained on English alone, so they beat their
    // multilingual counterpart at the same size — worth having if you never dictate anything else.
    ModelSpec {
        id: "tiny.en",
        name: "Tiny English",
        file: "ggml-tiny.en.bin",
        size_mb: 74,
        speed: "~0.1s",
        accuracy: "Fair",
        speed_rank: 5,
        accuracy_rank: 2,
        note: "English only, and better at it than Tiny.",
        group: GROUP_ENGLISH,
        measured: false,
    },
    ModelSpec {
        id: "base.en",
        name: "Base English",
        file: "ggml-base.en.bin",
        size_mb: 141,
        speed: "~0.2s",
        accuracy: "Good",
        speed_rank: 5,
        accuracy_rank: 3,
        note: "The sweet spot if you only ever dictate English.",
        group: GROUP_ENGLISH,
        measured: false,
    },
    ModelSpec {
        id: "small.en",
        name: "Small English",
        file: "ggml-small.en.bin",
        size_mb: 465,
        speed: "~0.4s",
        accuracy: "Very good",
        speed_rank: 4,
        accuracy_rank: 4,
        note: "Handles accents and jargon well without a long wait.",
        group: GROUP_ENGLISH,
        measured: false,
    },
    ModelSpec {
        id: "medium.en",
        name: "Medium English",
        file: "ggml-medium.en.bin",
        size_mb: 1462,
        speed: "~1.2s",
        accuracy: "Best",
        speed_rank: 2,
        accuracy_rank: 5,
        note: "Near-Large accuracy on English, at half the download.",
        group: GROUP_ENGLISH,
        measured: false,
    },
    // Quantised builds. Materially smaller and a little faster for a small accuracy cost, which
    // is the trade most people should take on the bigger models.
    ModelSpec {
        id: "base-q5_1",
        name: "Base (compressed)",
        file: "ggml-base-q5_1.bin",
        size_mb: 56,
        speed: "~0.2s",
        accuracy: "Fair",
        speed_rank: 5,
        accuracy_rank: 2,
        note: "Base at 40% of the size. Barely any accuracy lost.",
        group: GROUP_COMPRESSED,
        measured: false,
    },
    ModelSpec {
        id: "small-q5_1",
        name: "Small (compressed)",
        file: "ggml-small-q5_1.bin",
        size_mb: 181,
        speed: "~0.4s",
        accuracy: "Good",
        speed_rank: 4,
        accuracy_rank: 3,
        note: "Small accuracy for a Base-sized download.",
        group: GROUP_COMPRESSED,
        measured: false,
    },
    ModelSpec {
        id: "large-v3-turbo-q5_0",
        name: "Large v3 Turbo (compressed)",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size_mb: 547,
        speed: "~0.8s",
        accuracy: "Very good",
        speed_rank: 3,
        accuracy_rank: 4,
        note: "A third of the size of Large v3 Turbo. The best all-rounder here.",
        group: GROUP_COMPRESSED,
        measured: false,
    },
    ModelSpec {
        id: "large-v3-q5_0",
        name: "Large v3 (compressed)",
        file: "ggml-large-v3-q5_0.bin",
        size_mb: 1031,
        speed: "~2.2s",
        accuracy: "Best",
        speed_rank: 1,
        accuracy_rank: 5,
        note: "Large v3 accuracy for a third of the disk.",
        group: GROUP_COMPRESSED,
        measured: false,
    },
];

pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    REGISTRY.iter().find(|m| m.id == id)
}

pub fn models_dir() -> PathBuf {
    crate::settings::dir().join("models")
}

pub fn path_for(id: &str) -> Option<PathBuf> {
    spec(id).map(|m| models_dir().join(m.file))
}

pub fn is_installed(id: &str) -> bool {
    match (spec(id), path_for(id)) {
        (Some(m), Some(p)) => std::fs::metadata(&p)
            .map(|meta| {
                // A partial file from an interrupted download would otherwise look installed and
                // then fail to load. Allow 5% slack against the published size.
                meta.len() > (m.size_mb as u64 * 1024 * 1024 * 95) / 100
            })
            .unwrap_or(false),
        _ => false,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub size_mb: u32,
    pub speed: String,
    pub accuracy: String,
    pub speed_rank: u8,
    pub accuracy_rank: u8,
    pub note: String,
    pub group: String,
    pub measured: bool,
    pub installed: bool,
    pub active: bool,
}

pub fn list(active: &str) -> Vec<ModelInfo> {
    REGISTRY
        .iter()
        .map(|m| ModelInfo {
            id: m.id.to_string(),
            name: m.name.to_string(),
            size_mb: m.size_mb,
            speed: m.speed.to_string(),
            accuracy: m.accuracy.to_string(),
            speed_rank: m.speed_rank,
            accuracy_rank: m.accuracy_rank,
            note: m.note.to_string(),
            group: m.group.to_string(),
            measured: m.measured,
            installed: is_installed(m.id),
            active: m.id == active,
        })
        .collect()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub id: String,
    pub received_mb: u64,
    pub total_mb: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// Downloads a model, reporting progress through `on_progress`.
///
/// Writes to a `.partial` file and renames on success, so an interrupted download can never be
/// mistaken for a complete one.
pub fn download(
    id: &str,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(Progress),
) -> Result<()> {
    let Some(model) = spec(id) else {
        bail!("unknown model: {id}");
    };

    let dir = models_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let dest = dir.join(model.file);
    let tmp = dest.with_extension("partial");

    let url = format!("{BASE_URL}/{}", model.file);
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .context("building http client")?;

    let mut response = client.get(&url).send().context("starting download")?;
    if !response.status().is_success() {
        bail!("{url} returned HTTP {}", response.status());
    }

    let total = response.content_length().unwrap_or(0);
    let total_mb = total / (1024 * 1024);

    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;

    let mut buffer = vec![0u8; 512 * 1024];
    let mut received: u64 = 0;
    let mut last_reported_mb = 0;

    loop {
        if cancel.load(Ordering::SeqCst) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            bail!("cancelled");
        }

        let n = response.read(&mut buffer).context("reading response")?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n]).context("writing model file")?;
        received += n as u64;

        // Report per megabyte rather than per chunk; the UI cannot use 500KB granularity and
        // each event costs an IPC round trip.
        let received_mb = received / (1024 * 1024);
        if received_mb > last_reported_mb {
            last_reported_mb = received_mb;
            on_progress(Progress {
                id: id.to_string(),
                received_mb,
                total_mb,
                done: false,
                error: None,
            });
        }
    }

    file.flush().context("flushing model file")?;
    drop(file);
    std::fs::rename(&tmp, &dest).context("finalising model file")?;

    on_progress(Progress {
        id: id.to_string(),
        received_mb: total_mb,
        total_mb,
        done: true,
        error: None,
    });

    Ok(())
}

pub fn delete(id: &str) -> Result<()> {
    let Some(path) = path_for(id) else {
        bail!("unknown model: {id}");
    };
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_are_unique() {
        let mut ids: Vec<_> = REGISTRY.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate model id in registry");
    }

    #[test]
    fn default_model_exists_in_registry() {
        assert!(spec("base").is_some());
    }

    #[test]
    fn unknown_model_has_no_path() {
        assert!(path_for("does-not-exist").is_none());
        assert!(!is_installed("does-not-exist"));
    }

    #[test]
    fn ranks_are_on_the_meter_scale() {
        // The UI paints these as five segments, so anything outside 1-5 would silently clip.
        for m in REGISTRY {
            assert!(
                (1..=5).contains(&m.speed_rank),
                "{} has speed_rank {}",
                m.id,
                m.speed_rank
            );
            assert!(
                (1..=5).contains(&m.accuracy_rank),
                "{} has accuracy_rank {}",
                m.id,
                m.accuracy_rank
            );
        }
    }

    #[test]
    fn every_model_is_on_a_known_shelf() {
        for m in REGISTRY {
            assert!(
                [GROUP_GENERAL, GROUP_ENGLISH, GROUP_COMPRESSED].contains(&m.group),
                "{} has an unknown group '{}'",
                m.id,
                m.group
            );
        }
    }

    #[test]
    fn every_model_has_a_distinct_file() {
        let mut files: Vec<_> = REGISTRY.iter().map(|m| m.file).collect();
        files.sort_unstable();
        let count = files.len();
        files.dedup();
        assert_eq!(files.len(), count, "two models share a filename");
    }
}
