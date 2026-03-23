use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tracing::{error, info};
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::webrtc::PeerManager;

/// Shared application state accessible from all route handlers.
pub struct AppState {
    pub peer_manager: PeerManager,
    /// Most recently created peer connection (for ICE candidate trickle).
    /// In a production system you'd map session IDs to peers, but for LAN
    /// use with sequential connections this is sufficient.
    pub last_peer: Mutex<Option<Arc<RTCPeerConnection>>>,
}

#[derive(Deserialize)]
pub struct OfferRequest {
    pub sdp: String,
    #[serde(rename = "type")]
    pub sdp_type: String,
}

#[derive(Serialize)]
pub struct AnswerResponse {
    pub sdp: String,
    #[serde(rename = "type")]
    pub sdp_type: String,
}

#[derive(Deserialize)]
pub struct IceCandidateRequest {
    pub candidate: String,
    #[serde(rename = "sdpMid")]
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex")]
    pub sdp_mline_index: Option<u16>,
}

/// Build the axum Router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/offer", post(offer_handler))
        .route("/ice-candidate", post(ice_candidate_handler))
        .route("/status", get(status_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Serve the listener HTML page.
async fn index_handler() -> impl IntoResponse {
    Html(include_str!("../static/index.html"))
}

/// Handle SDP offer from browser, return SDP answer.
async fn offer_handler(
    State(state): State<Arc<AppState>>,
    Json(offer_req): Json<OfferRequest>,
) -> Result<Json<AnswerResponse>, (StatusCode, String)> {
    let offer = RTCSessionDescription::offer(offer_req.sdp).map_err(|e| {
        error!("Invalid SDP offer: {}", e);
        (StatusCode::BAD_REQUEST, format!("Invalid SDP offer: {}", e))
    })?;

    let (answer, peer) = state.peer_manager.handle_offer(offer).await.map_err(|e| {
        error!("Failed to handle offer: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create answer: {}", e),
        )
    })?;

    // Store the peer for ICE candidate trickle
    *state.last_peer.lock().await = Some(peer);

    Ok(Json(AnswerResponse {
        sdp: answer.sdp,
        sdp_type: "answer".to_string(),
    }))
}

/// Handle trickle ICE candidate from browser.
async fn ice_candidate_handler(
    State(state): State<Arc<AppState>>,
    Json(candidate_req): Json<IceCandidateRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let peer_lock = state.last_peer.lock().await;
    let peer = peer_lock.as_ref().ok_or((
        StatusCode::BAD_REQUEST,
        "No active peer connection".to_string(),
    ))?;

    let candidate = RTCIceCandidateInit {
        candidate: candidate_req.candidate,
        sdp_mid: candidate_req.sdp_mid,
        sdp_mline_index: candidate_req.sdp_mline_index,
        ..Default::default()
    };

    PeerManager::add_ice_candidate(peer, candidate)
        .await
        .map_err(|e| {
            error!("Failed to add ICE candidate: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to add ICE candidate: {}", e),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Simple status endpoint.
#[derive(Serialize, Deserialize)]
struct StatusResponse {
    status: String,
    active_peers: usize,
}

async fn status_handler(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    Json(StatusResponse {
        status: "running".to_string(),
        active_peers: state.peer_manager.peer_count().await,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app_state() -> Arc<AppState> {
        Arc::new(AppState {
            peer_manager: PeerManager::new().unwrap(),
            last_peer: Mutex::new(None),
        })
    }

    #[tokio::test]
    async fn test_index_returns_html() {
        let state = test_app_state();
        let app = build_router(state);

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Listen"));
    }

    #[tokio::test]
    async fn test_status_endpoint() {
        let state = test_app_state();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: StatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.status, "running");
        assert_eq!(status.active_peers, 0);
    }

    #[tokio::test]
    async fn test_offer_with_invalid_sdp() {
        let state = test_app_state();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/offer")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sdp": "invalid", "type": "offer"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return an error (either 400 or 500 depending on how webrtc-rs handles it)
        assert!(response.status().is_client_error() || response.status().is_server_error());
    }

    #[tokio::test]
    async fn test_ice_candidate_without_peer() {
        let state = test_app_state();
        let app = build_router(state);

        let response = app
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

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_offer_with_malformed_json() {
        let state = test_app_state();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/offer")
                    .header("content-type", "application/json")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_client_error());
    }
}
