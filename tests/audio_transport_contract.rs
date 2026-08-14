use reai_board_sdk::kernel::audio::{
    resolve_audio_transport, AudioCapabilities, AudioRouteRequest, AudioTransport,
    SequenceDisposition, SequenceTracker,
};
use reai_board_sdk::kernel::protocol_gatt::parse_audio_packet_v1;
use reai_board_sdk::kernel::protocol_hid::{
    parse_audio_capabilities_hid_response, parse_audio_stream_hid_response, parse_usb_audio_report,
    HidPacket, AUDIO_ENVELOPE_VERSION, AUDIO_FLAG_DATA, REPORT_ID_AUDIO,
};

#[test]
fn parses_usb_vendor_hid_v1_envelope_without_system_audio() {
    let mut report = [0u8; 64];
    report[0] = REPORT_ID_AUDIO;
    report[1] = AUDIO_ENVELOPE_VERSION;
    report[2] = AUDIO_FLAG_DATA;
    report[3..5].copy_from_slice(&0xFFFEu16.to_le_bytes());
    report[5] = 57;
    report[6] = 0xAD;
    let packet = parse_usb_audio_report(&report).expect("valid v1 report");
    assert_eq!(packet.transport, AudioTransport::UsbVendorHid);
    assert_eq!(packet.sequence, Some(0xFFFE));
    assert_eq!(packet.payload.len(), 57);
}

#[test]
fn parses_batched_ble_v1_packet_and_rejects_partial_msbc() {
    let mut packet = vec![AUDIO_ENVELOPE_VERSION, AUDIO_FLAG_DATA, 7, 0, 114];
    packet.extend_from_slice(&[0xAD; 114]);
    let parsed = parse_audio_packet_v1(&packet).expect("two complete frames");
    assert_eq!(parsed.transport, AudioTransport::BleGatt);
    assert_eq!(parsed.sequence, Some(7));
    assert_eq!(parsed.payload.len(), 114);

    packet[4] = 113;
    packet.pop();
    assert!(parse_audio_packet_v1(&packet).is_none());

    let mut trailing = vec![AUDIO_ENVELOPE_VERSION, AUDIO_FLAG_DATA, 8, 0, 57];
    trailing.extend_from_slice(&[0xAD; 58]);
    assert!(parse_audio_packet_v1(&trailing).is_none());
}

#[test]
fn sequence_tracker_distinguishes_wrap_gap_and_duplicate() {
    let mut tracker = SequenceTracker::default();
    assert_eq!(tracker.observe(0xFFFE), SequenceDisposition::First);
    assert_eq!(tracker.observe(0xFFFF), SequenceDisposition::Contiguous);
    assert_eq!(tracker.observe(0), SequenceDisposition::Contiguous);
    assert_eq!(tracker.observe(2), SequenceDisposition::Gap { missing: 1 });
    assert_eq!(tracker.observe(2), SequenceDisposition::Duplicate);
}

#[test]
fn board_first_never_silently_falls_back_to_uac() {
    let caps = AudioCapabilities {
        usb_vendor_hid_msbc_v1: false,
        ble_gatt_msbc_v1: true,
        stream_control_v1: true,
        packet_sequence_v1: true,
        ..AudioCapabilities::default()
    };
    assert_eq!(
        resolve_audio_transport(
            AudioRouteRequest::BoardFirst,
            Some(reai_board_sdk::ConnectionType::Usb),
            &caps,
        ),
        None
    );
    assert_eq!(
        resolve_audio_transport(
            AudioRouteRequest::ExplicitUsbUac,
            Some(reai_board_sdk::ConnectionType::Usb),
            &caps,
        ),
        Some(AudioTransport::UsbUac)
    );
}

#[test]
fn parses_capability_response_as_feature_bits_not_latest_version() {
    let response = [
        0x0A,
        0x6E,
        13,
        0,
        1,
        0b0000_1111,
        0,
        0,
        0,
        1,
        57,
        171,
        0x88,
        0x13,
        0x88,
        0x13,
    ];
    let caps = parse_audio_capabilities_hid_response(&response).expect("valid capabilities");
    assert!(caps.usb_vendor_hid_msbc_v1);
    assert!(caps.ble_gatt_msbc_v1);
    assert!(caps.stream_control_v1);
    assert_eq!(caps.usb_max_payload, 57);
    assert_eq!(caps.ble_max_payload, 171);
}

#[test]
fn stream_control_request_and_ack_freeze_exact_scope_and_lease() {
    use reai_board_sdk::{AudioStreamAction, AudioStreamScope};
    let request = HidPacket::audio_stream_control(
        AudioStreamAction::Start,
        AudioTransport::UsbVendorHid,
        AudioStreamScope::Timeline,
        0x1234_5678,
        5_000,
    )
    .unwrap();
    assert_eq!(
        &request[..13],
        &[0x0B, 0x6F, 10, 1, 1, 2, 1, 0x78, 0x56, 0x34, 0x12, 0x88, 0x13]
    );

    let ack = [
        0x0A, 0x6F, 10, 0, 1, 1, 2, 0x78, 0x56, 0x34, 0x12, 0x88, 0x13,
    ];
    let state = parse_audio_stream_hid_response(&ack).unwrap();
    assert_eq!(state.active_transport, Some(AudioTransport::UsbVendorHid));
    assert_eq!(state.scope, Some(AudioStreamScope::Timeline));
    assert_eq!(state.lease_id, 0x1234_5678);
    assert_eq!(state.ttl_ms, 5_000);
    assert!(state.matches_request(
        reai_board_sdk::AudioStreamAction::Start,
        AudioTransport::UsbVendorHid,
        AudioStreamScope::Timeline,
        0x1234_5678,
    ));

    let stale_ack = [
        0x0A, 0x6F, 10, 0, 1, 1, 2, 0x79, 0x56, 0x34, 0x12, 0x88, 0x13,
    ];
    assert!(!parse_audio_stream_hid_response(&stale_ack)
        .unwrap()
        .matches_request(
            reai_board_sdk::AudioStreamAction::Start,
            AudioTransport::UsbVendorHid,
            AudioStreamScope::Timeline,
            0x1234_5678,
        ));
}

// ---------------------------------------------------------------------------
// 解码 sink 的丢失语义。要有解码器才跑得起来，所以按传输 feature 门控。
// ---------------------------------------------------------------------------

#[cfg(any(feature = "usb", feature = "ble"))]
mod decoder_sink {
    use reai_board_sdk::kernel::audio::{AudioFrame, AudioTransport, EncodedAudioPacket};
    use reai_board_sdk::kernel::msbc::{MSBC_FRAME_SIZE, MSBC_SYNC_WORD};
    use reai_board_sdk::kernel::sink::{AudioFrameSink, EncodedAudioDecoderSink};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Copy)]
    struct Seen {
        samples: usize,
        sequence: Option<u16>,
        local_drop_frames: u64,
        sequence_gap_frames: u16,
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Seen>>);

    impl AudioFrameSink for Recorder {
        fn on_audio_frame(&self, frame: AudioFrame<'_>) {
            self.0.lock().unwrap().push(Seen {
                samples: frame.pcm.len(),
                sequence: frame.sequence,
                local_drop_frames: frame.local_drop_frames,
                sequence_gap_frames: frame.sequence_gap_frames,
            });
        }
    }

    /// 解码器只校验长度与 sync word，全零负载能解出静音，正好当合法帧用。
    fn frame() -> Vec<u8> {
        let mut f = vec![0u8; MSBC_FRAME_SIZE];
        f[0] = MSBC_SYNC_WORD;
        f
    }

    fn packet<'a>(payload: &'a [u8], sequence: u16, discontinuity: bool) -> EncodedAudioPacket<'a> {
        EncodedAudioPacket {
            payload,
            transport: AudioTransport::BleGatt,
            sequence: Some(sequence),
            device_discontinuity: discontinuity,
        }
    }

    #[test]
    fn device_discontinuity_resets_the_sequence_tracker() {
        let seen = Arc::new(Recorder::default());
        let sink = EncodedAudioDecoderSink::new(seen.clone(), 1);
        let good = frame();

        sink.on_packet(packet(&good, 100, false), 0);
        // 固件在同一租约里重启编码器：序号退回低位，但自己通告了不连续。
        sink.on_packet(packet(&good, 5, true), 0);
        // 不复位的话这一包会被判成乱序整包丢弃，后面几万个包同样出不来。
        sink.on_packet(packet(&good, 6, false), 0);

        let seen = seen.0.lock().unwrap();
        assert_eq!(
            seen.iter().map(|s| s.sequence).collect::<Vec<_>>(),
            vec![Some(100), Some(5), Some(6)]
        );
    }

    #[test]
    fn a_bad_frame_does_not_discard_the_good_frames_beside_it() {
        let seen = Arc::new(Recorder::default());
        let sink = EncodedAudioDecoderSink::new(seen.clone(), 1);

        let mut payload = frame();
        payload.extend_from_slice(&[0u8; MSBC_FRAME_SIZE]); // sync word 不对，解不出来
        sink.on_packet(packet(&payload, 1, false), 0);

        let seen = seen.0.lock().unwrap();
        assert_eq!(seen.len(), 1, "同一包里的好帧必须照送");
        assert!(seen[0].samples > 0);
        assert_eq!(seen[0].local_drop_frames, 1, "坏帧要算进丢失信号");
    }

    #[test]
    fn a_truncated_tail_does_not_discard_the_complete_frame_before_it() {
        let seen = Arc::new(Recorder::default());
        let sink = EncodedAudioDecoderSink::new(seen.clone(), 1);

        // 旧固件的 legacy 信封会截断负载，尾块不足一帧。
        let mut payload = frame();
        payload.extend_from_slice(&[0u8; 30]);
        sink.on_packet(packet(&payload, 1, false), 0);

        let seen = seen.0.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].samples > 0);
        assert_eq!(seen[0].local_drop_frames, 1);
    }

    #[test]
    fn a_packet_with_nothing_decodable_delivers_no_frame() {
        let seen = Arc::new(Recorder::default());
        let sink = EncodedAudioDecoderSink::new(seen.clone(), 1);

        sink.on_packet(packet(&[0u8; MSBC_FRAME_SIZE], 1, false), 0);
        assert!(seen.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_sequence_gap_is_reported_as_wire_loss_not_local_loss() {
        let seen = Arc::new(Recorder::default());
        let sink = EncodedAudioDecoderSink::new(seen.clone(), 1);
        let good = frame();

        sink.on_packet(packet(&good, 1, false), 0);
        sink.on_packet(packet(&good, 4, false), 0);

        let seen = seen.0.lock().unwrap();
        assert_eq!(seen[1].sequence_gap_frames, 2);
        assert_eq!(seen[1].local_drop_frames, 0);
    }
}
