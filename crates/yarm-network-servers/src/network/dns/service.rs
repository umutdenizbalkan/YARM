// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::dns_abi::{DnsCodecError, DnsRequest, DnsResponse, DnsStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DnsService {
    server_ipv4: Option<u32>,
    queries: u64,
}

impl DnsService {
    pub const fn new() -> Self {
        Self {
            server_ipv4: None,
            queries: 0,
        }
    }

    pub const fn server_ipv4(&self) -> Option<u32> {
        self.server_ipv4
    }

    pub const fn queries(&self) -> u64 {
        self.queries
    }

    pub fn handle_request(&mut self, request: DnsRequest) -> DnsResponse {
        match request {
            DnsRequest::GetStatus { request_id } => self.response(DnsStatus::Ok, request_id),
            DnsRequest::ConfigureServer {
                request_id,
                server_ipv4,
            } => {
                self.server_ipv4 = Some(server_ipv4);
                self.response(DnsStatus::Ok, request_id)
            }
            DnsRequest::ClearServer { request_id } => {
                self.server_ipv4 = None;
                self.response(DnsStatus::Ok, request_id)
            }
            DnsRequest::Query {
                request_id,
                kind: _,
                name: _,
            } => {
                if self.server_ipv4.is_none() {
                    self.response(DnsStatus::NotConfigured, request_id)
                } else {
                    self.queries = self.queries.saturating_add(1);
                    self.response(DnsStatus::NoAnswer, request_id)
                }
            }
            DnsRequest::ClearCache { request_id } => {
                self.response(DnsStatus::CacheEmpty, request_id)
            }
        }
    }

    pub fn handle_wire_request(&mut self, opcode: u16, payload: &[u8]) -> DnsResponse {
        match DnsRequest::decode(opcode, payload) {
            Ok(request) => self.handle_request(request),
            Err(DnsCodecError::UnsupportedOpcode) => self.response(DnsStatus::Unsupported, 0),
            Err(DnsCodecError::NameTooLong) => self.response(DnsStatus::NameTooLong, 0),
            Err(DnsCodecError::InvalidName) => self.response(DnsStatus::InvalidName, 0),
            Err(DnsCodecError::InvalidServer) => self.response(DnsStatus::InvalidServer, 0),
            Err(DnsCodecError::Malformed) => self.response(DnsStatus::BadRequest, 0),
        }
    }

    fn response(&self, status: DnsStatus, request_id: u64) -> DnsResponse {
        let mut response = DnsResponse::status(status, request_id);
        response.server_ipv4 = self.server_ipv4.unwrap_or(0);
        response.queries = self.queries;
        response
    }
}

impl Default for DnsService {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run() {
    yarm_user_rt::user_log!("DNS_SRV_ENTRY");
    let mut service = DnsService::new();
    yarm_user_rt::user_log!("DNS_READY mode=stub-no-network");

    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("DNS_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("DNS_RECV_LOOP cap={}", recv_cap);
    loop {
        // SAFETY: dns_srv owns its startup-provided service receive endpoint.
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
            Err(error) => yarm_user_rt::user_log!("DNS_RECV_ERR err={:?}", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::dns_abi::{DNS_OP_GET_STATUS, DNS_WIRE_LEN, DnsName, DnsQueryKind};

    const SERVER: u32 = u32::from_be_bytes([192, 0, 2, 53]);

    fn query(request_id: u64, name: &[u8]) -> DnsRequest {
        DnsRequest::Query {
            request_id,
            kind: DnsQueryKind::A,
            name: DnsName::new(name).unwrap(),
        }
    }

    #[test]
    fn query_without_server_is_not_configured() {
        let mut service = DnsService::new();
        assert_eq!(
            service.handle_request(query(1, b"example.test")).status,
            DnsStatus::NotConfigured
        );
        assert_eq!(service.queries(), 0);
    }

    #[test]
    fn configure_server_then_query_returns_no_answer() {
        let mut service = DnsService::new();
        assert_eq!(
            service
                .handle_request(DnsRequest::ConfigureServer {
                    request_id: 1,
                    server_ipv4: SERVER,
                })
                .status,
            DnsStatus::Ok
        );
        assert_eq!(service.server_ipv4(), Some(SERVER));
        assert_eq!(
            service.handle_request(query(2, b"example.test")).status,
            DnsStatus::NoAnswer
        );
        assert_eq!(service.queries(), 1);
    }

    #[test]
    fn invalid_server_and_names_are_rejected_by_wire_decoder() {
        let mut service = DnsService::new();
        let mut invalid_server = [0u8; DNS_WIRE_LEN];
        invalid_server[0] = 1;
        assert_eq!(
            service
                .handle_wire_request(
                    yarm_ipc_abi::dns_abi::DNS_OP_CONFIGURE_SERVER,
                    &invalid_server,
                )
                .status,
            DnsStatus::InvalidServer
        );

        let mut invalid_name = [0u8; DNS_WIRE_LEN];
        invalid_name[0] = 1;
        invalid_name[16] = 4;
        invalid_name[20..24].copy_from_slice(b"-bad");
        assert_eq!(
            service
                .handle_wire_request(yarm_ipc_abi::dns_abi::DNS_OP_QUERY_A, &invalid_name)
                .status,
            DnsStatus::InvalidName
        );
    }

    #[test]
    fn clear_server_and_cache_are_stable() {
        let mut service = DnsService::new();
        service.handle_request(DnsRequest::ConfigureServer {
            request_id: 1,
            server_ipv4: SERVER,
        });
        assert_eq!(
            service
                .handle_request(DnsRequest::ClearServer { request_id: 2 })
                .status,
            DnsStatus::Ok
        );
        assert_eq!(service.server_ipv4(), None);
        assert_eq!(
            service
                .handle_request(DnsRequest::ClearCache { request_id: 3 })
                .status,
            DnsStatus::CacheEmpty
        );
    }

    #[test]
    fn malformed_and_unsupported_wire_requests_are_stable() {
        let mut service = DnsService::new();
        let mut malformed = [0u8; DNS_WIRE_LEN];
        malformed[0] = 1;
        malformed[8] = 1;
        assert_eq!(
            service
                .handle_wire_request(DNS_OP_GET_STATUS, &malformed)
                .status,
            DnsStatus::BadRequest
        );
        assert_eq!(
            service
                .handle_wire_request(0xffff, &[0; DNS_WIRE_LEN])
                .status,
            DnsStatus::Unsupported
        );
    }
}
