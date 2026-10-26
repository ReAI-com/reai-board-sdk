//! BLE admission before publishing a connected event.
//!
//! Firmware 1.75 drops every command except 0x74 while a bound connection is
//! awaiting identity. Capability discovery must therefore follow identity.

use std::future::Future;

use anyhow::Result;

use crate::kernel::protocol_gatt::hid_to_gatt_command;
use crate::kernel::protocol_hid::*;
use crate::kernel::types::ConnectionType;
use crate::tool::parse::parse_device_info_from_gatt;

use super::gatt_client::VendorGattClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Identity {
    Verified,
    Unbound,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    BoundToOtherHost,
    Disconnected,
    Retry(&'static str),
}

/// One connection generation, including OS liveness and notification readiness.
/// The same admission sequence is exercised by protocol-model tests below.
pub(crate) trait Transport: Sync {
    fn current(&self) -> bool;
    fn active(&self) -> impl Future<Output = bool> + Send;
    fn disconnect_reason(&self) -> Option<u8>;
    fn command(
        &self,
        command: u8,
        packet: &[u8],
        timeout_ms: u64,
    ) -> impl Future<Output = Result<Vec<u8>>> + Send;
    fn subscribe_audio(&self) -> impl Future<Output = Result<()>> + Send;
}

pub(crate) struct GattTransport<'a> {
    pub client: &'a VendorGattClient,
    pub epoch: u64,
}

impl Transport for GattTransport<'_> {
    fn current(&self) -> bool {
        self.client.connection_is_current(self.epoch)
    }

    async fn active(&self) -> bool {
        self.client.connection_is_active(self.epoch).await
    }

    fn disconnect_reason(&self) -> Option<u8> {
        self.client.device_disconnect_reason(self.epoch)
    }

    async fn command(&self, command: u8, packet: &[u8], timeout_ms: u64) -> Result<Vec<u8>> {
        self.client
            .send_command_in_session(self.epoch, command, packet, timeout_ms)
            .await
    }

    async fn subscribe_audio(&self) -> Result<()> {
        self.client.subscribe_audio(self.epoch).await
    }
}

async fn ensure_active(transport: &impl Transport) -> Result<(), AdmissionError> {
    if transport.active().await {
        Ok(())
    } else {
        Err(AdmissionError::Disconnected)
    }
}

/// A silent/unknown command alone is never proof of old firmware. Old releases
/// must still answer device-info with a version predating host binding.
async fn probe_device(
    transport: &impl Transport,
    require_legacy_version: bool,
) -> Result<(), AdmissionError> {
    ensure_active(transport).await?;
    let response = transport
        .command(
            CMD_GET_DEVICE_INFO,
            &hid_to_gatt_command(&HidPacket::get_device_info()),
            2000,
        )
        .await;
    ensure_active(transport).await?;
    let info = response
        .ok()
        .and_then(|r| parse_device_info_from_gatt(&r, ConnectionType::Ble).ok())
        .ok_or(AdmissionError::Retry(
            "BLE 设备信息未确认，不能宣布连接成功",
        ))?;
    if require_legacy_version {
        let version = info
            .firmware_version
            .split_once('.')
            .and_then(|(a, b)| Some((a.parse::<u16>().ok()?, b.parse::<u16>().ok()?)));
        if !matches!(version, Some(v) if v > (0, 0) && v < (1, 75)) {
            return Err(AdmissionError::Retry("BLE 身份未确认，不能按旧固件放行"));
        }
    }
    Ok(())
}

async fn verify_identity(
    transport: &impl Transport,
    token: &[u8; 16],
) -> Result<Identity, AdmissionError> {
    let mut retry_identity = false;
    loop {
        // Send first, while firmware's three-second gate is open. Audio subscription
        // is deliberately deferred until after this exchange.
        let response = transport
            .command(
                CMD_AI_HOST_IDENTITY,
                &hid_to_gatt_command(&HidPacket::host_identity(token)),
                1000,
            )
            .await;
        return match response {
            Ok(response) => {
                match parse_host_result_gatt_response(CMD_AI_HOST_IDENTITY, &response) {
                    Some(0) => {
                        ensure_active(transport).await?;
                        Ok(Identity::Verified)
                    }
                    Some(1) => {
                        // Actual 1.75 firmware also returns 01 when unbound, contrary to
                        // its protocol document. Its open command gate then answers 72.
                        let status = if transport.active().await {
                            transport
                                .command(
                                    CMD_AI_HOST_BIND_GET,
                                    &hid_to_gatt_command(&HidPacket::host_bind_get()),
                                    1000,
                                )
                                .await
                                .ok()
                                .and_then(|r| parse_host_bind_get_gatt_response(&r))
                        } else {
                            None
                        };
                        if status.is_some_and(|s| s.bound)
                            || transport.disconnect_reason()
                                == Some(DEVICE_DISCONNECT_REASON_BOUND_TO_OTHER_HOST)
                        {
                            return Err(AdmissionError::BoundToOtherHost);
                        }
                        ensure_active(transport).await?;
                        if status.is_some_and(|s| !s.bound) {
                            Ok(Identity::Unbound)
                        } else {
                            Err(AdmissionError::Retry("BLE 身份被拒，未能确认是否已绑定"))
                        }
                    }
                    _ => Err(AdmissionError::Retry("BLE 身份响应无效")),
                }
            }
            Err(_) => {
                if retry_identity {
                    return Err(AdmissionError::Retry("BLE 身份重试仍未确认"));
                }
                // Old firmware ignores unknown 74. Check capability only after that
                // attempt, never ahead of the identity required by bound 1.75.
                ensure_active(transport).await?;
                let response = transport
                    .command(
                        CMD_AI_GET_AUDIO_CAPABILITIES,
                        &hid_to_gatt_command(&HidPacket::get_audio_capabilities()),
                        2000,
                    )
                    .await;
                ensure_active(transport).await?;
                match response
                    .as_ref()
                    .ok()
                    .and_then(|r| parse_audio_capabilities_gatt_response(r))
                {
                    Some(caps) if caps.host_binding_v1 => {
                        // 74 is idempotent in 1.75. A lost success notification must
                        // not tear down a link whose now-open gate answers 6E.
                        // Retry once, and still require a real identity response.
                        retry_identity = true;
                        continue;
                    }
                    Some(_) => Ok(Identity::Legacy),
                    None => {
                        probe_device(transport, true).await?;
                        Ok(Identity::Legacy)
                    }
                }
            }
        };
    }
}

/// Success authorizes publishing a connected event, and only for this epoch.
pub(crate) async fn admit(
    transport: &impl Transport,
    token: Option<&[u8; 16]>,
) -> Result<Identity, AdmissionError> {
    // Avoid an OS round trip before 74: GAP connection, discovery and Event CCC
    // have already spent part of firmware's three-second identity deadline.
    // The command path checks this epoch again; OS liveness is required below.
    if !transport.current() {
        return Err(AdmissionError::Disconnected);
    }
    let identity = match token {
        Some(token) => verify_identity(transport, token).await?,
        None => {
            // Without a token a bound 1.75 board cannot admit us. Require a real
            // command round trip instead of briefly publishing a false link.
            probe_device(transport, false).await?;
            Identity::Legacy
        }
    };
    ensure_active(transport).await?;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        transport.subscribe_audio(),
    )
    .await
    .map_err(|_| AdmissionError::Retry("BLE 音频通知订阅超时"))?
    .map_err(|_| AdmissionError::Retry("BLE 音频通知订阅失败"))?;
    ensure_active(transport).await?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::kernel::audio::{AUDIO_CAP_HOST_BINDING_V1, AUDIO_PROTOCOL_VERSION};

    #[derive(Clone, Copy)]
    enum Firmware {
        Bound175 { matches: bool },
        Unbound175,
        Legacy { has_caps: bool },
        Silent,
        MissingIdentityReply,
    }

    struct Model {
        firmware: Firmware,
        state: Mutex<ModelState>,
    }

    struct ModelState {
        elapsed_ms: u64,
        os_connected: bool,
        epoch: u64,
        reason: Option<u8>,
        verified: bool,
        commands: Vec<u8>,
        audio_subscribed: bool,
        close_on_audio: bool,
        replace_on_audio: bool,
        invalid_identity: bool,
        invalid_caps: bool,
        identity_sent_at: Option<u64>,
        hang_audio: bool,
        active_delay_ms: u64,
        drop_identity_reply_once: bool,
        physical_disconnect_at: Option<u64>,
    }

    impl ModelState {
        fn advance(&mut self, milliseconds: u64) {
            self.elapsed_ms += milliseconds;
            if self
                .physical_disconnect_at
                .is_some_and(|at| self.elapsed_ms >= at)
            {
                self.os_connected = false;
            }
        }
    }

    impl Model {
        fn new(firmware: Firmware) -> Self {
            Self {
                firmware,
                state: Mutex::new(ModelState {
                    // Service discovery and Event CCC have already consumed
                    // part of firmware's budget before admission starts.
                    elapsed_ms: 1200,
                    os_connected: true,
                    epoch: 1,
                    reason: None,
                    verified: false,
                    commands: Vec::new(),
                    audio_subscribed: false,
                    close_on_audio: false,
                    replace_on_audio: false,
                    invalid_identity: false,
                    invalid_caps: false,
                    identity_sent_at: None,
                    hang_audio: false,
                    active_delay_ms: 0,
                    drop_identity_reply_once: false,
                    physical_disconnect_at: None,
                }),
            }
        }

        fn commands(&self) -> Vec<u8> {
            self.state.lock().unwrap().commands.clone()
        }
    }

    fn caps_response(bound_capability: bool) -> Vec<u8> {
        let mut response = vec![CMD_AI_GET_AUDIO_CAPABILITIES, 13, 0, AUDIO_PROTOCOL_VERSION];
        response.extend_from_slice(
            &(if bound_capability {
                AUDIO_CAP_HOST_BINDING_V1
            } else {
                1
            })
            .to_le_bytes(),
        );
        response.extend_from_slice(&[0; 7]);
        response
    }

    fn info_response(minor: u8) -> Vec<u8> {
        let mut response = vec![CMD_GET_DEVICE_INFO, 21, 0];
        let mut payload = [0u8; 20];
        payload[0] = 2;
        payload[9] = minor;
        payload[10] = 1;
        payload[16..20].copy_from_slice(&[1, 2, 3, 4]);
        response.extend_from_slice(&payload);
        response
    }

    impl Transport for Model {
        fn current(&self) -> bool {
            let state = self.state.lock().unwrap();
            state.epoch == 1 && state.reason.is_none()
        }

        async fn active(&self) -> bool {
            let mut state = self.state.lock().unwrap();
            let delay = state.active_delay_ms;
            state.advance(delay);
            state.os_connected && state.epoch == 1 && state.reason.is_none()
        }

        fn disconnect_reason(&self) -> Option<u8> {
            self.state.lock().unwrap().reason
        }

        async fn command(&self, command: u8, packet: &[u8], timeout_ms: u64) -> Result<Vec<u8>> {
            tokio::task::yield_now().await;
            assert_eq!(packet[0], command);
            assert_eq!(packet.len(), usize::from(packet[1]) + 2);
            let mut state = self.state.lock().unwrap();
            state.commands.push(command);
            if !state.os_connected {
                anyhow::bail!("disconnected");
            }
            // Model the actual 1.75 FE61 gate: ATT writes succeed, but every
            // business command except 74 is silently dropped until verified.
            let gated = matches!(self.firmware, Firmware::Bound175 { .. }) && !state.verified;
            let response = if gated && command != CMD_AI_HOST_IDENTITY {
                None
            } else {
                match (self.firmware, command) {
                    (_, CMD_AI_HOST_IDENTITY) if state.invalid_identity => Some(vec![0x74, 1]),
                    (Firmware::Bound175 { matches }, CMD_AI_HOST_IDENTITY) => {
                        assert_eq!(
                            packet,
                            hid_to_gatt_command(&HidPacket::host_identity(&[7; 16]))
                        );
                        let sent_at = state.elapsed_ms;
                        state.identity_sent_at.get_or_insert(sent_at);
                        if !state.verified && state.elapsed_ms >= 3000 {
                            state.physical_disconnect_at = Some(3100);
                            state.reason = Some(3);
                            None
                        } else if matches {
                            state.verified = true;
                            if state.drop_identity_reply_once {
                                state.drop_identity_reply_once = false;
                                None
                            } else {
                                Some(vec![0x74, 1, 0])
                            }
                        } else {
                            // Firmware sends 60, then allows 100ms for delivery
                            // before the physical GAP disconnect. Main-loop
                            // scheduling itself remains a hardware acceptance item.
                            state.physical_disconnect_at = Some(state.elapsed_ms + 100);
                            state.reason = Some(3);
                            Some(vec![0x74, 1, 1])
                        }
                    }
                    (Firmware::Unbound175, CMD_AI_HOST_IDENTITY) => Some(vec![0x74, 1, 1]),
                    (Firmware::Unbound175, CMD_AI_HOST_BIND_GET) => {
                        let mut result = vec![0x72, 13, 0];
                        result.extend_from_slice(&[0; 12]);
                        Some(result)
                    }
                    (_, CMD_AI_GET_AUDIO_CAPABILITIES) if state.invalid_caps => {
                        Some(vec![0x6e, 1, 0])
                    }
                    (Firmware::Legacy { has_caps: true }, CMD_AI_GET_AUDIO_CAPABILITIES) => {
                        Some(caps_response(false))
                    }
                    (
                        Firmware::MissingIdentityReply | Firmware::Bound175 { .. },
                        CMD_AI_GET_AUDIO_CAPABILITIES,
                    ) => Some(caps_response(true)),
                    (Firmware::Legacy { has_caps }, CMD_GET_DEVICE_INFO) => {
                        Some(info_response(if has_caps { 74 } else { 57 }))
                    }
                    (
                        Firmware::Unbound175 | Firmware::MissingIdentityReply,
                        CMD_GET_DEVICE_INFO,
                    ) => Some(info_response(75)),
                    _ => None,
                }
            };
            match response {
                Some(response) => {
                    state.advance(50);
                    Ok(response)
                }
                None => {
                    state.advance(timeout_ms);
                    if gated && !state.verified && state.elapsed_ms >= 3000 {
                        state.reason = Some(3);
                        state.physical_disconnect_at = Some(3100);
                        state.advance(0);
                    }
                    anyhow::bail!("command response timed out")
                }
            }
        }

        async fn subscribe_audio(&self) -> Result<()> {
            let hang = self.state.lock().unwrap().hang_audio;
            if hang {
                std::future::pending::<()>().await;
            }
            let mut state = self.state.lock().unwrap();
            if matches!(self.firmware, Firmware::Bound175 { .. }) {
                assert!(
                    state.verified,
                    "Audio CCC must not consume the identity deadline"
                );
            }
            state.audio_subscribed = true;
            if state.close_on_audio {
                state.os_connected = false;
            }
            if state.replace_on_audio {
                state.epoch += 1;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn bound_175_authenticates_before_caps_and_audio_within_deadline() {
        let model = Model::new(Firmware::Bound175 { matches: true });
        assert_eq!(admit(&model, Some(&[7; 16])).await, Ok(Identity::Verified));
        assert_eq!(model.commands(), vec![0x74]);
        let state = model.state.lock().unwrap();
        assert!(state.identity_sent_at.unwrap() < 3000);
        assert!(state.audio_subscribed);
    }

    #[tokio::test]
    async fn identity_does_not_wait_for_an_os_liveness_round_trip() {
        let model = Model::new(Firmware::Bound175 { matches: true });
        {
            let mut state = model.state.lock().unwrap();
            state.elapsed_ms = 2200;
            state.active_delay_ms = 1000;
        }
        assert_eq!(admit(&model, Some(&[7; 16])).await, Ok(Identity::Verified));
        assert_eq!(model.state.lock().unwrap().identity_sent_at, Some(2200));
    }

    #[tokio::test]
    async fn lost_identity_success_reply_gets_one_retry_on_the_live_open_gate() {
        let model = Model::new(Firmware::Bound175 { matches: true });
        model.state.lock().unwrap().drop_identity_reply_once = true;
        assert_eq!(admit(&model, Some(&[7; 16])).await, Ok(Identity::Verified));
        assert_eq!(model.commands(), vec![0x74, 0x6e, 0x74]);
        assert!(model.state.lock().unwrap().audio_subscribed);
    }

    #[tokio::test]
    async fn discovery_can_exhaust_firmware_deadline_before_admission() {
        let model = Model::new(Firmware::Bound175 { matches: true });
        model.state.lock().unwrap().elapsed_ms = 3050;
        assert_eq!(
            admit(&model, Some(&[7; 16])).await,
            Err(AdmissionError::Disconnected)
        );
        assert!(!model.state.lock().unwrap().audio_subscribed);
    }

    #[tokio::test]
    async fn firmware_model_reproduces_original_caps_first_deadlock() {
        let model = Model::new(Firmware::Bound175 { matches: true });
        assert!(model.command(0x6e, &[0x6e, 0], 2000).await.is_err());
        assert_eq!(model.disconnect_reason(), Some(3));
        assert!(!model.active().await);
        assert!(!model.state.lock().unwrap().verified);
    }

    #[tokio::test]
    async fn actual_175_unbound_reply_01_is_not_misreported_as_another_host() {
        let model = Model::new(Firmware::Unbound175);
        assert_eq!(admit(&model, Some(&[7; 16])).await, Ok(Identity::Unbound));
        assert_eq!(model.commands(), vec![0x74, 0x72]);
    }

    #[tokio::test]
    async fn mismatched_identity_is_rejected_without_connected_or_audio() {
        let model = Model::new(Firmware::Bound175 { matches: false });
        assert_eq!(
            admit(&model, Some(&[7; 16])).await,
            Err(AdmissionError::BoundToOtherHost)
        );
        let mut state = model.state.lock().unwrap();
        assert!(!state.audio_subscribed);
        assert!(
            state.os_connected,
            "60 must reject before physical disconnect"
        );
        state.advance(100);
        assert!(!state.os_connected);
    }

    #[tokio::test]
    async fn old_firmware_ignores_74_then_confirms_no_binding_capability() {
        let model = Model::new(Firmware::Legacy { has_caps: true });
        assert_eq!(admit(&model, Some(&[7; 16])).await, Ok(Identity::Legacy));
        assert_eq!(model.commands(), vec![0x74, 0x6e]);
    }

    #[tokio::test]
    async fn pre_159_firmware_requires_successful_device_info_before_admission() {
        let model = Model::new(Firmware::Legacy { has_caps: false });
        assert_eq!(admit(&model, Some(&[7; 16])).await, Ok(Identity::Legacy));
        assert_eq!(model.commands(), vec![0x74, 0x6e, 0x13]);
    }

    #[tokio::test]
    async fn silent_transport_never_becomes_legacy_after_repeated_attempts() {
        let model = Model::new(Firmware::Silent);
        for _ in 0..4 {
            assert!(admit(&model, Some(&[7; 16])).await.is_err());
        }
        assert!(!model.state.lock().unwrap().audio_subscribed);
    }

    #[tokio::test]
    async fn binding_capability_cannot_bypass_missing_identity_response() {
        let model = Model::new(Firmware::MissingIdentityReply);
        assert!(matches!(
            admit(&model, Some(&[7; 16])).await,
            Err(AdmissionError::Retry(_))
        ));
        assert_eq!(model.commands(), vec![0x74, 0x6e, 0x74]);
        assert!(!model.state.lock().unwrap().audio_subscribed);
    }

    #[tokio::test]
    async fn malformed_capability_and_new_firmware_info_are_not_legacy_proof() {
        let model = Model::new(Firmware::MissingIdentityReply);
        model.state.lock().unwrap().invalid_caps = true;
        assert!(matches!(
            admit(&model, Some(&[7; 16])).await,
            Err(AdmissionError::Retry(_))
        ));
    }

    #[tokio::test]
    async fn malformed_identity_is_not_a_rejection_or_legacy_fallback() {
        let model = Model::new(Firmware::Unbound175);
        model.state.lock().unwrap().invalid_identity = true;
        assert_eq!(
            admit(&model, Some(&[7; 16])).await,
            Err(AdmissionError::Retry("BLE 身份响应无效"))
        );
        assert_eq!(model.commands(), vec![0x74]);
    }

    #[tokio::test]
    async fn disconnect_or_epoch_change_after_auth_cannot_publish_connected() {
        for replace_epoch in [false, true] {
            let model = Model::new(Firmware::Bound175 { matches: true });
            {
                let mut state = model.state.lock().unwrap();
                state.close_on_audio = !replace_epoch;
                state.replace_on_audio = replace_epoch;
            }
            assert_eq!(
                admit(&model, Some(&[7; 16])).await,
                Err(AdmissionError::Disconnected)
            );
        }
    }

    #[tokio::test]
    async fn no_token_cannot_briefly_admit_a_bound_175_board() {
        let model = Model::new(Firmware::Bound175 { matches: true });
        assert_eq!(admit(&model, None).await, Err(AdmissionError::Disconnected));
        assert!(!model.state.lock().unwrap().audio_subscribed);
    }

    #[tokio::test]
    async fn no_token_still_supports_unbound_firmware() {
        let model = Model::new(Firmware::Unbound175);
        assert_eq!(admit(&model, None).await, Ok(Identity::Legacy));
        assert_eq!(model.commands(), vec![0x13]);
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_audio_subscription_is_bounded_after_identity() {
        let model = Model::new(Firmware::Bound175 { matches: true });
        model.state.lock().unwrap().hang_audio = true;
        assert_eq!(
            admit(&model, Some(&[7; 16])).await,
            Err(AdmissionError::Retry("BLE 音频通知订阅超时"))
        );
    }
}
