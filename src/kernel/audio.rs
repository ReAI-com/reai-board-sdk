//! Versioned board-audio transport contracts shared by USB HID and BLE GATT.

use crate::kernel::types::ConnectionType;
use serde::{Deserialize, Serialize};
use std::time::Instant;

pub const AUDIO_PROTOCOL_VERSION: u8 = 1;
pub const AUDIO_CAP_USB_VENDOR_HID_MSBC_V1: u32 = 1 << 0;
pub const AUDIO_CAP_BLE_GATT_MSBC_V1: u32 = 1 << 1;
pub const AUDIO_CAP_STREAM_CONTROL_V1: u32 = 1 << 2;
pub const AUDIO_CAP_PACKET_SEQUENCE_V1: u32 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioTransport {
    UsbVendorHid,
    BleGatt,
    /// Compatibility path. Opening this transport can require OS microphone permission.
    UsbUac,
    /// Host-owned system input. The SDK never opens this endpoint itself.
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRouteRequest {
    BoardFirst,
    ExplicitUsbUac,
    ExplicitSystem,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AudioCapabilities {
    pub protocol_version: u8,
    pub usb_vendor_hid_msbc_v1: bool,
    pub ble_gatt_msbc_v1: bool,
    pub stream_control_v1: bool,
    pub packet_sequence_v1: bool,
    pub envelope_version: u8,
    pub usb_max_payload: u8,
    pub ble_max_payload: u8,
    pub default_ttl_ms: u16,
    pub max_ttl_ms: u16,
}

impl AudioCapabilities {
    pub fn from_bits(
        protocol_version: u8,
        bits: u32,
        envelope_version: u8,
        usb_max_payload: u8,
        ble_max_payload: u8,
        default_ttl_ms: u16,
        max_ttl_ms: u16,
    ) -> Self {
        Self {
            protocol_version,
            usb_vendor_hid_msbc_v1: bits & AUDIO_CAP_USB_VENDOR_HID_MSBC_V1 != 0,
            ble_gatt_msbc_v1: bits & AUDIO_CAP_BLE_GATT_MSBC_V1 != 0,
            stream_control_v1: bits & AUDIO_CAP_STREAM_CONTROL_V1 != 0,
            packet_sequence_v1: bits & AUDIO_CAP_PACKET_SEQUENCE_V1 != 0,
            envelope_version,
            usb_max_payload,
            ble_max_payload,
            default_ttl_ms,
            max_ttl_ms,
        }
    }

    pub fn supports(&self, transport: AudioTransport) -> bool {
        match transport {
            AudioTransport::UsbVendorHid => {
                self.protocol_version == AUDIO_PROTOCOL_VERSION
                    && self.usb_vendor_hid_msbc_v1
                    && self.stream_control_v1
                    && self.packet_sequence_v1
                    && self.envelope_version == AUDIO_PROTOCOL_VERSION
                    && usize::from(self.usb_max_payload) >= crate::kernel::msbc::MSBC_FRAME_SIZE
            }
            AudioTransport::BleGatt => {
                self.protocol_version == AUDIO_PROTOCOL_VERSION
                    && self.ble_gatt_msbc_v1
                    && self.stream_control_v1
                    && self.packet_sequence_v1
                    && self.envelope_version == AUDIO_PROTOCOL_VERSION
                    && usize::from(self.ble_max_payload) >= crate::kernel::msbc::MSBC_FRAME_SIZE
            }
            AudioTransport::UsbUac | AudioTransport::System => true,
        }
    }
}

/// Resolve a transport without ever turning Board-first into an implicit OS input request.
pub fn resolve_audio_transport(
    request: AudioRouteRequest,
    connection: Option<ConnectionType>,
    capabilities: &AudioCapabilities,
) -> Option<AudioTransport> {
    match request {
        AudioRouteRequest::BoardFirst => match connection {
            Some(ConnectionType::Usb) if capabilities.supports(AudioTransport::UsbVendorHid) => {
                Some(AudioTransport::UsbVendorHid)
            }
            Some(ConnectionType::Ble) if capabilities.supports(AudioTransport::BleGatt) => {
                Some(AudioTransport::BleGatt)
            }
            _ => None,
        },
        AudioRouteRequest::ExplicitUsbUac if connection == Some(ConnectionType::Usb) => {
            Some(AudioTransport::UsbUac)
        }
        AudioRouteRequest::ExplicitUsbUac => None,
        AudioRouteRequest::ExplicitSystem => Some(AudioTransport::System),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceDisposition {
    First,
    Contiguous,
    Gap { missing: u16 },
    Duplicate,
    OutOfOrder,
}

#[derive(Debug, Default, Clone)]
pub struct SequenceTracker {
    last: Option<u16>,
}

impl SequenceTracker {
    pub fn reset(&mut self) {
        self.last = None;
    }

    pub fn observe(&mut self, sequence: u16) -> SequenceDisposition {
        let Some(last) = self.last else {
            self.last = Some(sequence);
            return SequenceDisposition::First;
        };
        let delta = sequence.wrapping_sub(last);
        match delta {
            0 => SequenceDisposition::Duplicate,
            1 => {
                self.last = Some(sequence);
                SequenceDisposition::Contiguous
            }
            2..=0x7FFF => {
                self.last = Some(sequence);
                SequenceDisposition::Gap { missing: delta - 1 }
            }
            _ => SequenceDisposition::OutOfOrder,
        }
    }
}

/// Borrowed encoded packet parsed from a transport envelope.
#[derive(Debug, Clone, Copy)]
pub struct EncodedAudioPacket<'a> {
    pub payload: &'a [u8],
    pub transport: AudioTransport,
    pub sequence: Option<u16>,
    pub device_discontinuity: bool,
}

/// Decoded 16 kHz mono frame delivered to Host-owned audio infrastructure.
pub struct AudioFrame<'a> {
    pub pcm: &'a [f32],
    pub sample_rate: u32,
    pub channels: u8,
    pub transport: AudioTransport,
    pub connection_epoch: u64,
    pub sequence: Option<u16>,
    pub captured_at_monotonic: Instant,
    pub device_discontinuity: bool,
    pub sequence_gap_frames: u16,
    pub local_drop_frames: u64,
}

impl AudioFrame<'_> {
    pub fn discontinuity(&self) -> bool {
        self.device_discontinuity || self.sequence_gap_frames != 0 || self.local_drop_frames != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioStreamAction {
    Stop = 0,
    Start = 1,
    Heartbeat = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioStreamScope {
    Session = 1,
    Timeline = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioStreamResult {
    Ok = 0,
    UnsupportedVersion = 1,
    Busy = 2,
    LeaseMismatch = 3,
    InvalidArgument = 4,
    TransportUnavailable = 5,
}

impl TryFrom<u8> for AudioStreamResult {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::UnsupportedVersion),
            2 => Ok(Self::Busy),
            3 => Ok(Self::LeaseMismatch),
            4 => Ok(Self::InvalidArgument),
            5 => Ok(Self::TransportUnavailable),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStreamState {
    pub result: AudioStreamResult,
    pub protocol_version: u8,
    pub active_transport: Option<AudioTransport>,
    pub scope: Option<AudioStreamScope>,
    pub lease_id: u32,
    pub ttl_ms: u16,
}

impl AudioStreamState {
    /// Verify that an OK acknowledgement belongs to the exact request which caused it. This
    /// prevents a delayed/stale same-command response from silently switching route ownership.
    pub fn matches_request(
        &self,
        action: AudioStreamAction,
        transport: AudioTransport,
        scope: AudioStreamScope,
        lease_id: u32,
    ) -> bool {
        if self.result != AudioStreamResult::Ok || self.protocol_version != AUDIO_PROTOCOL_VERSION {
            return false;
        }
        match action {
            AudioStreamAction::Start | AudioStreamAction::Heartbeat => {
                self.active_transport == Some(transport)
                    && self.scope == Some(scope)
                    && self.lease_id == lease_id
                    && self.ttl_ms != 0
            }
            AudioStreamAction::Stop => {
                self.active_transport.is_none()
                    && self.scope.is_none()
                    && self.lease_id == 0
                    && self.ttl_ms == 0
            }
        }
    }
}
