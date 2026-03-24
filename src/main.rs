mod audio;
mod config;
mod latency_test;
mod server;
mod service;
mod webrtc;

use std::net::SocketAddr;
use std::sync::Arc;

use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use tokio::sync::Mutex;
use tracing::info;

use audio::{start_audio_capture, CpalAudioSource, ToneAudioSource};
use config::Config;
use server::{build_router, AppState};
use webrtc::{audio_to_track_writer, PeerManager};

/// WHCanRC Assisted Listening — low-latency WebRTC audio streaming server.
///
/// Captures audio from the system's default input device and streams it
/// to browsers over WebRTC on the local network.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Stream a 440Hz test tone instead of capturing from the audio input device
    #[arg(long)]
    test_tone: bool,

    /// Enable round-trip latency measurement: injects a 2ms chirp every second
    /// and listens for it to return through the mic
    #[arg(long)]
    latency_test: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let cli = Cli::parse();

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

    // Start audio capture (real mic or test tone)
    let audio_tx = if cli.test_tone {
        info!("Using test tone (440Hz sine wave) instead of audio input");
        start_audio_capture(
            ToneAudioSource,
            config.audio_sample_rate,
            config.audio_channels,
        )
    } else {
        start_audio_capture(
            CpalAudioSource,
            config.audio_sample_rate,
            config.audio_channels,
        )
    };

    // Set up latency test if requested
    let chirp_state = if cli.latency_test {
        let state = std::sync::Arc::new(latency_test::ChirpState::new(config.audio_sample_rate));
        info!("Latency test enabled — injecting chirp every second");

        // Spawn the 1-second chirp timer
        let timer_state = Arc::clone(&state);
        tokio::spawn(latency_test::chirp_timer(timer_state));

        // Spawn the chirp detector on raw mic audio
        let detector_state = Arc::clone(&state);
        let detector_rx = audio_tx.subscribe();
        tokio::spawn(latency_test::chirp_detector(detector_state, detector_rx));

        Some(state)
    } else {
        None
    };

    // Start the audio-to-WebRTC-track writer
    let audio_rx = audio_tx.subscribe();
    tokio::spawn(audio_to_track_writer(
        audio_track,
        audio_rx,
        config.opus_frame_ms,
        chirp_state,
    ));

    // Build application state and HTTP server
    let app_state = Arc::new(AppState {
        peer_manager,
        last_peer: Mutex::new(None),
    });

    let app = build_router(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));

    // Generate self-signed TLS cert for HTTPS (needed for getUserMedia on LAN)
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "0.0.0.0".to_string()])?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();

    let tls_config = RustlsConfig::from_pem(cert_pem.into(), key_pem.into()).await?;

    // Detect LAN IP for the QR code
    let lan_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "localhost".to_string());

    let url = format!("https://{}:{}", lan_ip, config.port);
    info!("Listening on {}", url);
    info!("Note: self-signed cert — accept the browser warning to connect");

    // Print QR code to terminal
    if let Ok(qr) = qrcode::QrCode::new(url.as_bytes()) {
        let rendered = qr
            .render::<char>()
            .quiet_zone(true)
            .module_dimensions(2, 1)
            .build();
        eprintln!("\n{}\n  {}\n", rendered, url);
    }

    axum_server::bind_rustls(addr, tls_config)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}
