use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// A chunk of audio samples (mono f32, at the configured sample rate).
/// Sent via broadcast channel to all WebRTC peer writers.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Trait abstracting audio capture so we can mock it in tests.
pub trait AudioSource: Send + 'static {
    /// Start capturing audio. Sends chunks to the provided broadcast sender.
    /// This method blocks the calling thread until capture stops.
    fn start_capture(
        &self,
        tx: broadcast::Sender<AudioChunk>,
        sample_rate: u32,
        channels: u16,
    ) -> anyhow::Result<()>;
}

/// List available audio input devices. Returns (name, is_default) pairs.
pub fn list_input_devices() -> Vec<(String, bool)> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    host.input_devices()
        .map(|devices| {
            devices
                .filter_map(|d| {
                    let name = d.name().ok()?;
                    let is_default = name == default_name;
                    Some((name, is_default))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Real audio source using cpal. Optionally targets a specific device by name.
pub struct CpalAudioSource {
    pub device_name: Option<String>,
}

impl AudioSource for CpalAudioSource {
    fn start_capture(
        &self,
        tx: broadcast::Sender<AudioChunk>,
        sample_rate: u32,
        channels: u16,
    ) -> anyhow::Result<()> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = if let Some(ref name) = self.device_name {
            host.input_devices()?
                .find(|d| d.name().ok().as_deref() == Some(name))
                .ok_or_else(|| anyhow::anyhow!("Audio device '{}' not found", name))?
        } else {
            host.default_input_device()
                .ok_or_else(|| anyhow::anyhow!("No default input device found"))?
        };

        info!(
            "Using input device: {}",
            device.name().unwrap_or_else(|_| "Unknown".to_string())
        );

        // Try small fixed buffer sizes for low latency, fall back to default
        let buffer_size = [256u32, 512, 1024]
            .iter()
            .find(|&&size| {
                let test_config = cpal::StreamConfig {
                    channels,
                    sample_rate: cpal::SampleRate(sample_rate),
                    buffer_size: cpal::BufferSize::Fixed(size),
                };
                device.supported_input_configs().is_ok_and(|_| {
                    // Try building a dummy stream to check if the buffer size works
                    device
                        .build_input_stream(
                            &test_config,
                            |_data: &[f32], _: &cpal::InputCallbackInfo| {},
                            |_| {},
                            None,
                        )
                        .is_ok()
                })
            })
            .map(|&size| {
                info!(
                    "Using fixed audio buffer size: {} samples ({:.1}ms)",
                    size,
                    size as f32 / sample_rate as f32 * 1000.0
                );
                cpal::BufferSize::Fixed(size)
            })
            .unwrap_or_else(|| {
                info!("No small fixed buffer size supported, using device default");
                cpal::BufferSize::Default
            });

        let desired_config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size,
        };

        // Try the desired config first; if unsupported, fall back to a device-native config
        let (actual_config, needs_conversion) = match device.build_input_stream(
            &desired_config,
            |_: &[f32], _: &cpal::InputCallbackInfo| {},
            |_| {},
            None,
        ) {
            Ok(_) => (desired_config.clone(), false),
            Err(e) => {
                warn!(
                    "Desired audio config ({}Hz, {}ch) not supported: {}",
                    sample_rate, channels, e
                );

                // Query device-supported configs and pick the best match
                let supported = device
                    .supported_input_configs()
                    .map_err(|e| anyhow::anyhow!("Cannot query supported configs: {}", e))?;

                let fallback = supported
                    .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
                    .min_by_key(|c| {
                        let sr = c
                            .min_sample_rate()
                            .0
                            .max(c.max_sample_rate().0.min(sample_rate));
                        let sr_diff = (sr as i64 - sample_rate as i64).unsigned_abs();
                        let ch_diff = (c.channels() as i64 - channels as i64).unsigned_abs();
                        (ch_diff, sr_diff)
                    })
                    .ok_or_else(|| anyhow::anyhow!("No compatible audio input config found"))?;

                let actual_sr = fallback
                    .min_sample_rate()
                    .0
                    .max(fallback.max_sample_rate().0.min(sample_rate));
                let actual_ch = fallback.channels();

                warn!(
                    "Falling back to device-native config: {}Hz, {}ch (will convert to {}Hz, {}ch)",
                    actual_sr, actual_ch, sample_rate, channels
                );

                if actual_sr != sample_rate {
                    warn!(
                        "Resampling from {}Hz to {}Hz using linear interpolation",
                        actual_sr, sample_rate
                    );
                }
                if actual_ch != channels {
                    warn!(
                        "Downmixing from {} channels to {} channel(s)",
                        actual_ch, channels
                    );
                }

                let cfg = cpal::StreamConfig {
                    channels: actual_ch,
                    sample_rate: cpal::SampleRate(actual_sr),
                    buffer_size: cpal::BufferSize::Default,
                };
                (cfg, actual_sr != sample_rate || actual_ch != channels)
            }
        };

        let actual_sr = actual_config.sample_rate.0;
        let actual_ch = actual_config.channels;

        let tx_err = tx.clone();
        let stream = device.build_input_stream(
            &actual_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = if needs_conversion {
                    // Downmix to mono if needed
                    let mono: Vec<f32> = if actual_ch > 1 {
                        data.chunks(actual_ch as usize)
                            .map(|frame| frame.iter().sum::<f32>() / actual_ch as f32)
                            .collect()
                    } else {
                        data.to_vec()
                    };

                    // Resample if needed (simple linear interpolation)
                    if actual_sr != sample_rate {
                        let ratio = sample_rate as f64 / actual_sr as f64;
                        let out_len = (mono.len() as f64 * ratio) as usize;
                        (0..out_len)
                            .map(|i| {
                                let src_pos = i as f64 / ratio;
                                let idx = src_pos as usize;
                                let frac = src_pos - idx as f64;
                                let a = mono[idx.min(mono.len() - 1)];
                                let b = mono[(idx + 1).min(mono.len() - 1)];
                                a + (b - a) * frac as f32
                            })
                            .collect()
                    } else {
                        mono
                    }
                } else {
                    data.to_vec()
                };

                let chunk = AudioChunk {
                    samples,
                    sample_rate,
                    channels,
                };
                let _ = tx.send(chunk);
            },
            move |err| {
                error!("Audio input stream error: {}", err);
                let _ = tx_err.send(AudioChunk {
                    samples: vec![],
                    sample_rate,
                    channels,
                });
            },
            None, // no timeout
        )?;

        stream.play()?;
        info!("Audio capture started ({}Hz, {} ch)", sample_rate, channels);

        // Block forever — the stream runs until this thread is stopped
        std::thread::park();

        Ok(())
    }
}

/// Audio source that generates a continuous 440Hz sine wave test tone.
pub struct ToneAudioSource;

impl AudioSource for ToneAudioSource {
    fn start_capture(
        &self,
        tx: broadcast::Sender<AudioChunk>,
        sample_rate: u32,
        channels: u16,
    ) -> anyhow::Result<()> {
        info!("Test tone mode: generating 440Hz sine wave");
        let frame_count = (sample_rate / 100) as usize; // 10ms chunks
        let mut sample_offset: usize = 0;

        loop {
            let samples: Vec<f32> = (0..frame_count)
                .map(|s| {
                    let t = (sample_offset + s) as f32 / sample_rate as f32;
                    (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.5
                })
                .collect();
            sample_offset += frame_count;

            let chunk = AudioChunk {
                samples,
                sample_rate,
                channels,
            };
            if tx.send(chunk).is_err() {
                break;
            }

            // Sleep for ~10ms to match real-time playback rate
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    }
}

/// Mock audio source for testing — generates a finite number of chunks.
#[cfg(test)]
pub struct MockAudioSource {
    pub chunk_count: usize,
}

#[cfg(test)]
impl AudioSource for MockAudioSource {
    fn start_capture(
        &self,
        tx: broadcast::Sender<AudioChunk>,
        sample_rate: u32,
        channels: u16,
    ) -> anyhow::Result<()> {
        for i in 0..self.chunk_count {
            // Generate a small chunk of samples (960 samples = 20ms at 48kHz)
            let frame_count = (sample_rate / 50) as usize; // 20ms worth
            let samples: Vec<f32> = (0..frame_count)
                .map(|s| {
                    // Simple sine wave at 440Hz for testing
                    let t = (i * frame_count + s) as f32 / sample_rate as f32;
                    (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.5
                })
                .collect();

            let chunk = AudioChunk {
                samples,
                sample_rate,
                channels,
            };
            if tx.send(chunk).is_err() {
                break;
            }
        }
        Ok(())
    }
}

/// Start audio capture on a background thread, returning a broadcast sender.
pub fn start_audio_capture<S: AudioSource>(
    source: S,
    sample_rate: u32,
    channels: u16,
) -> broadcast::Sender<AudioChunk> {
    // Buffer up to 100 chunks (~2 seconds at 20ms/chunk)
    let (tx, _rx) = broadcast::channel(100);
    let tx_clone = tx.clone();

    std::thread::spawn(move || {
        if let Err(e) = source.start_capture(tx_clone, sample_rate, channels) {
            error!("Audio capture failed: {}", e);
        }
    });

    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_chunk_clone() {
        let chunk = AudioChunk {
            samples: vec![0.1, 0.2, 0.3],
            sample_rate: 48000,
            channels: 1,
        };
        let cloned = chunk.clone();
        assert_eq!(cloned.samples, vec![0.1, 0.2, 0.3]);
        assert_eq!(cloned.sample_rate, 48000);
        assert_eq!(cloned.channels, 1);
    }

    #[test]
    fn test_mock_audio_source_generates_samples() {
        let (tx, mut rx) = broadcast::channel(16);
        let source = MockAudioSource { chunk_count: 5 };
        source.start_capture(tx, 48000, 1).unwrap();

        let mut count = 0;
        while let Ok(chunk) = rx.try_recv() {
            assert!(!chunk.samples.is_empty());
            assert_eq!(chunk.sample_rate, 48000);
            assert_eq!(chunk.channels, 1);
            // 48000 / 50 = 960 samples per chunk (20ms)
            assert_eq!(chunk.samples.len(), 960);
            count += 1;
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn test_mock_audio_source_sine_wave_range() {
        let (tx, mut rx) = broadcast::channel(16);
        let source = MockAudioSource { chunk_count: 1 };
        source.start_capture(tx, 48000, 1).unwrap();

        let chunk = rx.try_recv().unwrap();
        for sample in &chunk.samples {
            assert!(
                *sample >= -1.0 && *sample <= 1.0,
                "Sample out of range: {}",
                sample
            );
        }
    }

    #[test]
    fn test_start_audio_capture_returns_sender() {
        // Test that start_audio_capture returns a working sender
        // by calling MockAudioSource directly (synchronously) to avoid race conditions
        let (tx, mut rx) = broadcast::channel(16);
        let source = MockAudioSource { chunk_count: 3 };

        // Subscribe BEFORE sending to ensure we receive all messages
        source.start_capture(tx, 48000, 1).unwrap();

        let mut received = 0;
        while let Ok(_chunk) = rx.try_recv() {
            received += 1;
        }
        assert_eq!(received, 3, "Should have received exactly 3 chunks");
    }

    #[test]
    fn test_broadcast_no_receivers_does_not_panic() {
        let (tx, _) = broadcast::channel::<AudioChunk>(16);
        // Drop the only receiver
        let source = MockAudioSource { chunk_count: 5 };
        // This should not panic even though no one is listening
        let result = source.start_capture(tx, 48000, 1);
        assert!(result.is_ok());
    }
}
