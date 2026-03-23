use tokio::sync::broadcast;

// Re-export types from the main crate
// Note: These tests use the public API. The mock audio source and unit tests
// live inside src/audio.rs as #[cfg(test)] module tests.

#[test]
fn test_broadcast_channel_multiple_receivers() {
    let (tx, _rx1) = broadcast::channel::<Vec<f32>>(16);
    let mut rx2 = tx.subscribe();
    let mut rx3 = tx.subscribe();

    tx.send(vec![0.1, 0.2, 0.3]).unwrap();

    let data2 = rx2.try_recv().unwrap();
    let data3 = rx3.try_recv().unwrap();
    assert_eq!(data2, vec![0.1, 0.2, 0.3]);
    assert_eq!(data3, vec![0.1, 0.2, 0.3]);
}

#[test]
fn test_broadcast_channel_overflow_handling() {
    // Buffer size of 2
    let (tx, mut rx) = broadcast::channel::<i32>(2);

    // Send 3 items — first should be dropped
    tx.send(1).unwrap();
    tx.send(2).unwrap();
    tx.send(3).unwrap();

    // First recv should return a Lagged error
    match rx.try_recv() {
        Err(broadcast::error::TryRecvError::Lagged(n)) => {
            assert_eq!(n, 1);
        }
        other => panic!("Expected Lagged error, got {:?}", other),
    }

    // Subsequent reads should work
    assert_eq!(rx.try_recv().unwrap(), 2);
    assert_eq!(rx.try_recv().unwrap(), 3);
}

#[test]
fn test_f32_to_i16_conversion() {
    // Test the same conversion logic used in audio_to_track_writer
    let samples: Vec<f32> = vec![0.0, 1.0, -1.0, 0.5, -0.5];
    let pcm_bytes: Vec<u8> = samples
        .iter()
        .flat_map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            let i16_val = (clamped * i16::MAX as f32) as i16;
            i16_val.to_le_bytes()
        })
        .collect();

    // 5 samples * 2 bytes each = 10 bytes
    assert_eq!(pcm_bytes.len(), 10);

    // Check silence (0.0) maps to ~0
    let val = i16::from_le_bytes([pcm_bytes[0], pcm_bytes[1]]);
    assert_eq!(val, 0);

    // Check 1.0 maps to i16::MAX
    let val = i16::from_le_bytes([pcm_bytes[2], pcm_bytes[3]]);
    assert_eq!(val, i16::MAX);

    // Check -1.0 maps to near i16::MIN
    let val = i16::from_le_bytes([pcm_bytes[4], pcm_bytes[5]]);
    assert!(val < -32000);
}

#[test]
fn test_clamping_out_of_range_samples() {
    let samples: Vec<f32> = vec![2.0, -2.0, 1.5, -1.5];
    for &s in &samples {
        let clamped = s.clamp(-1.0, 1.0);
        assert!(clamped >= -1.0 && clamped <= 1.0);
    }
}
