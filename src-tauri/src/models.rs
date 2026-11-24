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

/// Pinned to a commit, not `main`: `bytes` and `sha256` in the registry describe the files at this
/// revision, and a moving branch would turn every upstream re-upload into a failed checksum.
const BASE_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1";

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
    /// Exact length and SHA-256 of `file` at the pinned revision, from the Hugging Face LFS
    /// metadata. whisper.cpp parses the file with no validation of its own, so nothing loads
    /// until it matches both.
    pub bytes: u64,
    pub sha256: &'static str,
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

/// `size_mb` is for display only; `bytes` is what install checks compare against.
pub const REGISTRY: &[ModelSpec] = &[
    ModelSpec {
        id: "tiny",
        name: "Tiny",
        file: "ggml-tiny.bin",
        bytes: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
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
        bytes: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
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
        bytes: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
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
        bytes: 1_533_763_059,
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
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
        bytes: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
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
        bytes: 3_095_033_483,
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
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
        bytes: 77_704_715,
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
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
        bytes: 147_964_211,
        sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
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
        bytes: 487_614_201,
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
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
        bytes: 1_533_774_781,
        sha256: "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356",
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
        bytes: 59_707_625,
        sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
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
        bytes: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
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
        bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
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
        bytes: 1_081_140_203,
        sha256: "d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1",
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
        // Exact length only, not the hash: this runs on every launch and every model listing,
        // and hashing 3GB each time is too slow. The hash is checked once, before the rename
        // that makes a download visible here.
        (Some(m), Some(p)) => std::fs::metadata(&p)
            .map(|meta| meta.len() == m.bytes)
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
/// Writes to a `.partial` file and renames it only once its length and SHA-256 match the
/// registry, so a truncated or tampered file can never be mistaken for a complete one.
///
/// A failed download keeps its `.partial` file, and the next attempt resumes from it with an
/// HTTP Range request. A cancelled one deletes it: cancelling means the user wants it gone.
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

    let mut have = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if have >= model.bytes {
        // Full length but never renamed, so it failed the hash. Resuming would only append.
        let _ = std::fs::remove_file(&tmp);
        have = 0;
    }

    let url = format!("{BASE_URL}/{}", model.file);
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .context("building http client")?;

    let mut request = client.get(&url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().context("starting download")?;

    let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !response.status().is_success() {
        bail!("{url} returned HTTP {}", response.status());
    }
    if !resumed {
        // The server ignored the Range header and sent the whole file.
        have = 0;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&tmp)
        .with_context(|| format!("opening {}", tmp.display()))?;

    let total_mb = model.bytes / MB;
    let mut buffer = vec![0u8; 512 * 1024];
    let mut received = have;
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
        if received + n as u64 > model.bytes {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            bail!("server sent more data than expected for {}", model.file);
        }
        file.write_all(&buffer[..n]).context("writing model file")?;
        received += n as u64;

        // Report per megabyte rather than per chunk; the UI cannot use 500KB granularity and
        // each event costs an IPC round trip.
        let received_mb = received / MB;
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

    // A dropped connection can end the body cleanly, so the read loop alone cannot tell a
    // finished download from a cut one.
    if received != model.bytes {
        bail!(
            "download stopped at {} of {} MB; try again to resume",
            received / MB,
            total_mb
        );
    }

    let actual = sha256_file(&tmp)?;
    if actual != model.sha256 {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "{} failed its checksum (expected {}, got {actual}); deleted it",
            model.file,
            model.sha256
        );
    }

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

const MB: u64 = 1024 * 1024;

pub fn delete(id: &str) -> Result<()> {
    let Some(path) = path_for(id) else {
        bail!("unknown model: {id}");
    };
    // The resumable leftover from a failed download goes too, or it sits on disk unseen.
    let _ = std::fs::remove_file(path.with_extension("partial"));
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
    }
    Ok(())
}

fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).context("hashing model file")?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_a_hash_and_matching_display_size() {
        for m in REGISTRY {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(m.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{}", m.id);
            // size_mb is shown in the UI; it should round from the real length.
            let mb = m.bytes as f64 / MB as f64;
            assert!(
                (mb - m.size_mb as f64).abs() <= 1.0,
                "{}: {mb:.1} vs {}",
                m.id,
                m.size_mb
            );
        }
    }

    #[test]
    fn sha256_file_matches_a_known_digest() {
        let path = std::env::temp_dir().join(format!("wl-sha-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let digest = sha256_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

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
