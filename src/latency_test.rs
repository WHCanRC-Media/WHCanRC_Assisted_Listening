use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::audio::AudioChunk;

/// Generate a 2ms linear chirp sweep (2 kHz → 8 kHz) at the given sample rate.
/// A frequency sweep has much better autocorrelation properties than a pure tone,
/// making it easier to detect reliably even after Opus compression.
pub fn generate_chirp(sample_rate: u32) -> Vec<f32> {
    let duration_s: f32 = 0.002; // 2 ms
    let num_samples = (sample_rate as f32 * duration_s) as usize;
    let f0: f32 = 2000.0;
    let f1: f32 = 8000.0;

    (0..num_samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            // Instantaneous phase of a linear chirp
            let phase = 2.0
                * std::f32::consts::PI
                * (f0 * t + 0.5 * (f1 - f0) * t * t / duration_s);
            phase.sin() * 0.8
        })
        .collect()
}

/// Shared state between chirp injector (in encoders) and detector (mic listener).
///
/// Uses a generation counter so multiple encoders can each independently detect
/// when a new chirp should be injected (both the track writer and datachannel
/// writer need to inject the same chirp).
pub struct ChirpState {
    pub chirp_waveform: Vec<f32>,
    /// Microseconds since `epoch` when the last chirp was armed.
    last_chirp_send: AtomicU64,
    /// Incremented by the timer each second. Encoders compare against their
    /// own local counter to know when to inject.
    generation: AtomicU64,
    epoch: Instant,
}

impl ChirpState {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            chirp_waveform: generate_chirp(sample_rate),
            last_chirp_send: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// Called by the timer every second to arm the next chirp.
    pub fn arm(&self) {
        self.last_chirp_send
            .store(self.now_micros(), Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Called by an encoder: returns true if a new chirp should be injected.
    /// `last_gen` is the encoder's local generation counter — updated in place.
    pub fn should_inject(&self, last_gen: &mut u64) -> bool {
        let current = self.generation.load(Ordering::Acquire);
        if current > *last_gen {
            *last_gen = current;
            true
        } else {
            false
        }
    }

    pub fn last_send_micros(&self) -> u64 {
        self.last_chirp_send.load(Ordering::Acquire)
    }

    pub fn now_micros(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }
}

/// Normalized cross-correlation peak detector.
/// Returns `Some(offset)` if the chirp is detected above `threshold`.
fn detect_chirp(samples: &[f32], chirp: &[f32], threshold: f32) -> Option<usize> {
    if samples.len() < chirp.len() {
        return None;
    }

    let chirp_energy: f32 = chirp.iter().map(|s| s * s).sum();
    if chirp_energy < 1e-6 {
        return None;
    }

    let mut best_corr: f32 = 0.0;
    let mut best_offset = 0;

    for offset in 0..=(samples.len() - chirp.len()) {
        let mut corr: f32 = 0.0;
        let mut sig_energy: f32 = 0.0;
        for i in 0..chirp.len() {
            corr += samples[offset + i] * chirp[i];
            sig_energy += samples[offset + i] * samples[offset + i];
        }
        // Normalized correlation: corr / sqrt(chirp_energy * sig_energy)
        let norm = (chirp_energy * sig_energy).sqrt();
        let normalized = if norm > 1e-6 { corr / norm } else { 0.0 };
        if normalized > best_corr {
            best_corr = normalized;
            best_offset = offset;
        }
    }

    if best_corr > threshold {
        Some(best_offset)
    } else {
        None
    }
}

/// Background task: arms the chirp injector every second.
pub async fn chirp_timer(state: Arc<ChirpState>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    // Skip the first tick (fires immediately)
    interval.tick().await;
    loop {
        interval.tick().await;
        state.arm();
    }
}

/// Background task: listens to raw mic audio and detects returning chirps.
pub async fn chirp_detector(
    state: Arc<ChirpState>,
    mut audio_rx: broadcast::Receiver<AudioChunk>,
) {
    let chirp = &state.chirp_waveform;
    // Normalized correlation threshold (0.0–1.0). 0.5 avoids false positives from noise.
    let threshold = 0.5_f32;
    let mut last_detection_micros: u64 = 0;

    info!(
        "Chirp detector started (chirp length: {} samples, threshold: {})",
        chirp.len(),
        threshold
    );

    loop {
        match audio_rx.recv().await {
            Ok(chunk) => {
                if chunk.samples.is_empty() {
                    continue;
                }

                if let Some(offset) = detect_chirp(&chunk.samples, chirp, threshold) {
                    let now = state.now_micros();
                    let last_send = state.last_send_micros();

                    // Cooldown: ignore detections within 500ms of the last one
                    if last_send > 0 && now.saturating_sub(last_detection_micros) > 500_000 {
                        // Compensate for the sample offset within this chunk
                        let offset_micros =
                            (offset as u64 * 1_000_000) / chunk.sample_rate as u64;
                        let latency_micros =
                            now.saturating_sub(last_send).saturating_sub(offset_micros);
                        let latency_ms = latency_micros as f64 / 1000.0;

                        info!("Round-trip latency: {:.1}ms", latency_ms);
                        eprintln!("  >> Round-trip latency: {:.1}ms", latency_ms);

                        last_detection_micros = now;
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Chirp detector lagged, dropped {} chunks", n);
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("Audio channel closed, stopping chirp detector");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chirp_length() {
        let chirp = generate_chirp(48000);
        // 2ms at 48kHz = 96 samples
        assert_eq!(chirp.len(), 96);
    }

    #[test]
    fn test_chirp_amplitude_range() {
        let chirp = generate_chirp(48000);
        for s in &chirp {
            assert!(*s >= -1.0 && *s <= 1.0, "Sample out of range: {}", s);
        }
    }

    #[test]
    fn test_detect_chirp_in_clean_signal() {
        let chirp = generate_chirp(48000);
        // Embed chirp at offset 200 in a silent buffer
        let mut signal = vec![0.0_f32; 500];
        for (i, &s) in chirp.iter().enumerate() {
            signal[200 + i] = s;
        }
        let result = detect_chirp(&signal, &chirp, 0.35);
        assert_eq!(result, Some(200));
    }

    #[test]
    fn test_detect_chirp_not_present() {
        let chirp = generate_chirp(48000);
        // Random-ish noise
        let signal: Vec<f32> = (0..500)
            .map(|i| (i as f32 * 0.1).sin() * 0.1)
            .collect();
        let result = detect_chirp(&signal, &chirp, 0.35);
        assert!(result.is_none());
    }

    #[test]
    fn test_chirp_state_arm_and_inject() {
        let state = ChirpState::new(48000);
        let mut gen = 0u64;
        assert!(!state.should_inject(&mut gen)); // not armed yet
        state.arm();
        assert!(state.should_inject(&mut gen)); // armed, should fire
        assert!(!state.should_inject(&mut gen)); // same generation, should not fire again

        // A second encoder with its own counter should also see it
        let mut gen2 = 0u64;
        assert!(state.should_inject(&mut gen2));
    }
}
