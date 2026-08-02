//! Downloads the Parakeet TDT v3 ONNX weights.
//!
//! int8 only. The fp32 encoder ships a 2.4GB `.onnx.data` sidecar, which is a non-starter
//! for an app whose whole pitch is being small — so there is no reason to benchmark it.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const REPO: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";

/// `parakeet-rs` probes for these exact names, preferring fp32 then int8, so the int8
/// files are picked up without renaming.
const FILES: &[&str] = &[
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "vocab.txt",
];

pub fn model_dir() -> PathBuf {
    PathBuf::from("models/parakeet-tdt-v3-int8")
}

pub fn ensure(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;

    for name in FILES {
        let dest = dir.join(name);
        if dest.exists() {
            let size = fs::metadata(&dest)?.len();
            if size > 0 {
                println!("  ✓ {name} ({})", human(size));
                continue;
            }
        }
        download(&format!("{REPO}/{name}"), &dest)
            .with_context(|| format!("downloading {name}"))?;
    }

    Ok(())
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    print!("  ↓ {name} ");
    std::io::stdout().flush().ok();

    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    let mut resp = client.get(url).send()?;

    if !resp.status().is_success() {
        bail!("{} returned HTTP {}", url, resp.status());
    }

    let total = resp.content_length().unwrap_or(0);

    // Download to a temp path and rename on success, so an interrupted run never leaves a
    // truncated file that looks complete on the next pass.
    let tmp = dest.with_extension("partial");
    let mut out = fs::File::create(&tmp)?;

    let mut buf = vec![0u8; 1 << 20];
    let mut written: u64 = 0;
    let mut last_pct = 0;

    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        written += n as u64;

        if total > 0 {
            let pct = (written * 100 / total) as u32;
            if pct >= last_pct + 10 {
                print!("{pct}% ");
                std::io::stdout().flush().ok();
                last_pct = pct;
            }
        }
    }

    out.flush()?;
    drop(out);
    fs::rename(&tmp, dest)?;

    println!("done ({})", human(written));
    Ok(())
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.0}{}", UNITS[u])
}

pub fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}
