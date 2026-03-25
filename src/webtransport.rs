//! WebTransport server for low-latency audio streaming.
//!
//! WebTransport uses QUIC/HTTP3 and can achieve lower latency than WebRTC
//! by avoiding the browser's jitter buffer.
//!
//! Sends raw PCM i16 samples (no codec) for simplicity and lowest latency.

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};
use wtransport::endpoint::IncomingSession;
use wtransport::Identity;
use wtransport::{Endpoint, ServerConfig};

use crate::audio::AudioChunk;

/// Shared state containing the certificate hash for browser connections.
pub struct WebTransportState {
    pub cert_hash: String,
}

/// Configuration for the WebTransport server.
pub struct WebTransportServer {
    port: u16,
    state: Arc<RwLock<Option<WebTransportState>>>,
}

impl WebTransportServer {
    pub fn new(port: u16, state: Arc<RwLock<Option<WebTransportState>>>) -> Self {
        Self { port, state }
    }

    /// Start the WebTransport server and stream audio to connected clients.
    pub async fn run(self, audio_tx: broadcast::Sender<AudioChunk>) -> anyhow::Result<()> {
        // Generate self-signed identity for WebTransport
        let identity = Identity::self_signed(["localhost", "127.0.0.1", "0.0.0.0"])
            .map_err(|e| anyhow::anyhow!("Failed to create self-signed identity: {:?}", e))?;

        // Get certificate hash for browser
        let cert_chain = identity.certificate_chain();
        let cert = cert_chain
            .as_slice()
            .first()
            .ok_or_else(|| anyhow::anyhow!("No certificate in chain"))?;

        // Get the SHA-256 hash (wtransport provides this method)
        let hash_digest = cert.hash();
        let hash_bytes: &[u8; 32] = hash_digest.as_ref();
        let cert_hash =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, hash_bytes);

        info!("WebTransport certificate hash: {}", cert_hash);

        // Store the hash for the HTTP endpoint
        {
            let mut state = self.state.write().await;
            *state = Some(WebTransportState { cert_hash });
        }

        let config = ServerConfig::builder()
            .with_bind_default(self.port)
            .with_identity(identity)
            .build();

        let server = Endpoint::server(config)?;

        info!("WebTransport server listening on port {}", self.port);

        loop {
            let incoming = server.accept().await;
            let audio_rx = audio_tx.subscribe();

            tokio::spawn(handle_session(incoming, audio_rx));
        }
    }
}

/// Handle a single WebTransport session.
async fn handle_session(incoming: IncomingSession, audio_rx: broadcast::Receiver<AudioChunk>) {
    let result = handle_session_inner(incoming, audio_rx).await;
    if let Err(e) = result {
        warn!("WebTransport session error: {}", e);
    }
}

async fn handle_session_inner(
    incoming: IncomingSession,
    audio_rx: broadcast::Receiver<AudioChunk>,
) -> anyhow::Result<()> {
    let session_request = incoming.await?;

    info!(
        "WebTransport connection from: {:?}, path: {}",
        session_request.authority(),
        session_request.path()
    );

    let connection = session_request.accept().await?;

    info!("WebTransport session established");

    // Stream audio via datagrams (unreliable, low latency)
    stream_audio_datagrams(connection, audio_rx).await
}

/// Stream raw PCM i16 audio as WebTransport datagrams.
/// No codec = lowest latency, ~96KB/s for mono 48kHz.
async fn stream_audio_datagrams(
    connection: wtransport::Connection,
    mut audio_rx: broadcast::Receiver<AudioChunk>,
) -> anyhow::Result<()> {
    let mut send_count: u64 = 0;

    loop {
        match audio_rx.recv().await {
            Ok(chunk) => {
                if chunk.samples.is_empty() {
                    continue;
                }

                // Convert f32 to i16 and send as raw PCM
                let pcm_bytes: Vec<u8> = chunk
                    .samples
                    .iter()
                    .flat_map(|&s| {
                        let clamped = s.clamp(-1.0, 1.0);
                        let sample = (clamped * i16::MAX as f32) as i16;
                        sample.to_le_bytes()
                    })
                    .collect();

                // Send as datagram (unreliable, minimum latency)
                if let Err(e) = connection.send_datagram(&pcm_bytes) {
                    info!("WebTransport connection closed: {}", e);
                    return Ok(());
                }

                send_count += 1;
                if send_count.is_multiple_of(500) {
                    info!(
                        "[WebTransport] Sent {} datagrams ({} bytes each)",
                        send_count,
                        pcm_bytes.len()
                    );
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("WebTransport audio receiver lagged, dropped {} chunks", n);
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("Audio channel closed, stopping WebTransport stream");
                break;
            }
        }
    }

    Ok(())
}
