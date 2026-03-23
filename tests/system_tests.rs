use std::sync::Arc;
use tokio::sync::Mutex;

use whcanrc_assisted_listening::server::{build_router, AppState};
use whcanrc_assisted_listening::webrtc::PeerManager;

/// System test: start the server on a random port and verify it responds.
#[tokio::test]
async fn test_server_starts_and_responds() {
    let state = Arc::new(AppState {
        peer_manager: PeerManager::new().unwrap(),
        last_peer: Mutex::new(None),
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
