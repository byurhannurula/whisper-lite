//! Transcription.
//!
//! Whisper via whisper.cpp with Metal. M0 measured Base at 294MB resident and a ~190ms tail,
//! which is why Whisper is the default engine rather than Parakeet (1.47GB).
//!
//! The model is loaded once and reloaded when the user picks a different one. The *first* load
//! on a machine compiles Metal shaders and takes ~11s; every load after that is ~100ms because
//! the compiled result is cached by the system.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

pub struct Engine {
    // Field order is the drop order, and it matters: the state must be released before the
    // context that owns it.
    //
    // The state is created once and reused for every transcription. Creating one per call meant
    // a full Metal init and teardown on each dictation, which cost ~100ms of setup and tripped
    // ggml's `GGML_ASSERT([rsets->data count] == 0)` on the Metal residency set — the crash
    // reporter that appeared on quit.
    state: Mutex<WhisperState>,
    // Never read after construction, but it owns the memory `state` borrows from and must
    // outlive it. Dropping it early is a use-after-free, so this is load-bearing dead code.
    #[allow(dead_code)]
    ctx: WhisperContext,
    model_id: String,
}

impl Engine {
    pub fn load(model_id: &str, model_path: &Path) -> Result<Self> {
        if !model_path.exists() {
            bail!(
                "model '{model_id}' is not downloaded.\n  Expected at {}",
                model_path.display()
            );
        }

        let ctx = WhisperContext::new_with_params(
            model_path
                .to_str()
                .context("model path is not valid UTF-8")?,
            WhisperContextParameters::default(),
        )
        .with_context(|| format!("loading model '{model_id}'"))?;

        let state = ctx.create_state().context("creating whisper state")?;

        Ok(Self {
            state: Mutex::new(state),
            ctx,
            model_id: model_id.to_string(),
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Transcribes one complete utterance.
    ///
    /// The whole recording goes through in a single call rather than being segmented. That is a
    /// deliberate simplification the M0 numbers permit: Whisper pads to a fixed 30s window, so a
    /// 26s utterance costs ~590ms and a 2s one costs ~150ms. Segmentation is therefore about
    /// showing text progressively (M2), not about keeping latency down.
    pub fn transcribe(
        &self,
        samples: &[f32],
        language: &str,
        accurate: bool,
        vocabulary: &str,
    ) -> Result<String> {
        // Below ~0.3s there is nothing to transcribe, and Whisper's window padding makes it
        // likely to hallucinate a phrase into the silence.
        if samples.len() < (crate::audio::TARGET_RATE as usize) / 3 {
            return Ok(String::new());
        }

        // Beam search is materially better than greedy on long or accented speech, at roughly
        // 1.5-2x the decode time. With Base finishing in ~190ms there is budget for it.
        let strategy = if accurate {
            SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: -1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };

        let mut params = FullParams::new(strategy);

        if language != "auto" {
            params.set_language(Some(language));
        }
        params.set_n_threads(4);
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // Whisper's decoder always wants to emit text; these two stop it narrating the audio.
        params.set_suppress_blank(true);
        // The direct fix for "(foreign language)", "(mumbling)", "[BLANK_AUDIO]" and "♪":
        // non-speech tokens are blocked at the decoder rather than stripped afterwards, so they
        // never displace real words in the first place.
        params.set_suppress_nst(true);

        // Quality gates. When a decode comes out below these thresholds whisper.cpp retries at a
        // higher temperature instead of accepting a bad first pass. These are whisper.cpp's own
        // defaults, which the Rust binding does not apply for you.
        params.set_no_speech_thold(0.6);
        params.set_entropy_thold(2.4);
        params.set_logprob_thold(-1.0);
        params.set_temperature_inc(0.2);

        // Whisper accepts a prompt, which conditions both vocabulary and writing style. It is the
        // cheapest accuracy lever available: naming the terms you actually use makes the decoder
        // far likelier to spell them correctly. Capped because the prompt competes with the audio
        // for the model's limited text context.
        if !vocabulary.trim().is_empty() {
            let prompt: String = vocabulary.trim().chars().take(800).collect();
            params.set_initial_prompt(&prompt);
        }

        // Reset the prompt between dictations.
        //
        // whisper.cpp clears `prompt_past` at the top of `whisper_full` only when this is true,
        // then repopulates it per 30s window *within* that call regardless. So coherence across
        // windows of one long utterance is preserved either way — while `false` additionally
        // conditions this dictation on the previous one's tokens. With a `WhisperState` reused
        // across calls that means the model can continue or repeat whatever was said last time.
        params.set_no_context(true);

        // Fall back through higher temperatures only when decoding fails its quality checks,
        // rather than accepting a bad first pass.
        params.set_temperature(0.0);

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("whisper state is poisoned"))?;

        state.full(params, samples).context("whisper inference")?;

        let mut text = String::new();
        for i in 0..state.full_n_segments() {
            if let Some(segment) = state.get_segment(i) {
                text.push_str(&segment.to_str_lossy().unwrap_or_default());
            }
        }

        Ok(text)
    }
}

/// Path for a model id, honouring an override for development.
pub fn model_path(model_id: &str) -> PathBuf {
    if let Ok(p) = std::env::var("WHISPER_LITE_MODEL") {
        return PathBuf::from(p);
    }
    crate::models::path_for(model_id)
        .unwrap_or_else(|| crate::models::models_dir().join("ggml-base.bin"))
}
