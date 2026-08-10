#![cfg(feature = "test-mode")]

use reai_board_sdk::kernel::protocol_hid::{
    parse_factory_key_control_ack, parse_factory_key_event, FactoryKeyControlResult, HidPacket,
    CMD_AI_FACTORY_KEY_EVENT, CMD_AI_FACTORY_KEY_TEST_CONTROL, FACTORY_KEY_TEST_PROTOCOL_VERSION,
};

#[test]
fn builds_enable_and_disable_packets_with_a_nonzero_session() {
    let enable =
        HidPacket::factory_key_test_control(true, 0x1234).expect("nonzero session should be valid");
    assert_eq!(
        &enable[..7],
        &[
            0x0B,
            CMD_AI_FACTORY_KEY_TEST_CONTROL,
            0x04,
            0x01,
            FACTORY_KEY_TEST_PROTOCOL_VERSION,
            0x34,
            0x12,
        ]
    );

    let disable = HidPacket::factory_key_test_control(false, 0x1234)
        .expect("nonzero session should be valid");
    assert_eq!(disable[3], 0x00);
    assert!(HidPacket::factory_key_test_control(true, 0).is_err());
}

#[test]
fn parses_control_ack_and_rejects_wrong_session_or_version() {
    let ack = parse_factory_key_control_ack(
        &[
            CMD_AI_FACTORY_KEY_TEST_CONTROL,
            0x05,
            0x00,
            FACTORY_KEY_TEST_PROTOCOL_VERSION,
            0x01,
            0x34,
            0x12,
        ],
        0x1234,
    )
    .expect("valid ack");
    assert_eq!(ack.result, FactoryKeyControlResult::Ok);
    assert!(ack.enabled);
    assert_eq!(ack.session, 0x1234);

    assert!(parse_factory_key_control_ack(
        &[CMD_AI_FACTORY_KEY_TEST_CONTROL, 0x05, 0, 1, 1, 0x34, 0x12],
        0x9999,
    )
    .is_err());
    assert!(parse_factory_key_control_ack(
        &[CMD_AI_FACTORY_KEY_TEST_CONTROL, 0x05, 0, 9, 1, 0x34, 0x12],
        0x1234,
    )
    .is_err());
}

#[test]
fn parses_physical_event_and_rejects_invalid_or_foreign_data() {
    let event = parse_factory_key_event(
        &[
            CMD_AI_FACTORY_KEY_EVENT,
            0x06,
            FACTORY_KEY_TEST_PROTOCOL_VERSION,
            0x34,
            0x12,
            0x04,
            0x01,
            0x2A,
        ],
        0x1234,
    )
    .expect("valid physical event");
    assert_eq!(event.session, 0x1234);
    assert_eq!(event.input_index, 4);
    assert!(event.pressed);
    assert_eq!(event.sequence, 0x2A);

    assert!(parse_factory_key_event(
        &[CMD_AI_FACTORY_KEY_EVENT, 0x06, 1, 0x34, 0x12, 12, 1, 1],
        0x1234,
    )
    .is_err());
    assert!(parse_factory_key_event(
        &[CMD_AI_FACTORY_KEY_EVENT, 0x06, 1, 0x34, 0x12, 4, 1, 1],
        0x9999,
    )
    .is_err());
    assert!(parse_factory_key_event(
        &[CMD_AI_FACTORY_KEY_EVENT, 0x06, 9, 0x34, 0x12, 4, 1, 1],
        0x1234,
    )
    .is_err());
}
