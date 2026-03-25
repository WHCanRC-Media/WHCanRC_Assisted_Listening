use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use whcanrc_assisted_listening::audio::{start_audio_capture, ToneAudioSource};
use whcanrc_assisted_listening::server::{build_router, AppState};
use whcanrc_assisted_listening::webrtc::{audio_to_track_writer, PeerManager};

/// System test: start the server on a random port and verify it responds.
#[tokio::test]
async fn test_server_starts_and_responds() {
    let state = Arc::new(AppState {
        peer_manager: PeerManager::new().unwrap(),
        last_peer: Mutex::new(None),
        webtransport_port: 8081,
        webtransport_state: Arc::new(RwLock::new(None)),
    });

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Spawn the server
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Test that the HTML page is served
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{}/", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("WHCanRC"));

    // Test status endpoint
    let resp = client
        .get(format!("http://{}/status", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["status"], "running");
}

/// System test: verify the offer endpoint rejects bad SDPs gracefully.
#[tokio::test]
async fn test_server_handles_bad_offer_gracefully() {
    let state = Arc::new(AppState {
        peer_manager: PeerManager::new().unwrap(),
        last_peer: Mutex::new(None),
        webtransport_port: 8081,
        webtransport_state: Arc::new(RwLock::new(None)),
    });

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/offer", addr))
        .json(&serde_json::json!({
            "sdp": "this is not valid sdp",
            "type": "offer"
        }))
        .send()
        .await
        .unwrap();

    // Should fail but not crash the server
    assert!(resp.status().is_client_error() || resp.status().is_server_error());

    // Server should still be running — check status
    let resp = client
        .get(format!("http://{}/status", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

/// End-to-end test: start server with test tone, connect a WebRTC peer,
/// verify that Opus-encoded audio frames are actually received.
#[tokio::test]
async fn test_end_to_end_audio_delivery() {
    use webrtc::api::interceptor_registry::register_default_interceptors;
    use webrtc::api::media_engine::MediaEngine;
    use webrtc::api::APIBuilder;
    use webrtc::interceptor::registry::Registry;
    use webrtc::peer_connection::configuration::RTCConfiguration;
    use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

    // Ensure rustls crypto provider is installed (needed when axum-server pulls in rustls)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Start the full server pipeline with a test tone
    let peer_manager = PeerManager::new().unwrap();
    let audio_track = Arc::clone(peer_manager.audio_track());

    let audio_tx = start_audio_capture(ToneAudioSource, 48000, 1);
    let audio_rx = audio_tx.subscribe();
    tokio::spawn(audio_to_track_writer(audio_track, audio_rx, 10, None));

    let state = Arc::new(AppState {
        peer_manager,
        last_peer: Mutex::new(None),
        webtransport_port: 8081,
        webtransport_state: Arc::new(RwLock::new(None)),
    });

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // 2. Create a client-side WebRTC peer (simulating the browser)
    let mut client_media = MediaEngine::default();
    client_media.register_default_codecs().unwrap();
    let mut client_registry = Registry::new();
    client_registry = register_default_interceptors(client_registry, &mut client_media).unwrap();
    let client_api = APIBuilder::new()
        .with_media_engine(client_media)
        .with_interceptor_registry(client_registry)
        .build();

    let client_pc = Arc::new(
        client_api
            .new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap(),
    );

    // Add recvonly audio transceiver
    client_pc
        .add_transceiver_from_kind(
            webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
            Some(webrtc::rtp_transceiver::RTCRtpTransceiverInit {
                direction: webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Recvonly,
                send_encodings: vec![],
            }),
        )
        .await
        .unwrap();

    // Track received audio packets
    let packets_received = Arc::new(AtomicUsize::new(0));
    let packets_clone = Arc::clone(&packets_received);

    client_pc.on_track(Box::new(move |track, _, _| {
        let packets_clone = Arc::clone(&packets_clone);
        Box::pin(async move {
            let mut buf = vec![0u8; 1500];
            loop {
                match track.read(&mut buf).await {
                    Ok(_) => {
                        packets_clone.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => break,
                }
            }
        })
    }));

    // 3. Create offer, send to server, get answer
    let offer = client_pc.create_offer(None).await.unwrap();
    client_pc
        .set_local_description(offer.clone())
        .await
        .unwrap();

    // Wait for ICE gathering
    let mut gather_complete = client_pc.gathering_complete_promise().await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), gather_complete.recv()).await;

    let local_desc = client_pc.local_description().await.unwrap();

    let http_client = reqwest::Client::new();
    let resp = http_client
        .post(format!("http://{}/offer", addr))
        .json(&serde_json::json!({
            "sdp": local_desc.sdp,
            "type": "offer"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "Offer should succeed");

    let answer: serde_json::Value = resp.json().await.unwrap();
    let answer_sdp = answer["sdp"].as_str().unwrap();
    let remote_desc = RTCSessionDescription::answer(answer_sdp.to_string()).unwrap();
    client_pc.set_remote_description(remote_desc).await.unwrap();

    // 4. Wait for audio to flow
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // 5. Verify audio was received
    let count = packets_received.load(Ordering::Relaxed);
    assert!(
        count > 10,
        "Expected at least 10 audio packets in 2 seconds, got {}",
        count
    );

    client_pc.close().await.unwrap();
}
