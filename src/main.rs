mod audio;
mod config;
mod server;
mod service;
mod webrtc;

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use audio::{start_audio_capture, CpalAudioSource};
use config::Config;
use server::{build_router, AppState};
use webrtc::{audio_to_track_writer, PeerManager};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = Config::load()?;

    // Initialize logging
    let env_filter = tracing_subscriber::EnvFilter::try_new(&config.log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    info!("WHCanRC Assisted Listening starting up");
    info!(
        "Configuration: port={}, sample_rate={}, channels={}",
        config.port, config.audio_sample_rate, config.audio_channels
    );

    // Initialize WebRTC peer manager
    let peer_manager = PeerManager::new()?;
    let audio_track = Arc::clone(peer_manager.audio_track());

    // Start audio capture
    let audio_tx = start_audio_capture(
        CpalAudioSource,
        config.audio_sample_rate,
        config.audio_channels,
    );

    // Start the audio-to-WebRTC-track writer
    let audio_rx = audio_tx.subscribe();
    tokio::spawn(audio_to_track_writer(audio_track, audio_rx));

    // Build application state and HTTP server
    let app_state = Arc::new(AppState {
        peer_manager,
        last_peer: Mutex::new(None),
    });

    let app = build_router(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
