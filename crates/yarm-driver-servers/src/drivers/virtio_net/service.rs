// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::netdev_abi::{
    NETDEV_FEATURE_ALL, NETDEV_PACKET_F_CHECKSUM_REQUESTED, NetdevCodecError, NetdevInfo,
    NetdevPacket, NetdevRequest, NetdevResponse, NetdevStatus,
};

pub const VIRTIO_NET_FAKE_DEVICE_ID: u32 = 2;
pub const VIRTIO_NET_FAKE_GENERATION: u32 = 1;
pub const VIRTIO_NET_FAKE_MTU: u16 = 1500;
pub const VIRTIO_NET_TX_QUEUE_CAPACITY: usize = 8;
pub const VIRTIO_NET_RX_QUEUE_CAPACITY: usize = 8;
pub const VIRTIO_NET_FAKE_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 2];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetStats {
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub tx_dropped: u64,
    pub rx_dropped: u64,
    pub bad_requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PacketQueue<const N: usize> {
    entries: [Option<NetdevPacket>; N],
    head: usize,
    len: usize,
}

impl<const N: usize> PacketQueue<N> {
    const fn new() -> Self {
        Self {
            entries: [None; N],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, packet: NetdevPacket) -> Result<(), ()> {
        if self.len == N {
            return Err(());
        }
        let index = (self.head + self.len) % N;
        self.entries[index] = Some(packet);
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<NetdevPacket> {
        if self.len == 0 {
            return None;
        }
        let packet = self.entries[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        packet
    }

    const fn len(&self) -> usize {
        self.len
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetService {
    link_up: bool,
    tx: PacketQueue<VIRTIO_NET_TX_QUEUE_CAPACITY>,
    rx: PacketQueue<VIRTIO_NET_RX_QUEUE_CAPACITY>,
    stats: VirtioNetStats,
}

impl VirtioNetService {
    pub const fn new() -> Self {
        Self {
            link_up: true,
            tx: PacketQueue::new(),
            rx: PacketQueue::new(),
            stats: VirtioNetStats {
                tx_packets: 0,
                rx_packets: 0,
                tx_dropped: 0,
                rx_dropped: 0,
                bad_requests: 0,
            },
        }
    }

    pub const fn info(&self) -> NetdevInfo {
        NetdevInfo {
            device_id: VIRTIO_NET_FAKE_DEVICE_ID,
            generation: VIRTIO_NET_FAKE_GENERATION,
            mac: VIRTIO_NET_FAKE_MAC,
            mtu: VIRTIO_NET_FAKE_MTU,
            features: NETDEV_FEATURE_ALL,
            tx_capacity: VIRTIO_NET_TX_QUEUE_CAPACITY as u16,
            rx_capacity: VIRTIO_NET_RX_QUEUE_CAPACITY as u16,
            link_up: self.link_up,
        }
    }

    pub const fn stats(&self) -> VirtioNetStats {
        self.stats
    }

    pub fn handle_request(&mut self, request: NetdevRequest) -> NetdevResponse {
        match request {
            NetdevRequest::GetInfo => {
                let mut response = self.response(NetdevStatus::Ok);
                response.info = Some(self.info());
                response
            }
            NetdevRequest::GetStatus => {
                let mut response = self.response(NetdevStatus::Ok);
                response.info = Some(self.info());
                response
            }
            NetdevRequest::SetLinkState { link_up } => {
                self.link_up = link_up;
                let mut response = self.response(NetdevStatus::Ok);
                response.info = Some(self.info());
                response
            }
            NetdevRequest::TxInline { packet } => self.tx_inline(packet),
            NetdevRequest::RxInline => self.rx_inline(),
            NetdevRequest::InjectRxTest { packet } => self.inject_rx_test(packet),
            NetdevRequest::DrainTxTest => self.drain_tx_test(),
            NetdevRequest::ClearStats => {
                self.stats = VirtioNetStats {
                    tx_packets: 0,
                    rx_packets: 0,
                    tx_dropped: 0,
                    rx_dropped: 0,
                    bad_requests: 0,
                };
                self.response(NetdevStatus::Ok)
            }
        }
    }

    pub fn handle_wire_request(&mut self, opcode: u16, payload: &[u8]) -> NetdevResponse {
        match NetdevRequest::decode(opcode, payload) {
            Ok(request) => self.handle_request(request),
            Err(NetdevCodecError::UnsupportedOpcode) => {
                self.stats.bad_requests = self.stats.bad_requests.saturating_add(1);
                self.response(NetdevStatus::Unsupported)
            }
            Err(NetdevCodecError::MessageTooLarge) => {
                self.stats.bad_requests = self.stats.bad_requests.saturating_add(1);
                self.response(NetdevStatus::MessageTooLarge)
            }
            Err(NetdevCodecError::UnsupportedFlags) => {
                self.stats.bad_requests = self.stats.bad_requests.saturating_add(1);
                self.response(NetdevStatus::Unsupported)
            }
            Err(_) => {
                self.stats.bad_requests = self.stats.bad_requests.saturating_add(1);
                self.response(NetdevStatus::BadRequest)
            }
        }
    }

    fn tx_inline(&mut self, packet: NetdevPacket) -> NetdevResponse {
        if !self.link_up {
            return self.response(NetdevStatus::LinkDown);
        }
        if packet.flags & NETDEV_PACKET_F_CHECKSUM_REQUESTED != 0 {
            return self.response(NetdevStatus::ChecksumUnsupported);
        }
        if self.tx.push(packet).is_err() {
            self.stats.tx_dropped = self.stats.tx_dropped.saturating_add(1);
            return self.response(NetdevStatus::TableFull);
        }
        self.stats.tx_packets = self.stats.tx_packets.saturating_add(1);
        self.response(NetdevStatus::Ok)
    }

    fn drain_tx_test(&mut self) -> NetdevResponse {
        let Some(packet) = self.tx.pop() else {
            return self.response(NetdevStatus::Empty);
        };
        self.packet_response(packet)
    }

    fn inject_rx_test(&mut self, packet: NetdevPacket) -> NetdevResponse {
        if packet.flags & NETDEV_PACKET_F_CHECKSUM_REQUESTED != 0 {
            return self.response(NetdevStatus::ChecksumUnsupported);
        }
        if self.rx.push(packet).is_err() {
            self.stats.rx_dropped = self.stats.rx_dropped.saturating_add(1);
            return self.response(NetdevStatus::TableFull);
        }
        self.response(NetdevStatus::Ok)
    }

    fn rx_inline(&mut self) -> NetdevResponse {
        let Some(packet) = self.rx.pop() else {
            return self.response(NetdevStatus::Empty);
        };
        self.stats.rx_packets = self.stats.rx_packets.saturating_add(1);
        self.packet_response(packet)
    }

    fn packet_response(&self, packet: NetdevPacket) -> NetdevResponse {
        NetdevResponse {
            status: NetdevStatus::Ok,
            info: None,
            packet: Some(packet),
            tx_depth: self.tx.len() as u16,
            rx_depth: self.rx.len() as u16,
            tx_packets: 0,
            rx_packets: 0,
            tx_dropped: 0,
            rx_dropped: 0,
            bad_requests: 0,
        }
    }

    fn response(&self, status: NetdevStatus) -> NetdevResponse {
        NetdevResponse {
            status,
            info: None,
            packet: None,
            tx_depth: self.tx.len() as u16,
            rx_depth: self.rx.len() as u16,
            tx_packets: self.stats.tx_packets,
            rx_packets: self.stats.rx_packets,
            tx_dropped: self.stats.tx_dropped,
            rx_dropped: self.stats.rx_dropped,
            bad_requests: self.stats.bad_requests,
        }
    }
}

impl Default for VirtioNetService {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run() {
    yarm_user_rt::user_log!("VIRTIO_NET_SRV_ENTRY");
    let mut service = VirtioNetService::new();
    yarm_user_rt::user_log!(
        "VIRTIO_NET_READY mode=fake tx_capacity={} rx_capacity={} mtu={}",
        VIRTIO_NET_TX_QUEUE_CAPACITY,
        VIRTIO_NET_RX_QUEUE_CAPACITY,
        VIRTIO_NET_FAKE_MTU
    );

    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("VIRTIO_NET_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("VIRTIO_NET_RECV_LOOP cap={}", recv_cap);
    loop {
        // SAFETY: virtio_net_srv owns its startup-provided service receive endpoint.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let response = service
                    .handle_wire_request(received.message.opcode, received.message.as_slice());
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
            Err(error) => yarm_user_rt::user_log!("VIRTIO_NET_RECV_ERR err={:?}", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::netdev_abi::{
        NETDEV_ETHERTYPE_IPV4, NETDEV_MAX_INLINE_PACKET, NETDEV_OP_GET_INFO, NETDEV_PACKET_F_NONE,
        NETDEV_WIRE_LEN,
    };

    fn packet(id: u32, bytes: &[u8]) -> NetdevPacket {
        NetdevPacket::new(id, NETDEV_PACKET_F_NONE, NETDEV_ETHERTYPE_IPV4, bytes).expect("packet")
    }

    #[test]
    fn virtio_net_get_info_returns_fake_device_metadata() {
        let mut service = VirtioNetService::new();
        let response = service.handle_request(NetdevRequest::GetInfo);
        assert_eq!(response.status, NetdevStatus::Ok);
        assert_eq!(response.info, Some(service.info()));
        let info = response.info.expect("info");
        assert_eq!(info.mac, VIRTIO_NET_FAKE_MAC);
        assert_eq!(info.mtu, VIRTIO_NET_FAKE_MTU);
        assert_eq!(info.tx_capacity, VIRTIO_NET_TX_QUEUE_CAPACITY as u16);
        assert_eq!(info.rx_capacity, VIRTIO_NET_RX_QUEUE_CAPACITY as u16);
        let status = service.handle_request(NetdevRequest::GetStatus);
        assert_eq!(status.info.map(|info| info.link_up), Some(true));
    }

    #[test]
    fn virtio_net_link_state_blocks_and_permits_tx() {
        let mut service = VirtioNetService::new();
        assert_eq!(
            service
                .handle_request(NetdevRequest::SetLinkState { link_up: false })
                .status,
            NetdevStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(NetdevRequest::TxInline {
                    packet: packet(1, b"down"),
                })
                .status,
            NetdevStatus::LinkDown
        );
        service.handle_request(NetdevRequest::SetLinkState { link_up: true });
        assert_eq!(
            service
                .handle_request(NetdevRequest::TxInline {
                    packet: packet(2, b"up"),
                })
                .status,
            NetdevStatus::Ok
        );
    }

    #[test]
    fn virtio_net_tx_queue_capacity_and_drain_are_exact() {
        let mut service = VirtioNetService::new();
        for index in 0..VIRTIO_NET_TX_QUEUE_CAPACITY {
            assert_eq!(
                service
                    .handle_request(NetdevRequest::TxInline {
                        packet: packet(index as u32 + 1, &[index as u8, 0xaa]),
                    })
                    .status,
                NetdevStatus::Ok
            );
        }
        assert_eq!(
            service
                .handle_request(NetdevRequest::TxInline {
                    packet: packet(100, b"full"),
                })
                .status,
            NetdevStatus::TableFull
        );
        let drained = service.handle_request(NetdevRequest::DrainTxTest);
        assert_eq!(drained.status, NetdevStatus::Ok);
        let packet = drained.packet.expect("packet");
        assert_eq!(packet.packet_id, 1);
        assert_eq!(packet.bytes(), &[0, 0xaa]);
        assert_eq!(drained.tx_depth, (VIRTIO_NET_TX_QUEUE_CAPACITY - 1) as u16);
    }

    #[test]
    fn virtio_net_rx_empty_inject_and_receive_are_exact() {
        let mut service = VirtioNetService::new();
        assert_eq!(
            service.handle_request(NetdevRequest::RxInline).status,
            NetdevStatus::Empty
        );
        let expected = packet(7, b"receive");
        assert_eq!(
            service
                .handle_request(NetdevRequest::InjectRxTest { packet: expected })
                .status,
            NetdevStatus::Ok
        );
        let received = service.handle_request(NetdevRequest::RxInline);
        assert_eq!(received.status, NetdevStatus::Ok);
        assert_eq!(received.packet, Some(expected));
        assert_eq!(received.rx_depth, 0);
    }

    #[test]
    fn virtio_net_rx_queue_capacity_drops_deterministically() {
        let mut service = VirtioNetService::new();
        for index in 0..VIRTIO_NET_RX_QUEUE_CAPACITY {
            assert_eq!(
                service
                    .handle_request(NetdevRequest::InjectRxTest {
                        packet: packet(index as u32 + 1, &[index as u8]),
                    })
                    .status,
                NetdevStatus::Ok
            );
        }
        assert_eq!(
            service
                .handle_request(NetdevRequest::InjectRxTest {
                    packet: packet(100, b"full"),
                })
                .status,
            NetdevStatus::TableFull
        );
        assert_eq!(service.stats().rx_dropped, 1);
    }

    #[test]
    fn virtio_net_stats_and_clear_are_deterministic() {
        let mut service = VirtioNetService::new();
        service.handle_request(NetdevRequest::TxInline {
            packet: packet(1, b"tx"),
        });
        service.handle_request(NetdevRequest::InjectRxTest {
            packet: packet(2, b"rx"),
        });
        service.handle_request(NetdevRequest::RxInline);
        let mut malformed = [0u8; NETDEV_WIRE_LEN];
        malformed[127] = 1;
        service.handle_wire_request(NETDEV_OP_GET_INFO, &malformed);
        assert_eq!(
            service.stats(),
            VirtioNetStats {
                tx_packets: 1,
                rx_packets: 1,
                tx_dropped: 0,
                rx_dropped: 0,
                bad_requests: 1,
            }
        );
        let cleared = service.handle_request(NetdevRequest::ClearStats);
        assert_eq!(cleared.status, NetdevStatus::Ok);
        assert_eq!(
            service.stats(),
            VirtioNetStats {
                tx_packets: 0,
                rx_packets: 0,
                tx_dropped: 0,
                rx_dropped: 0,
                bad_requests: 0,
            }
        );
    }

    #[test]
    fn virtio_net_wire_errors_and_checksum_requests_are_rejected() {
        let mut service = VirtioNetService::new();
        let mut malformed = [0u8; NETDEV_WIRE_LEN];
        malformed[127] = 1;
        assert_eq!(
            service
                .handle_wire_request(NETDEV_OP_GET_INFO, &malformed)
                .status,
            NetdevStatus::BadRequest
        );
        assert_eq!(
            service
                .handle_wire_request(0xffff, &[0; NETDEV_WIRE_LEN])
                .status,
            NetdevStatus::Unsupported
        );
        let checksum = NetdevPacket::new(
            9,
            NETDEV_PACKET_F_CHECKSUM_REQUESTED,
            NETDEV_ETHERTYPE_IPV4,
            b"no checksum",
        )
        .expect("packet metadata is valid");
        assert_eq!(
            service
                .handle_request(NetdevRequest::TxInline { packet: checksum })
                .status,
            NetdevStatus::ChecksumUnsupported
        );
        assert_eq!(NETDEV_MAX_INLINE_PACKET, 96);
    }
}
