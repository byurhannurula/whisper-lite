//! M0 spike for whisper-lite.
//!
//! Answers one question with numbers: on *this* machine, is Parakeet TDT fast enough that the
//! decode of a 6-second segment hides behind a natural pause?
//!
//! Throwaway by design. Nothing here graduates into the app.

mod audio;
mod bench;
mod models;
mod report;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "m0-bench",
    about = "whisper-lite M0: measure Parakeet on this machine"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download the Parakeet TDT v3 int8 weights (~650MB, once).
    Fetch,
    /// Generate the test clips with `say` + `afconvert`.
    Audio,
    /// Benchmark a single execution provider and print JSON. Used internally by `all`.
    Bench {
        #[arg(long, value_enum)]
        ep: bench::Ep,
        #[arg(long, default_value_t = 5)]
        iters: usize,
        /// Emit JSON only, for the parent process to aggregate.
        #[arg(long)]
        json: bool,
    },
    /// Run the whole matrix (one subprocess per EP) and write M0-RESULTS.md.
    All {
        #[arg(long, default_value_t = 5)]
        iters: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let model_dir = models::model_dir();
    let audio_dir = audio::audio_dir();

    match cli.cmd {
        Cmd::Fetch => {
            println!(
                "Fetching Parakeet TDT v3 (int8) into {}",
                model_dir.display()
            );
            models::ensure(&model_dir)?;
            println!(
                "\nTotal on disk: {}",
                models::human(models::dir_size(&model_dir))
            );
        }

        Cmd::Audio => {
            println!("Generating test clips into {}", audio_dir.display());
            audio::ensure(&audio_dir)?;
        }

        Cmd::Bench { ep, iters, json } => {
            let result = bench::run(ep, &model_dir, &audio_dir, iters);
            if json {
                println!("{}", serde_json::to_string(&result)?);
            } else {
                report::print_ep(&result);
            }
        }

        Cmd::All { iters } => run_all(&model_dir, &audio_dir, iters)?,
    }

    Ok(())
}

fn run_all(model_dir: &PathBuf, audio_dir: &PathBuf, iters: usize) -> Result<()> {
    println!("== whisper-lite M0 ==\n");

    println!("Models:");
    models::ensure(model_dir)?;
    println!("\nAudio:");
    audio::ensure(audio_dir)?;

    let exe = std::env::current_exe().context("locating own binary")?;
    let mut results = Vec::new();

    println!("\nBenchmarking ({iters} iterations per clip, after one warm-up):\n");

    for ep in bench::Ep::all() {
        print!("  {:<18} ", ep.label());
        use std::io::Write;
        std::io::stdout().flush().ok();

        // Separate process per EP so peak RSS is that EP's own high-water mark, and so an EP
        // that hard-crashes the ONNX runtime does not take the whole run down with it.
        let out = std::process::Command::new(&exe)
            .args(["bench", "--json", "--ep"])
            .arg(ep_flag(*ep))
            .args(["--iters", &iters.to_string()])
            .output();

        match out {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let line = stdout.lines().last().unwrap_or("");
                match serde_json::from_str::<bench::EpResult>(line) {
                    Ok(r) => {
                        match (&r.error, r.clips.iter().find(|c| c.clip == "mid")) {
                            (Some(e), _) => println!("FAILED — {e}"),
                            (None, Some(c)) => println!("ok ({:.0}x realtime on mid)", c.rtf),
                            (None, None) => println!("no clips measured"),
                        }
                        results.push(r);
                    }
                    Err(e) => println!("unparseable output: {e}"),
                }
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let tail = err.lines().rev().take(2).collect::<Vec<_>>().join(" | ");
                println!("CRASHED — {tail}");
                results.push(bench::EpResult {
                    ep: *ep,
                    label: ep.label().to_string(),
                    load_ms: 0.0,
                    peak_rss_mb: 0.0,
                    clips: vec![],
                    error: Some(format!("process failed: {tail}")),
                });
            }
            Err(e) => println!("could not spawn: {e}"),
        }
    }

    println!();
    report::print_summary(&results);
    let path = PathBuf::from("M0-RESULTS.md");
    report::write_markdown(&path, &results, iters)?;
    println!("\nWrote {}", path.display());

    Ok(())
}

fn ep_flag(ep: bench::Ep) -> &'static str {
    match ep {
        bench::Ep::Cpu => "cpu",
        bench::Ep::Coreml => "coreml",
        bench::Ep::CoremlAne => "coreml-ane",
        bench::Ep::Webgpu => "webgpu",
    }
}
