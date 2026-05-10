//! Whisper parallelism investigation for WI-UX (#60).
//!
//! Measures whether `tokio::task::spawn_blocking` calls to whisper-rs
//! actually parallelize on Metal (macOS) when sharing a single
//! `Arc<WhisperContext>`. Each task creates its own state via
//! `ctx.create_state()`.
//!
//! ## Method
//!
//! 1. Load the configured model.
//! 2. Generate a synthetic `UTTERANCE_SECONDS`-second two-tone utterance
//!    (440 + 880 Hz, 16 kHz mono — long enough to exceed Whisper's 1 s
//!    minimum input).
//! 3. **Solo timing:** transcribe the utterance once, sequentially. Repeat
//!    `N_SOLO` times, take the median.
//! 4. **Concurrent timing:** spawn `N_CONCURRENT` `spawn_blocking` tasks
//!    transcribing the same utterance simultaneously. Wait for all to
//!    finish. Record total wall time.
//! 5. Report:
//!    - solo_ms (median)
//!    - concurrent_total_ms
//!    - concurrent_per_task_ms = concurrent_total_ms / N_CONCURRENT
//!    - speedup = solo_ms / concurrent_per_task_ms
//!
//! Interpretation:
//! - speedup ≈ 1.0 → tasks serialize (concurrent ≈ N × solo). File a
//!   sub-issue for explicit parallelization (separate WhisperContexts or
//!   batch processing). Tauri-Unmix should then use two separate contexts.
//! - speedup ≫ 1.0 → tasks parallelize. UX progress counter alone is
//!   sufficient. Tauri-Unmix can keep a shared context.
//!
//! ## Run
//!
//! ```
//! cargo run --release -p meetily-client --example whisper_parallel_bench \
//!   -- ~/.local/share/meetily/models/ggml-large-v3-turbo.bin
//! ```

use anyhow::{Context, Result};
use meetily_client::transcribe::{load_model, transcribe_chunk};
use std::sync::Arc;
use std::time::Instant;

const SAMPLE_RATE: u32 = 16_000;
const UTTERANCE_SECONDS: usize = 3;
const N_SOLO: usize = 5;
const N_CONCURRENT: usize = 4;

fn synth_utterance() -> Vec<f32> {
    // Make sure we exceed Whisper's 1 s minimum input.
    let n = SAMPLE_RATE as usize * UTTERANCE_SECONDS;
    let mut buf = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / SAMPLE_RATE as f32;
        // Two-tone signal so Whisper has something less trivial to chew on
        // than a pure sine wave.
        let s = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.2
            + (2.0 * std::f32::consts::PI * 880.0 * t).sin() * 0.1;
        buf.push(s);
    }
    buf
}

#[tokio::main]
async fn main() -> Result<()> {
    let model_path = std::env::args()
        .nth(1)
        .context("usage: whisper_parallel_bench <path-to-ggml-model>")?;

    eprintln!("Loading model from {model_path} ...");
    let whisper = Arc::new(load_model(&model_path)?);
    let utt = synth_utterance();
    eprintln!(
        "Loaded. Synthesized {}-sample utterance ({}s @ {}Hz).",
        utt.len(),
        UTTERANCE_SECONDS,
        SAMPLE_RATE
    );

    // Warm-up: discarded.
    eprintln!("Warm-up transcribe (discarded) ...");
    transcribe_chunk(&utt, &whisper, "warmup", 0.0)?;

    // Solo timings.
    eprintln!("Solo timing: {} runs", N_SOLO);
    let mut solo_ms: Vec<u128> = Vec::with_capacity(N_SOLO);
    for i in 0..N_SOLO {
        let start = Instant::now();
        transcribe_chunk(&utt, &whisper, "solo", 0.0)?;
        let elapsed_ms = start.elapsed().as_millis();
        eprintln!("  run {}: {} ms", i + 1, elapsed_ms);
        solo_ms.push(elapsed_ms);
    }
    solo_ms.sort();
    let solo_median_ms = solo_ms[solo_ms.len() / 2] as f64;

    // Concurrent timing.
    eprintln!("Concurrent timing: {} tasks via spawn_blocking", N_CONCURRENT);
    let start = Instant::now();
    let mut handles = Vec::with_capacity(N_CONCURRENT);
    for i in 0..N_CONCURRENT {
        let whisper = whisper.clone();
        let utt = utt.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            let task_start = Instant::now();
            transcribe_chunk(&utt, &whisper, "concurrent", 0.0)
                .map(|_| (i, task_start.elapsed().as_millis()))
        }));
    }
    for h in handles {
        let (i, ms) = h.await??;
        eprintln!("  task {} finished in {} ms (per-task wall)", i, ms);
    }
    let concurrent_total_ms = start.elapsed().as_millis() as f64;
    let concurrent_per_task_ms = concurrent_total_ms / N_CONCURRENT as f64;
    let speedup = solo_median_ms / concurrent_per_task_ms;

    eprintln!();
    println!("=== whisper-rs parallelism on this box ===");
    println!("  solo_median_ms          = {:.0}", solo_median_ms);
    println!("  concurrent_total_ms     = {:.0}", concurrent_total_ms);
    println!("  concurrent_per_task_ms  = {:.0}", concurrent_per_task_ms);
    println!("  N_CONCURRENT            = {}", N_CONCURRENT);
    println!("  speedup vs solo         = {:.2}x", speedup);
    println!();
    if speedup < 1.3 {
        println!(
            "VERDICT: tasks SERIALIZE. concurrent ≈ N × solo. UX progress \
             counter alone is enough; explicit parallelization (separate \
             contexts or batch transcribe) is a sub-issue. Tauri-Unmix \
             should plan two separate WhisperContexts for its parallel chains."
        );
    } else {
        println!(
            "VERDICT: tasks PARALLELIZE. speedup is {:.2}x with N={}; \
             progress counter alone is enough.",
            speedup, N_CONCURRENT
        );
    }

    Ok(())
}
