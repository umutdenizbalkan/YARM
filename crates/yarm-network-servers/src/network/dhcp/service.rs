// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::dhcp_abi::{
    DhcpCodecError, DhcpInterfaceConfig, DhcpLease, DhcpRequest, DhcpResponse, DhcpState,
    DhcpStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpService {
    state: DhcpState,
    config: Option<DhcpInterfaceConfig>,
    lease: Option<DhcpLease>,
    polls: u64,
}

impl DhcpService {
    pub const fn new() -> Self {
        Self {
            state: DhcpState::Unconfigured,
            config: None,
            lease: None,
            polls: 0,
        }
    }

    pub const fn state(&self) -> DhcpState {
        self.state
    }

    pub const fn config(&self) -> Option<DhcpInterfaceConfig> {
        self.config
    }

    pub const fn lease(&self) -> Option<DhcpLease> {
        self.lease
    }

    pub const fn polls(&self) -> u64 {
        self.polls
    }

    pub fn handle_request(&mut self, request: DhcpRequest) -> DhcpResponse {
        match request {
            DhcpRequest::GetStatus { request_id } => self.response(DhcpStatus::Ok, request_id),
            DhcpRequest::ConfigureInterface { request_id, config } => {
                self.config = Some(config);
                self.lease = None;
                self.state = DhcpState::Configured;
                self.response(DhcpStatus::Ok, request_id)
            }
            DhcpRequest::Start { request_id } => {
                let status = match (self.config, self.state) {
                    (None, _) => DhcpStatus::NotConfigured,
                    (Some(_), DhcpState::Running) => DhcpStatus::AlreadyRunning,
                    (Some(_), _) => {
                        self.state = DhcpState::Running;
                        DhcpStatus::Ok
                    }
                };
                self.response(status, request_id)
            }
            DhcpRequest::Stop { request_id } => {
                let status = match (self.config, self.state) {
                    (None, _) => DhcpStatus::NotConfigured,
                    (Some(_), DhcpState::Running) => {
                        self.state = DhcpState::Stopped;
                        DhcpStatus::Ok
                    }
                    (Some(_), _) => DhcpStatus::NotRunning,
                };
                self.response(status, request_id)
            }
            DhcpRequest::Poll {
                request_id,
                timeout_hint: _,
            } => {
                let status = match (self.config, self.state, self.lease) {
                    (None, _, _) => DhcpStatus::NotConfigured,
                    (Some(_), state, _) if state != DhcpState::Running => DhcpStatus::NotRunning,
                    (Some(_), DhcpState::Running, lease) => {
                        self.polls = self.polls.saturating_add(1);
                        if lease.is_some() {
                            DhcpStatus::Ok
                        } else {
                            DhcpStatus::NoLease
                        }
                    }
                    _ => DhcpStatus::InvalidState,
                };
                self.response(status, request_id)
            }
            DhcpRequest::GetLease { request_id } => self.response(
                if self.lease.is_some() {
                    DhcpStatus::Ok
                } else {
                    DhcpStatus::NoLease
                },
                request_id,
            ),
            DhcpRequest::ClearLease { request_id } => {
                self.lease = None;
                self.response(DhcpStatus::Ok, request_id)
            }
        }
    }

    pub fn handle_wire_request(&mut self, opcode: u16, payload: &[u8]) -> DhcpResponse {
        match DhcpRequest::decode(opcode, payload) {
            Ok(request) => self.handle_request(request),
            Err(DhcpCodecError::UnsupportedOpcode) => self.response(DhcpStatus::Unsupported, 0),
            Err(DhcpCodecError::InvalidDevice) => self.response(DhcpStatus::InvalidDevice, 0),
            Err(DhcpCodecError::Malformed | DhcpCodecError::InvalidLease) => {
                self.response(DhcpStatus::BadRequest, 0)
            }
        }
    }

    fn response(&self, status: DhcpStatus, request_id: u64) -> DhcpResponse {
        DhcpResponse {
            status,
            state: self.state,
            request_id,
            config: self.config,
            lease: self.lease,
            polls: self.polls,
        }
    }

    #[cfg(test)]
    fn inject_static_lease(&mut self, lease: DhcpLease) -> Result<(), DhcpStatus> {
        let Some(config) = self.config else {
            return Err(DhcpStatus::NotConfigured);
        };
        if !lease.is_valid()
            || lease.device_id != config.device_id
            || lease.generation != config.generation
        {
            return Err(DhcpStatus::InvalidDevice);
        }
        self.lease = Some(lease);
        Ok(())
    }
}

impl Default for DhcpService {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run() {
    yarm_user_rt::user_log!("DHCP_SRV_ENTRY");
    let mut service = DhcpService::new();
    yarm_user_rt::user_log!(
        "DHCP_READY mode=stub-no-network state={:?}",
        service.state()
    );

    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("DHCP_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("DHCP_RECV_LOOP cap={}", recv_cap);
    loop {
        // SAFETY: dhcp_srv owns its startup-provided service receive endpoint.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let response = service
                    .handle_wire_request(received.message.opcode, received.message.as_slice());
                let Some(reply_cap) = received.reply_cap else {
                    continue;
                };
                let Ok(payload) = response.encode() else {
                    continue;
                };
                if let Ok(reply) = yarm_user_rt::ipc::Message::with_header(
                    0,
                    received.message.opcode,
                    0,
                    None,
                    &payload,
                ) {
                    // SAFETY: the reply capability accompanied this received request.
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                }
            }
            Ok(None) => {}
            Err(error) => yarm_user_rt::user_log!("DHCP_RECV_ERR err={:?}", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::dhcp_abi::{DHCP_OP_GET_STATUS, DHCP_WIRE_LEN};

    const CONFIG: DhcpInterfaceConfig = DhcpInterfaceConfig {
        device_id: 7,
        owner_id: 9,
        generation: 3,
    };

    fn configure(service: &mut DhcpService) {
        assert_eq!(
            service
                .handle_request(DhcpRequest::ConfigureInterface {
                    request_id: 1,
                    config: CONFIG,
                })
                .status,
            DhcpStatus::Ok
        );
    }

    #[test]
    fn configure_start_and_stop_are_deterministic() {
        let mut service = DhcpService::new();
        configure(&mut service);
        assert_eq!(service.config(), Some(CONFIG));
        assert_eq!(
            service
                .handle_request(DhcpRequest::Start { request_id: 2 })
                .status,
            DhcpStatus::Ok
        );
        assert_eq!(service.state(), DhcpState::Running);
        assert_eq!(
            service
                .handle_request(DhcpRequest::Stop { request_id: 3 })
                .status,
            DhcpStatus::Ok
        );
        assert_eq!(service.state(), DhcpState::Stopped);
    }

    #[test]
    fn start_without_configuration_is_rejected() {
        assert_eq!(
            DhcpService::new()
                .handle_request(DhcpRequest::Start { request_id: 1 })
                .status,
            DhcpStatus::NotConfigured
        );
    }

    #[test]
    fn poll_and_get_lease_return_no_lease_without_injection() {
        let mut service = DhcpService::new();
        configure(&mut service);
        assert_eq!(
            service
                .handle_request(DhcpRequest::Start { request_id: 2 })
                .status,
            DhcpStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(DhcpRequest::Poll {
                    request_id: 3,
                    timeout_hint: 50,
                })
                .status,
            DhcpStatus::NoLease
        );
        assert_eq!(service.polls(), 1);
        assert_eq!(
            service
                .handle_request(DhcpRequest::GetLease { request_id: 4 })
                .status,
            DhcpStatus::NoLease
        );
    }

    #[test]
    fn clear_lease_is_stable() {
        let mut service = DhcpService::new();
        assert_eq!(
            service
                .handle_request(DhcpRequest::ClearLease { request_id: 1 })
                .status,
            DhcpStatus::Ok
        );
        configure(&mut service);
        let lease = DhcpLease {
            device_id: CONFIG.device_id,
            generation: CONFIG.generation,
            assigned_ipv4: u32::from_be_bytes([192, 0, 2, 10]),
            prefix_len: 24,
            gateway_ipv4: u32::from_be_bytes([192, 0, 2, 1]),
            dns_server_ipv4: u32::from_be_bytes([192, 0, 2, 53]),
            lease_seconds: 3600,
        };
        service.inject_static_lease(lease).unwrap();
        assert_eq!(service.lease(), Some(lease));
        assert_eq!(
            service
                .handle_request(DhcpRequest::ClearLease { request_id: 2 })
                .status,
            DhcpStatus::Ok
        );
        assert_eq!(service.lease(), None);
    }

    #[test]
    fn malformed_and_unsupported_wire_requests_are_stable() {
        let mut service = DhcpService::new();
        let mut malformed = [0u8; DHCP_WIRE_LEN];
        malformed[0] = 1;
        malformed[8] = 1;
        assert_eq!(
            service
                .handle_wire_request(DHCP_OP_GET_STATUS, &malformed)
                .status,
            DhcpStatus::BadRequest
        );
        assert_eq!(
            service
                .handle_wire_request(0xffff, &[0; DHCP_WIRE_LEN])
                .status,
            DhcpStatus::Unsupported
        );
    }
}
