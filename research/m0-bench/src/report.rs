//! Output formatting.
//!
//! M0's deliverable is "a table of numbers, not a decision to make". This module is
//! that table, plus the one derived judgement worth stating: does the 6s tail fit the budget?

use crate::bench::EpResult;
use anyhow::Result;
use std::fmt::Write as _;
use std::path::Path;

/// 6s is the force-cut, so the `mid` clip's decode time is the worst-case tail a user
/// waits on. The rest of the perceived-latency budget (capture start, post-processing, injection)
/// is ~70-180ms, so the decode has roughly this much room inside the 400ms p50 target.
const TAIL_BUDGET_MS: f64 = 250.0;

pub fn print_ep(r: &EpResult) {
    println!(
        "\n{} — load {:.0}ms, peak RSS {:.0}MB",
        r.label, r.load_ms, r.peak_rss_mb
    );
    if let Some(e) = &r.error {
        println!("  ERROR: {e}");
        return;
    }
    for c in &r.clips {
        println!(
            "  {:<6} {:>5.1}s audio → {:>7.1}ms  ({:>5.1}x realtime, WER {:.0}%)",
            c.clip,
            c.audio_secs,
            c.median_ms,
            c.rtf,
            c.wer * 100.0
        );
        println!("         “{}”", c.text.trim());
    }
}

/// Column headers from the clips actually measured, so the table can never claim a duration the
/// audio does not have. `say` output lands close to the target lengths but not exactly on them.
fn headers(results: &[EpResult]) -> Vec<String> {
    let sample = results.iter().find(|r| !r.clips.is_empty());
    ["short", "mid", "long", "xlong"]
        .iter()
        .map(|name| {
            sample
                .and_then(|r| r.clips.iter().find(|c| c.clip == *name))
                .map(|c| format!("{:.1}s", c.audio_secs))
                .unwrap_or_else(|| (*name).to_string())
        })
        .collect()
}

pub fn print_summary(results: &[EpResult]) {
    let h = headers(results);
    println!("=== Decode time by clip length (median ms) ===\n");
    println!(
        "  {:<18} {:>8} {:>8} {:>8} {:>8} {:>9} {:>8}",
        "provider", h[0], h[1], h[2], h[3], "peak RSS", "load"
    );
    println!("  {}", "-".repeat(72));

    for r in results {
        if r.error.is_some() {
            println!("  {:<18} {:>8}", r.label, "failed");
            continue;
        }
        let get = |name: &str| {
            r.clips
                .iter()
                .find(|c| c.clip == name)
                .map(|c| format!("{:.0}", c.median_ms))
                .unwrap_or_else(|| "-".into())
        };
        println!(
            "  {:<18} {:>8} {:>8} {:>8} {:>8} {:>7.0}MB {:>7.0}ms",
            r.label,
            get("short"),
            get("mid"),
            get("long"),
            get("xlong"),
            r.peak_rss_mb,
            r.load_ms
        );
    }

    println!("\n=== Verdict ===\n");

    let mut ranked: Vec<_> = results
        .iter()
        .filter(|r| r.error.is_none())
        .filter_map(|r| r.clips.iter().find(|c| c.clip == "mid").map(|c| (r, c)))
        .collect();
    ranked.sort_by(|a, b| a.1.median_ms.partial_cmp(&b.1.median_ms).unwrap());

    if ranked.is_empty() {
        println!("  No execution provider completed. Fall back to whisper-rs.");
        return;
    }

    for (r, c) in &ranked {
        let verdict = if c.wer > 0.35 {
            "OUTPUT IS WRONG — unusable regardless of speed"
        } else if c.median_ms <= TAIL_BUDGET_MS {
            "fits the 6s-tail budget"
        } else {
            "too slow for the 400ms p50 target"
        };
        println!(
            "  {:<18} 6s tail {:>6.0}ms  WER {:>3.0}%   {}",
            r.label,
            c.median_ms,
            c.wer * 100.0,
            verdict
        );
    }

    let (best, bc) = ranked
        .iter()
        .find(|(_, c)| c.wer <= 0.35)
        .unwrap_or(&ranked[0]);

    println!(
        "\n  Winner: {} at {:.0}ms for a 6s segment.",
        best.label, bc.median_ms
    );
    if bc.median_ms <= TAIL_BUDGET_MS {
        println!("  The latency thesis holds. Proceed to M1.");
    } else {
        println!(
            "  Over the {TAIL_BUDGET_MS:.0}ms budget. Either lower the force-cut below 6s, \n  \
             or fall back to whisper-rs."
        );
    }
}

pub fn write_markdown(path: &Path, results: &[EpResult], iters: usize) -> Result<()> {
    let mut s = String::new();

    writeln!(s, "# M0 results\n")?;
    writeln!(
        s,
        "Machine: {}\n",
        std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    )?;
    writeln!(
        s,
        "Model: Parakeet TDT 0.6B v3, ONNX int8, via `parakeet-rs` 0.3.7 (ONNX Runtime).\n"
    )?;
    writeln!(
        s,
        "Method: {iters} timed iterations per clip after one untimed warm-up; one process per \
         execution provider so peak RSS is not contaminated across runs. Clips are macOS `say` \
         TTS, so **absolute WER is optimistic** — it is here to catch a provider returning \
         garbage, not to publish an accuracy number.\n"
    )?;

    let h = headers(results);
    writeln!(s, "## Decode time by clip length (median ms)\n")?;
    writeln!(
        s,
        "| Provider | {} | {} | {} | {} | Peak RSS | Model load |",
        h[0], h[1], h[2], h[3]
    )?;
    writeln!(s, "|---|---|---|---|---|---|---|")?;

    for r in results {
        if let Some(e) = &r.error {
            writeln!(
                s,
                "| {} | \\- | \\- | \\- | \\- | \\- | **failed**: {e} |",
                r.label
            )?;
            continue;
        }
        let get = |name: &str| {
            r.clips
                .iter()
                .find(|c| c.clip == name)
                .map(|c| format!("{:.0}ms", c.median_ms))
                .unwrap_or_else(|| "-".into())
        };
        writeln!(
            s,
            "| {} | {} | {} | {} | {} | {:.0}MB | {:.0}ms |",
            r.label,
            get("short"),
            get("mid"),
            get("long"),
            get("xlong"),
            r.peak_rss_mb,
            r.load_ms
        )?;
    }

    writeln!(s, "\n## Real-time factor and accuracy\n")?;
    writeln!(s, "| Provider | Clip | Audio | Median | p95 | RTF | WER |")?;
    writeln!(s, "|---|---|---|---|---|---|---|")?;
    for r in results {
        for c in &r.clips {
            writeln!(
                s,
                "| {} | {} | {:.1}s | {:.0}ms | {:.0}ms | {:.0}x | {:.0}% |",
                r.label,
                c.clip,
                c.audio_secs,
                c.median_ms,
                c.p95_ms,
                c.rtf,
                c.wer * 100.0
            )?;
        }
    }

    writeln!(s, "\n## Transcripts\n")?;
    writeln!(
        s,
        "Checked against the reference text to catch numerically wrong providers.\n"
    )?;
    for r in results {
        writeln!(s, "**{}**\n", r.label)?;
        if let Some(e) = &r.error {
            writeln!(s, "- failed: {e}\n")?;
            continue;
        }
        for c in &r.clips {
            writeln!(
                s,
                "- `{}` (WER {:.0}%): {}",
                c.clip,
                c.wer * 100.0,
                c.text.trim()
            )?;
        }
        writeln!(s)?;
    }

    writeln!(s, "## What this decides\n")?;
    writeln!(
        s,
        "Segmentation force-cuts at 6s, so the **6s column is the worst-case tail** the user \
         waits on after releasing the key. Budget for that decode is ~{TAIL_BUDGET_MS:.0}ms inside \
         the 400ms p50 target, once capture start, post-processing and injection are accounted \
         for.\n"
    )?;

    std::fs::write(path, s)?;
    Ok(())
}
