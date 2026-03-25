use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use crate::audio::AudioChunk;

/// Manages all active WebRTC peer connections and the shared audio track.
pub struct PeerManager {
    peers: Arc<Mutex<Vec<Arc<RTCPeerConnection>>>>,
    audio_track: Arc<TrackLocalStaticSample>,
    api: webrtc::api::API,
}

impl PeerManager {
    /// Create a new PeerManager.
    pub fn new() -> anyhow::Result<Self> {
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)?;

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        let audio_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48000,
                channels: 1,
                ..Default::default()
            },
            "audio".to_string(),
            "whcanrc-stream".to_string(),
        ));

        Ok(Self {
            peers: Arc::new(Mutex::new(Vec::new())),
            audio_track,
            api,
        })
    }

    /// Get a reference to the audio track for writing samples.
    pub fn audio_track(&self) -> &Arc<TrackLocalStaticSample> {
        &self.audio_track
    }

    /// Handle an incoming SDP offer: create a peer connection, add the audio track,
    /// set the remote description, create and return an answer.
    pub async fn handle_offer(
        &self,
        offer: RTCSessionDescription,
    ) -> anyhow::Result<(RTCSessionDescription, Arc<RTCPeerConnection>)> {
        // No STUN/TURN needed for LAN
        let config = RTCConfiguration::default();

        let peer_connection = Arc::new(self.api.new_peer_connection(config).await?);

        // Add the audio track to this peer
        let rtp_sender = peer_connection
            .add_track(Arc::clone(&self.audio_track) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        // Spawn a task to read and discard incoming RTCP packets
        tokio::spawn(async move {
            let mut buf = vec![0u8; 1500];
            while rtp_sender.read(&mut buf).await.is_ok() {}
        });

        // Track connection state for cleanup
        let peers_ref = Arc::clone(&self.peers);
        let pc_weak = Arc::downgrade(&peer_connection);
        peer_connection.on_ice_connection_state_change(Box::new(move |state| {
            info!("ICE connection state changed: {}", state);
            if state == RTCIceConnectionState::Disconnected
                || state == RTCIceConnectionState::Failed
                || state == RTCIceConnectionState::Closed
            {
                let peers_ref = peers_ref.clone();
                let pc_weak = pc_weak.clone();
                tokio::spawn(async move {
                    if let Some(pc) = pc_weak.upgrade() {
                        let mut peers: tokio::sync::MutexGuard<'_, Vec<Arc<RTCPeerConnection>>> =
                            peers_ref.lock().await;
                        peers.retain(|p| !Arc::ptr_eq(p, &pc));
                        info!("Removed disconnected peer. Active peers: {}", peers.len());
                    }
                });
            }
            Box::pin(async {})
        }));

        // Set the remote SDP offer
        peer_connection.set_remote_description(offer).await?;

        // Create an answer
        let answer = peer_connection.create_answer(None).await?;

        // Set the local description (starts ICE gathering)
        peer_connection
            .set_local_description(answer.clone())
            .await?;

        // Wait for ICE gathering to complete
        let mut gather_complete = peer_connection.gathering_complete_promise().await;

        // Wait with a timeout for ICE gathering
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), gather_complete.recv()).await;

        // Get the local description with ICE candidates included
        let local_desc = peer_connection
            .local_description()
            .await
            .ok_or_else(|| anyhow::anyhow!("Failed to get local description"))?;

        // Store the peer
        {
            let mut peers = self.peers.lock().await;
            peers.push(Arc::clone(&peer_connection));
            info!("New peer connected. Active peers: {}", peers.len());
        }

        Ok((local_desc, peer_connection))
    }

    /// Add an ICE candidate to a peer connection.
    pub async fn add_ice_candidate(
        peer: &RTCPeerConnection,
        candidate: RTCIceCandidateInit,
    ) -> anyhow::Result<()> {
        peer.add_ice_candidate(candidate).await?;
        Ok(())
    }

    /// Get the number of currently connected peers.
    pub async fn peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }

    /// Close all peer connections and clean up.
    #[allow(dead_code)]
    pub async fn close_all(&self) {
        let mut peers = self.peers.lock().await;
        for peer in peers.drain(..) {
            if let Err(e) = peer.close().await {
                warn!("Error closing peer connection: {}", e);
            }
        }
        info!("All peer connections closed");
    }
}

/// Encode raw PCM f32 samples to Opus and write to the WebRTC track.
/// This task runs continuously, reading from the audio broadcast channel.
///
/// `opus_frame_ms` controls the Opus frame duration (valid: 5, 10, 20, 40, 60).
/// Lower values reduce latency but increase per-packet overhead.
pub async fn audio_to_track_writer(
    track: Arc<TrackLocalStaticSample>,
    mut audio_rx: broadcast::Receiver<AudioChunk>,
    opus_frame_ms: u64,
    chirp_state: Option<Arc<crate::latency_test::ChirpState>>,
) {
    use audiopus::coder::Encoder;
    use audiopus::{Application, Channels, SampleRate, Signal, Bitrate};
    use webrtc::media::Sample;

    // Calculate frame size in samples: e.g. 10ms at 48kHz = 480 samples
    let opus_frame_size = (48000 * opus_frame_ms as usize) / 1000;
    let opus_frame_duration = std::time::Duration::from_millis(opus_frame_ms);

    info!(
        "Audio-to-track writer started ({}ms Opus frames, {} samples/frame)",
        opus_frame_ms, opus_frame_size
    );

    let mut encoder = match Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::LowDelay) {
        Ok(enc) => enc,
        Err(e) => {
            error!("Failed to create Opus encoder: {}", e);
            return;
        }
    };
    // Tune encoder for minimum latency
    let _ = encoder.set_bitrate(Bitrate::BitsPerSecond(32000));
    let _ = encoder.set_complexity(5);
    let _ = encoder.set_signal(Signal::Voice);

    // Buffer to accumulate samples into complete Opus frames
    let mut pcm_buffer: Vec<i16> = Vec::with_capacity(opus_frame_size * 2);
    let mut opus_output = vec![0u8; 4000]; // max Opus packet size
    let mut chirp_gen = 0u64;

    // Send timing instrumentation (similar to DataChannel path)
    let start_time = std::time::Instant::now();
    let mut last_send = start_time;
    let mut send_count: u64 = 0;
    let mut delta_sum_us: u64 = 0;
    let mut delta_sq_sum: f64 = 0.0;
    let mut min_delta_us: u64 = u64::MAX;
    let mut max_delta_us: u64 = 0;

    loop {
        match audio_rx.recv().await {
            Ok(chunk) => {
                if chunk.samples.is_empty() {
                    continue;
                }

                if let Some(ref cs) = chirp_state {
                    // Latency test mode: send silence (same length as mic data)
                    // plus chirp when armed — avoids feedback loop
                    pcm_buffer.extend(std::iter::repeat_n(0i16, chunk.samples.len()));
                    if cs.should_inject(&mut chirp_gen) {
                        let chirp_i16: Vec<i16> = cs.chirp_waveform.iter()
                            .map(|&s| (s * i16::MAX as f32) as i16)
                            .collect();
                        // Overwrite the tail of the buffer with the chirp
                        let start = pcm_buffer.len().saturating_sub(chirp_i16.len());
                        pcm_buffer[start..].copy_from_slice(&chirp_i16);
                    }
                } else {
                    // Normal mode: forward mic audio
                    for &s in &chunk.samples {
                        let clamped = s.clamp(-1.0, 1.0);
                        pcm_buffer.push((clamped * i16::MAX as f32) as i16);
                    }
                }

                // Encode complete frames
                while pcm_buffer.len() >= opus_frame_size {
                    let frame: Vec<i16> = pcm_buffer.drain(..opus_frame_size).collect();

                    match encoder.encode(&frame, &mut opus_output) {
                        Ok(len) => {
                            let sample = Sample {
                                data: opus_output[..len].to_vec().into(),
                                duration: opus_frame_duration,
                                ..Default::default()
                            };

                            if let Err(e) = track.write_sample(&sample).await {
                                warn!("Failed to write audio sample to track: {}", e);
                            } else {
                                // Track send timing for RTP path
                                let now = std::time::Instant::now();
                                let delta_us = now.duration_since(last_send).as_micros() as u64;
                                last_send = now;

                                if send_count > 0 {
                                    // Skip first sample (no valid delta)
                                    delta_sum_us += delta_us;
                                    delta_sq_sum += (delta_us as f64).powi(2);
                                    min_delta_us = min_delta_us.min(delta_us);
                                    max_delta_us = max_delta_us.max(delta_us);
                                }
                                send_count += 1;

                                // Log stats every 500 packets (~5 seconds at 10ms frames)
                                if send_count > 1 && send_count.is_multiple_of(500) {
                                    let n = send_count - 1; // exclude first sample
                                    let mean_us = delta_sum_us as f64 / n as f64;
                                    let variance = (delta_sq_sum / n as f64) - (mean_us * mean_us);
                                    let stddev_ms = (variance.max(0.0).sqrt()) / 1000.0;
                                    let mean_ms = mean_us / 1000.0;
                                    let min_ms = min_delta_us as f64 / 1000.0;
                                    let max_ms = max_delta_us as f64 / 1000.0;

                                    info!(
                                        "[RTP] Send interval: mean={:.2}ms stddev={:.3}ms min={:.2}ms max={:.2}ms (n={})",
                                        mean_ms, stddev_ms, min_ms, max_ms, n
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Opus encode error: {}", e);
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Audio receiver lagged, dropped {} chunks", n);
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("Audio channel closed, stopping track writer");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_peer_manager_creation() {
        let pm = PeerManager::new();
        assert!(pm.is_ok());
    }

    #[tokio::test]
    async fn test_peer_count_starts_at_zero() {
        let pm = PeerManager::new().unwrap();
        assert_eq!(pm.peer_count().await, 0);
    }

    #[tokio::test]
    async fn test_close_all_with_no_peers() {
        let pm = PeerManager::new().unwrap();
        pm.close_all().await;
        assert_eq!(pm.peer_count().await, 0);
    }

    #[tokio::test]
    async fn test_audio_track_exists() {
        let pm = PeerManager::new().unwrap();
        let track = pm.audio_track();
        assert_eq!(track.stream_id(), "whcanrc-stream");
    }
}
