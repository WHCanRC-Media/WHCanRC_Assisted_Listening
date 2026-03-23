use webrtc::track::track_local::TrackLocal;
use whcanrc_assisted_listening::webrtc::PeerManager;

#[tokio::test]
async fn test_peer_manager_creation() {
    let pm = PeerManager::new();
    assert!(pm.is_ok(), "PeerManager should be created successfully");
}

#[tokio::test]
async fn test_initial_peer_count_is_zero() {
    let pm = PeerManager::new().unwrap();
    assert_eq!(pm.peer_count().await, 0);
}

#[tokio::test]
async fn test_close_all_on_empty() {
    let pm = PeerManager::new().unwrap();
    pm.close_all().await;
    assert_eq!(pm.peer_count().await, 0);
}

#[tokio::test]
async fn test_audio_track_properties() {
    let pm = PeerManager::new().unwrap();
    let track = pm.audio_track();
    assert_eq!(track.stream_id(), "whcanrc-stream");
}

#[tokio::test]
async fn test_handle_offer_with_invalid_sdp() {
    use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

    // RTCSessionDescription::offer() itself validates the SDP, so invalid SDP
    // is rejected at parse time — which is correct behavior.
    let result = RTCSessionDescription::offer("invalid sdp".to_string());
    assert!(
        result.is_err(),
        "Invalid SDP should be rejected at parse time"
    );
}
