use std::fs;
use std::path::PathBuf;

use reai_board_sdk::kernel::audio::{AudioStreamAction, AudioStreamScope, AudioTransport};
use reai_board_sdk::kernel::protocol_gatt::{
    hid_to_gatt_command, parse_audio_packet_v1, AUDIO_CHAR_UUID, CMD_CHAR_UUID, EVENT_CHAR_UUID,
    SERVICE_UUID, VENDOR_DEVICE_PREFIX,
};
use reai_board_sdk::kernel::protocol_hid::{
    parse_app_online_gatt_response, parse_audio_capabilities_gatt_response,
    parse_audio_stream_gatt_response, parse_open_url_gatt_response,
    parse_silent_record_gatt_response, parse_sleep_timeout_gatt_response,
    parse_work_mode_gatt_response, HidPacket, WorkMode, CMD_AI_GET_APP_ONLINE, CMD_AI_GET_OPEN_URL,
    CMD_GET_SILENT_RECORD, CMD_GET_SLEEP_TIMEOUT, KEY_DATA_LEN,
};
use reai_board_sdk::kernel::types::{ConnectionType, SleepTimeout};
use reai_board_sdk::tool::parse::parse_device_info_from_gatt;
use serde_json::{json, Value};

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/flutter_protocol_vectors.json");
    let body = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "共享 Flutter 协议 fixture 不可读 {}: {error}",
            path.display()
        )
    });
    serde_json::from_str(&body).expect("共享 Flutter 协议 fixture 必须是合法 JSON")
}

fn command(packet: [u8; 64]) -> Value {
    json!(hid_to_gatt_command(&packet))
}

fn bytes(value: &Value) -> Vec<u8> {
    value
        .as_array()
        .expect("协议向量必须是数组")
        .iter()
        .map(|byte| byte.as_u64().expect("协议字节必须是整数") as u8)
        .collect()
}

#[test]
fn flutter_golden_locks_vendor_identity() {
    let fixture = fixture();
    assert_eq!(fixture["device_prefix"], json!(VENDOR_DEVICE_PREFIX));
    assert_eq!(fixture["uuids"]["service"], json!(SERVICE_UUID.to_string()));
    assert_eq!(
        fixture["uuids"]["command"],
        json!(CMD_CHAR_UUID.to_string())
    );
    assert_eq!(
        fixture["uuids"]["event"],
        json!(EVENT_CHAR_UUID.to_string())
    );
    assert_eq!(
        fixture["uuids"]["audio"],
        json!(AUDIO_CHAR_UUID.to_string())
    );
    assert_ne!(
        fixture["uuids"]["service"],
        json!("0000fe60-0000-1000-8000-00805f9b34fb"),
        "固件 UUID 不是 Bluetooth Base UUID 的 16-bit 展开"
    );
}

#[test]
fn flutter_golden_locks_mobile_command_bytes() {
    let fixture = fixture();
    let commands = &fixture["commands"];
    assert_eq!(
        commands["get_device_info"],
        command(HidPacket::get_device_info())
    );
    assert_eq!(
        commands["get_key_config"],
        command(HidPacket::get_key_config())
    );

    let key_data: [u8; KEY_DATA_LEN] = std::array::from_fn(|index| index as u8);
    assert_eq!(
        commands["set_key_config"],
        command(HidPacket::set_key_config(&key_data))
    );
    assert_eq!(
        commands["get_work_mode"],
        command(HidPacket::get_work_mode())
    );
    assert_eq!(
        commands["get_silent_record"],
        command(HidPacket::get_silent_record())
    );
    assert_eq!(
        commands["set_silent_record_true"],
        command(HidPacket::set_silent_record(true))
    );
    assert_eq!(
        commands["get_sleep_timeout"],
        command(HidPacket::get_sleep_timeout())
    );
    assert_eq!(
        commands["set_sleep_timeout"],
        command(HidPacket::set_sleep_timeout(SleepTimeout::new(60, 600)))
    );
    assert_eq!(
        commands["notify_app_online"],
        command(HidPacket::app_online_notify(true))
    );
    assert_eq!(
        commands["get_app_online"],
        command(HidPacket::get_app_online())
    );
    assert_eq!(commands["get_open_url"], command(HidPacket::get_open_url()));
    assert_eq!(
        commands["get_audio_capabilities"],
        command(HidPacket::get_audio_capabilities())
    );
    assert_eq!(
        commands["start_audio"],
        command(
            HidPacket::audio_stream_control(
                AudioStreamAction::Start,
                AudioTransport::BleGatt,
                AudioStreamScope::Session,
                0x1234_5678,
                5_000,
            )
            .expect("BLE GATT 音频命令必须可编码")
        )
    );
}

#[test]
fn flutter_golden_locks_mobile_response_parsing() {
    let fixture = fixture();
    let responses = &fixture["responses"];

    let info = parse_device_info_from_gatt(&bytes(&responses["device_info"]), ConnectionType::Ble)
        .expect("设备信息向量必须可解析");
    assert_eq!(info.mac_address, "AA:BB:CC:DD:EE:FF");
    assert_eq!(info.firmware_version, "1.55");
    assert_eq!(info.battery_level, 73);
    assert_eq!(info.chip_id, "1CE60729");

    assert_eq!(
        parse_work_mode_gatt_response(&bytes(&responses["work_mode_yolo"])),
        Some(WorkMode::Yolo)
    );
    assert_eq!(
        parse_silent_record_gatt_response(
            &bytes(&responses["silent_record_on"]),
            CMD_GET_SILENT_RECORD,
        ),
        Some(true)
    );
    assert_eq!(
        parse_sleep_timeout_gatt_response(
            &bytes(&responses["sleep_timeout"]),
            CMD_GET_SLEEP_TIMEOUT,
        ),
        Some(SleepTimeout::new(60, 600))
    );
    assert_eq!(
        parse_app_online_gatt_response(&bytes(&responses["app_online"]), CMD_AI_GET_APP_ONLINE,),
        Some(true)
    );
    assert_eq!(
        parse_open_url_gatt_response(&bytes(&responses["open_url"]), CMD_AI_GET_OPEN_URL),
        Some("https://x".to_string())
    );

    let capabilities =
        parse_audio_capabilities_gatt_response(&bytes(&responses["audio_capabilities"]))
            .expect("音频能力向量必须可解析");
    assert!(capabilities.supports(AudioTransport::BleGatt));
    assert_eq!(capabilities.ble_max_payload, 57);

    let stream = parse_audio_stream_gatt_response(&bytes(&responses["audio_started"]))
        .expect("音频 lease 向量必须可解析");
    assert!(stream.matches_request(
        AudioStreamAction::Start,
        AudioTransport::BleGatt,
        AudioStreamScope::Session,
        0x1234_5678,
    ));

    let mut packet = bytes(&fixture["audio"]["versioned_header"]);
    packet.extend(std::iter::repeat_n(0xAD, 57));
    let audio = parse_audio_packet_v1(&packet).expect("版本化音频向量必须可解析");
    assert_eq!(audio.sequence, Some(0x1234));
    assert!(audio.device_discontinuity);
    assert_eq!(audio.payload.len(), 57);
}
