use tokio::sync::broadcast;
use tracing::{error, info};

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

/// Real audio source using cpal.
pub struct CpalAudioSource;

impl AudioSource for CpalAudioSource {
    fn start_capture(
        &self,
        tx: broadcast::Sender<AudioChunk>,
        sample_rate: u32,
        channels: u16,
    ) -> anyhow::Result<()> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No default input device found"))?;

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
                device.supported_input_configs().map_or(false, |_| {
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
                info!("Using fixed audio buffer size: {} samples ({:.1}ms)", size, size as f32 / sample_rate as f32 * 1000.0);
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

        let tx_err = tx.clone();
        let stream = device.build_input_stream(
            &desired_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let chunk = AudioChunk {
                    samples: data.to_vec(),
                    sample_rate,
                    channels,
                };
                // If no receivers, that's ok — just drop the data
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

/// Start audio capture on a background thread, returning a broadcast receiver.
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
        let source = MockAudioSource { chunk_count: 3 };
        let tx = start_audio_capture(source, 48000, 1);

        // We should be able to subscribe
        let mut rx = tx.subscribe();

        // Give the thread a moment to produce data
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut received = 0;
        while let Ok(_chunk) = rx.try_recv() {
            received += 1;
        }
        assert!(received > 0, "Should have received at least one chunk");
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
