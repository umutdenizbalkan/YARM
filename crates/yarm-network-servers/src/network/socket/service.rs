// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::socket_abi::{
    SOCKET_AF_INET, SOCKET_MAX_INLINE_DATA, SOCKET_PROTOCOL_DEFAULT, SOCKET_PROTOCOL_UDP,
    SOCKET_TYPE_DGRAM, SocketCodecError, SocketEndpoint, SocketRequest, SocketResponse,
    SocketShutdown, SocketState, SocketStatus,
};

pub const MAX_SOCKETS: usize = 64;
pub const MAX_PENDING_DATAGRAMS: usize = 1;
pub const MAX_DGRAM_PAYLOAD: usize = SOCKET_MAX_INLINE_DATA;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingDatagram {
    len: u16,
    source: Option<SocketEndpoint>,
    data: [u8; MAX_DGRAM_PAYLOAD],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketEntry {
    handle: u32,
    state: SocketState,
    domain: u16,
    socket_type: u16,
    protocol: u16,
    local: Option<SocketEndpoint>,
    remote: Option<SocketEndpoint>,
    pending: Option<PendingDatagram>,
    read_shutdown: bool,
    write_shutdown: bool,
}

impl SocketEntry {
    const EMPTY: Self = Self {
        handle: 0,
        state: SocketState::Empty,
        domain: 0,
        socket_type: 0,
        protocol: 0,
        local: None,
        remote: None,
        pending: None,
        read_shutdown: false,
        write_shutdown: false,
    };

    const fn response(self, status: SocketStatus) -> SocketResponse {
        SocketResponse {
            status,
            handle: self.handle,
            state: self.state,
            value: 0,
            endpoint: self.local,
            data_len: 0,
            data: [0; SOCKET_MAX_INLINE_DATA],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketService {
    entries: [SocketEntry; MAX_SOCKETS],
    next_handle: u32,
}

impl SocketService {
    pub const fn new() -> Self {
        Self {
            entries: [SocketEntry::EMPTY; MAX_SOCKETS],
            next_handle: 1,
        }
    }

    pub fn socket_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| !matches!(entry.state, SocketState::Empty | SocketState::Closed))
            .count()
    }

    pub fn handle_request(&mut self, request: SocketRequest) -> SocketResponse {
        match request {
            SocketRequest::Create {
                domain,
                socket_type,
                protocol,
            } => self.create(domain, socket_type, protocol),
            SocketRequest::Close { handle } => self.close(handle),
            SocketRequest::Bind { handle, endpoint } => self.bind(handle, endpoint),
            SocketRequest::Listen { .. } | SocketRequest::Accept { .. } => {
                SocketResponse::status(SocketStatus::Unsupported)
            }
            SocketRequest::Connect { handle, endpoint } => self.connect(handle, endpoint),
            SocketRequest::Send { handle, len, data } => self.send(handle, len, data),
            SocketRequest::Recv { handle, max_len } => self.recv(handle, max_len),
            SocketRequest::Shutdown { handle, how } => self.shutdown(handle, how),
            SocketRequest::GetStatus { handle } => self.get_status(handle),
        }
    }

    pub fn handle_wire_request(&mut self, opcode: u16, payload: &[u8]) -> SocketResponse {
        match SocketRequest::decode(opcode, payload) {
            Ok(request) => self.handle_request(request),
            Err(SocketCodecError::UnsupportedOpcode) => {
                SocketResponse::status(SocketStatus::Unsupported)
            }
            Err(SocketCodecError::MessageTooLarge) => {
                SocketResponse::status(SocketStatus::MessageTooLarge)
            }
            Err(_) => SocketResponse::status(SocketStatus::BadRequest),
        }
    }

    fn create(&mut self, domain: u16, socket_type: u16, protocol: u16) -> SocketResponse {
        if domain != SOCKET_AF_INET
            || socket_type != SOCKET_TYPE_DGRAM
            || !matches!(protocol, SOCKET_PROTOCOL_DEFAULT | SOCKET_PROTOCOL_UDP)
        {
            return SocketResponse::status(SocketStatus::Unsupported);
        }
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| matches!(entry.state, SocketState::Empty | SocketState::Closed))
        else {
            return SocketResponse::status(SocketStatus::TableFull);
        };
        let handle = self.allocate_handle();
        self.entries[index] = SocketEntry {
            handle,
            state: SocketState::Created,
            domain,
            socket_type,
            protocol,
            ..SocketEntry::EMPTY
        };
        self.entries[index].response(SocketStatus::Ok)
    }

    fn close(&mut self, handle: u32) -> SocketResponse {
        let Some(index) = self.entry_index(handle) else {
            return SocketResponse::status(SocketStatus::NotFound);
        };
        if self.entries[index].state == SocketState::Closed {
            return self.entries[index].response(SocketStatus::Closed);
        }
        self.entries[index].state = SocketState::Closed;
        self.entries[index].local = None;
        self.entries[index].remote = None;
        self.entries[index].pending = None;
        self.entries[index].read_shutdown = true;
        self.entries[index].write_shutdown = true;
        self.entries[index].response(SocketStatus::Ok)
    }

    fn bind(&mut self, handle: u32, endpoint: SocketEndpoint) -> SocketResponse {
        if !endpoint.is_valid_loopback() {
            return SocketResponse::status(SocketStatus::BadRequest);
        }
        let Some(index) = self.entry_index(handle) else {
            return SocketResponse::status(SocketStatus::NotFound);
        };
        if self.entries.iter().any(|entry| {
            !matches!(entry.state, SocketState::Empty | SocketState::Closed)
                && entry.local == Some(endpoint)
        }) {
            return SocketResponse::status(SocketStatus::AlreadyBound);
        }
        match self.entries[index].state {
            SocketState::Created => {
                self.entries[index].local = Some(endpoint);
                self.entries[index].state = SocketState::Bound;
                self.entries[index].response(SocketStatus::Ok)
            }
            SocketState::Bound | SocketState::Connected => {
                self.entries[index].response(SocketStatus::AlreadyBound)
            }
            SocketState::Closed => self.entries[index].response(SocketStatus::Closed),
            _ => self.entries[index].response(SocketStatus::InvalidState),
        }
    }

    fn connect(&mut self, handle: u32, endpoint: SocketEndpoint) -> SocketResponse {
        let destination_exists = self.entries.iter().any(|entry| {
            !matches!(entry.state, SocketState::Empty | SocketState::Closed)
                && entry.local == Some(endpoint)
        });
        if !destination_exists {
            return SocketResponse::status(SocketStatus::NotFound);
        }
        let Some(index) = self.entry_index(handle) else {
            return SocketResponse::status(SocketStatus::NotFound);
        };
        match self.entries[index].state {
            SocketState::Created | SocketState::Bound => {
                self.entries[index].remote = Some(endpoint);
                self.entries[index].state = SocketState::Connected;
                let mut response = self.entries[index].response(SocketStatus::Ok);
                response.endpoint = Some(endpoint);
                response
            }
            SocketState::Connected => self.entries[index].response(SocketStatus::InvalidState),
            SocketState::Closed => self.entries[index].response(SocketStatus::Closed),
            _ => self.entries[index].response(SocketStatus::InvalidState),
        }
    }

    fn send(
        &mut self,
        handle: u32,
        len: u16,
        data: [u8; SOCKET_MAX_INLINE_DATA],
    ) -> SocketResponse {
        if usize::from(len) > MAX_DGRAM_PAYLOAD {
            return SocketResponse::status(SocketStatus::MessageTooLarge);
        }
        let Some(sender_index) = self.entry_index(handle) else {
            return SocketResponse::status(SocketStatus::NotFound);
        };
        let sender = self.entries[sender_index];
        if sender.state == SocketState::Closed || sender.write_shutdown {
            return sender.response(SocketStatus::Closed);
        }
        if sender.state != SocketState::Connected {
            return sender.response(SocketStatus::NotConnected);
        }
        let Some(remote) = sender.remote else {
            return sender.response(SocketStatus::NotConnected);
        };
        let Some(receiver_index) = self.entries.iter().position(|entry| {
            !matches!(entry.state, SocketState::Empty | SocketState::Closed)
                && entry.local == Some(remote)
        }) else {
            return sender.response(SocketStatus::NotFound);
        };
        if self.entries[receiver_index].read_shutdown {
            return sender.response(SocketStatus::Closed);
        }
        if self.entries[receiver_index].pending.is_some() {
            return sender.response(SocketStatus::WouldBlock);
        }
        self.entries[receiver_index].pending = Some(PendingDatagram {
            len,
            source: sender.local,
            data,
        });
        let mut response = sender.response(SocketStatus::Ok);
        response.value = u32::from(len);
        response.endpoint = Some(remote);
        response
    }

    fn recv(&mut self, handle: u32, max_len: u16) -> SocketResponse {
        let Some(index) = self.entry_index(handle) else {
            return SocketResponse::status(SocketStatus::NotFound);
        };
        let entry = self.entries[index];
        if entry.state == SocketState::Closed || entry.read_shutdown {
            return entry.response(SocketStatus::Closed);
        }
        if entry.local.is_none() {
            return entry.response(SocketStatus::NotBound);
        }
        let Some(datagram) = self.entries[index].pending.take() else {
            return entry.response(SocketStatus::WouldBlock);
        };
        let copy_len = core::cmp::min(datagram.len, max_len);
        let mut response = self.entries[index].response(SocketStatus::Ok);
        response.value = u32::from(datagram.len);
        response.endpoint = datagram.source;
        response.data_len = copy_len;
        response.data[..usize::from(copy_len)]
            .copy_from_slice(&datagram.data[..usize::from(copy_len)]);
        response
    }

    fn shutdown(&mut self, handle: u32, how: SocketShutdown) -> SocketResponse {
        let Some(index) = self.entry_index(handle) else {
            return SocketResponse::status(SocketStatus::NotFound);
        };
        if self.entries[index].state == SocketState::Closed {
            return self.entries[index].response(SocketStatus::Closed);
        }
        match how {
            SocketShutdown::Read => self.entries[index].read_shutdown = true,
            SocketShutdown::Write => self.entries[index].write_shutdown = true,
            SocketShutdown::Both => {
                self.entries[index].read_shutdown = true;
                self.entries[index].write_shutdown = true;
            }
        }
        self.entries[index].response(SocketStatus::Ok)
    }

    fn get_status(&self, handle: u32) -> SocketResponse {
        self.entry_index(handle)
            .map(|index| self.entries[index].response(SocketStatus::Ok))
            .unwrap_or_else(|| SocketResponse::status(SocketStatus::NotFound))
    }

    fn entry_index(&self, handle: u32) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.handle == handle && entry.state != SocketState::Empty)
    }

    fn allocate_handle(&mut self) -> u32 {
        loop {
            let handle = self.next_handle;
            self.next_handle = self.next_handle.wrapping_add(1).max(1);
            if handle != 0 && self.entry_index(handle).is_none() {
                return handle;
            }
        }
    }
}

impl Default for SocketService {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run() {
    yarm_user_rt::user_log!("SOCKET_SRV_ENTRY");
    let mut service = SocketService::new();
    yarm_user_rt::user_log!(
        "SOCKET_SRV_READY profile=dgram-loopback capacity={}",
        MAX_SOCKETS
    );

    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("SOCKET_SRV_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("SOCKET_SRV_RECV_LOOP cap={}", recv_cap);
    loop {
        // SAFETY: socket_srv owns its startup-provided service receive endpoint.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let response = service
                    .handle_wire_request(received.message.opcode, received.message.as_slice());
                if response.status == SocketStatus::Unsupported {
                    yarm_user_rt::user_log!(
                        "SOCKET_SRV_UNSUPPORTED_OPCODE opcode={}",
                        received.message.opcode
                    );
                }
                let Some(reply_cap) = received.reply_cap else {
                    continue;
                };
                let Ok(encoded) = response.encode() else {
                    continue;
                };
                if let Ok(reply) = yarm_user_rt::ipc::Message::with_header(
                    0,
                    received.message.opcode,
                    0,
                    None,
                    &encoded,
                ) {
                    // SAFETY: the reply capability accompanied this received request.
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                }
            }
            Ok(None) => {}
            Err(error) => yarm_user_rt::user_log!("SOCKET_SRV_RECV_ERR err={:?}", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::socket_abi::{SOCKET_OP_CREATE, SOCKET_WIRE_LEN};

    fn create(service: &mut SocketService) -> u32 {
        let response = service.handle_request(SocketRequest::Create {
            domain: SOCKET_AF_INET,
            socket_type: SOCKET_TYPE_DGRAM,
            protocol: SOCKET_PROTOCOL_UDP,
        });
        assert_eq!(response.status, SocketStatus::Ok);
        response.handle
    }

    fn bind(service: &mut SocketService, handle: u32, port: u16) {
        assert_eq!(
            service
                .handle_request(SocketRequest::Bind {
                    handle,
                    endpoint: SocketEndpoint::loopback(port),
                })
                .status,
            SocketStatus::Ok
        );
    }

    #[test]
    fn socket_create_and_close_transitions_state() {
        let mut service = SocketService::new();
        let handle = create(&mut service);
        assert_eq!(service.socket_count(), 1);
        let closed = service.handle_request(SocketRequest::Close { handle });
        assert_eq!(closed.status, SocketStatus::Ok);
        assert_eq!(closed.state, SocketState::Closed);
        assert_eq!(service.socket_count(), 0);
        assert_eq!(
            service
                .handle_request(SocketRequest::Close { handle })
                .status,
            SocketStatus::Closed
        );
    }

    #[test]
    fn socket_table_enforces_capacity_and_reuses_closed_slots() {
        let mut service = SocketService::new();
        let mut first = 0;
        for index in 0..MAX_SOCKETS {
            let handle = create(&mut service);
            if index == 0 {
                first = handle;
            }
        }
        assert_eq!(
            service
                .handle_request(SocketRequest::Create {
                    domain: SOCKET_AF_INET,
                    socket_type: SOCKET_TYPE_DGRAM,
                    protocol: SOCKET_PROTOCOL_DEFAULT,
                })
                .status,
            SocketStatus::TableFull
        );
        assert_eq!(
            service
                .handle_request(SocketRequest::Close { handle: first })
                .status,
            SocketStatus::Ok
        );
        assert_ne!(create(&mut service), first);
    }

    #[test]
    fn socket_bind_and_duplicate_bind_are_checked() {
        let mut service = SocketService::new();
        let first = create(&mut service);
        let second = create(&mut service);
        bind(&mut service, first, 9000);
        assert_eq!(
            service
                .handle_request(SocketRequest::Bind {
                    handle: second,
                    endpoint: SocketEndpoint::loopback(9000),
                })
                .status,
            SocketStatus::AlreadyBound
        );
    }

    #[test]
    fn datagram_loopback_connect_send_and_recv() {
        let mut service = SocketService::new();
        let receiver = create(&mut service);
        let sender = create(&mut service);
        bind(&mut service, receiver, 9001);
        bind(&mut service, sender, 9002);
        assert_eq!(
            service
                .handle_request(SocketRequest::Connect {
                    handle: sender,
                    endpoint: SocketEndpoint::loopback(9001),
                })
                .status,
            SocketStatus::Ok
        );
        let mut data = [0; SOCKET_MAX_INLINE_DATA];
        data[..4].copy_from_slice(b"ping");
        let sent = service.handle_request(SocketRequest::Send {
            handle: sender,
            len: 4,
            data,
        });
        assert_eq!(sent.status, SocketStatus::Ok);
        assert_eq!(sent.value, 4);
        let received = service.handle_request(SocketRequest::Recv {
            handle: receiver,
            max_len: SOCKET_MAX_INLINE_DATA as u16,
        });
        assert_eq!(received.status, SocketStatus::Ok);
        assert_eq!(received.data_len, 4);
        assert_eq!(&received.data[..4], b"ping");
        assert_eq!(received.endpoint, Some(SocketEndpoint::loopback(9002)));
    }

    #[test]
    fn datagram_recv_empty_would_block() {
        let mut service = SocketService::new();
        let receiver = create(&mut service);
        bind(&mut service, receiver, 9003);
        assert_eq!(
            service
                .handle_request(SocketRequest::Recv {
                    handle: receiver,
                    max_len: SOCKET_MAX_INLINE_DATA as u16,
                })
                .status,
            SocketStatus::WouldBlock
        );
    }

    #[test]
    fn datagram_send_to_closed_destination_is_rejected() {
        let mut service = SocketService::new();
        let receiver = create(&mut service);
        let sender = create(&mut service);
        bind(&mut service, receiver, 9004);
        assert_eq!(
            service
                .handle_request(SocketRequest::Connect {
                    handle: sender,
                    endpoint: SocketEndpoint::loopback(9004),
                })
                .status,
            SocketStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(SocketRequest::Close { handle: receiver })
                .status,
            SocketStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(SocketRequest::Send {
                    handle: sender,
                    len: 0,
                    data: [0; SOCKET_MAX_INLINE_DATA],
                })
                .status,
            SocketStatus::NotFound
        );
    }

    #[test]
    fn socket_shutdown_disables_requested_direction() {
        let mut service = SocketService::new();
        let receiver = create(&mut service);
        bind(&mut service, receiver, 9005);
        assert_eq!(
            service
                .handle_request(SocketRequest::Shutdown {
                    handle: receiver,
                    how: SocketShutdown::Read,
                })
                .status,
            SocketStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(SocketRequest::Recv {
                    handle: receiver,
                    max_len: 1,
                })
                .status,
            SocketStatus::Closed
        );
    }

    #[test]
    fn stream_profile_and_unknown_opcode_are_unsupported() {
        let mut service = SocketService::new();
        assert_eq!(
            service
                .handle_request(SocketRequest::Create {
                    domain: SOCKET_AF_INET,
                    socket_type: yarm_ipc_abi::socket_abi::SOCKET_TYPE_STREAM,
                    protocol: yarm_ipc_abi::socket_abi::SOCKET_PROTOCOL_TCP,
                })
                .status,
            SocketStatus::Unsupported
        );
        assert_eq!(
            service
                .handle_wire_request(0xffff, &[0; SOCKET_WIRE_LEN])
                .status,
            SocketStatus::Unsupported
        );
        let mut malformed = [0; SOCKET_WIRE_LEN];
        malformed[127] = 1;
        assert_eq!(
            service
                .handle_wire_request(SOCKET_OP_CREATE, &malformed)
                .status,
            SocketStatus::BadRequest
        );
    }
}
