use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tower::ServiceExt;

use whcanrc_assisted_listening::server::{build_router, AppState};
use whcanrc_assisted_listening::webrtc::PeerManager;

fn test_state() -> Arc<AppState> {
    Arc::new(AppState {
        peer_manager: PeerManager::new().unwrap(),
        last_peer: Mutex::new(None),
        webtransport_port: 8081,
        webtransport_state: Arc::new(RwLock::new(None)),
    })
}

#[tokio::test]
async fn test_index_serves_html() {
    let app = build_router(test_state());
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("WHCanRC"));
}

#[tokio::test]
async fn test_status_endpoint_returns_json() {
    let app = build_router(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "running");
    assert_eq!(json["active_peers"], 0);
}

#[tokio::test]
async fn test_offer_with_malformed_json_returns_error() {
    let app = build_router(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/offer")
                .header("content-type", "application/json")
                .body(Body::from("not valid json"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(resp.status().is_client_error());
}

#[tokio::test]
async fn test_offer_with_empty_sdp() {
    let app = build_router(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/offer")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sdp": "", "type": "offer"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // Empty SDP should fail
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
}

#[tokio::test]
async fn test_ice_candidate_without_peer_returns_bad_request() {
    let app = build_router(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ice-candidate")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"candidate": "candidate:1 1 UDP 2122252543 192.168.1.1 12345 typ host", "sdpMid": "0", "sdpMLineIndex": 0}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_nonexistent_route_returns_404() {
    let app = build_router(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
