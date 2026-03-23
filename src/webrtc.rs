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
pub async fn audio_to_track_writer(
    track: Arc<TrackLocalStaticSample>,
    mut audio_rx: broadcast::Receiver<AudioChunk>,
) {
    use webrtc::media::Sample;

    info!("Audio-to-track writer started");

    loop {
        match audio_rx.recv().await {
            Ok(chunk) => {
                if chunk.samples.is_empty() {
                    continue;
                }

                // Convert f32 samples to i16 PCM bytes (little-endian)
                let pcm_bytes: Vec<u8> = chunk
                    .samples
                    .iter()
                    .flat_map(|&s| {
                        let clamped = s.clamp(-1.0, 1.0);
                        let i16_val = (clamped * i16::MAX as f32) as i16;
                        i16_val.to_le_bytes()
                    })
                    .collect();

                // Calculate duration of this chunk
                let duration_ms = (chunk.samples.len() as u64 * 1000) / chunk.sample_rate as u64;

                let sample = Sample {
                    data: pcm_bytes.into(),
                    duration: std::time::Duration::from_millis(duration_ms),
                    ..Default::default()
                };

                if let Err(e) = track.write_sample(&sample).await {
                    warn!("Failed to write audio sample to track: {}", e);
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
