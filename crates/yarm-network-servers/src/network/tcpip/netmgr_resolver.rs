// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::netmgr_abi::{NetmgrRequest, NetmgrResponse, NetmgrStatus};
use yarm_user_rt::ipc::Message;

use super::service::{ResolvedIpv4Route, RouteResolveError, TcpipRouteResolver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetmgrIpcExchangeError {
    Transport,
    NoReply,
}

pub trait NetmgrIpcTransport {
    fn exchange(&mut self, request: &Message) -> Result<Message, NetmgrIpcExchangeError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetmgrIpcEndpoint {
    request_send_cap: u32,
    reply_recv_cap: u32,
    timeout_ticks: u64,
}

impl NetmgrIpcEndpoint {
    pub const fn new(
        request_send_cap: u32,
        reply_recv_cap: u32,
        timeout_ticks: u64,
    ) -> Option<Self> {
        if request_send_cap == 0 || reply_recv_cap == 0 {
            None
        } else {
            Some(Self {
                request_send_cap,
                reply_recv_cap,
                timeout_ticks,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallNetmgrIpcTransport {
    endpoint: NetmgrIpcEndpoint,
}

impl SyscallNetmgrIpcTransport {
    pub const fn new(endpoint: NetmgrIpcEndpoint) -> Self {
        Self { endpoint }
    }
}

impl NetmgrIpcTransport for SyscallNetmgrIpcTransport {
    fn exchange(&mut self, request: &Message) -> Result<Message, NetmgrIpcExchangeError> {
        // SAFETY: construction of this transport is the caller's explicit assertion that both
        // capability IDs belong to tcpip_srv with SEND and RECV rights respectively.
        unsafe {
            yarm_user_rt::syscall::ipc_call(
                self.endpoint.request_send_cap,
                self.endpoint.reply_recv_cap,
                request,
            )
        }
        .map_err(|_| NetmgrIpcExchangeError::Transport)?;

        // SAFETY: the endpoint configuration grants tcpip_srv receive authority for replies.
        unsafe {
            yarm_user_rt::syscall::ipc_recv_with_deadline(
                self.endpoint.reply_recv_cap,
                self.endpoint.timeout_ticks,
            )
        }
        .map_err(|_| NetmgrIpcExchangeError::Transport)?
        .ok_or(NetmgrIpcExchangeError::NoReply)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetmgrIpcResolverError {
    RequestCodec,
    Message,
    Exchange(NetmgrIpcExchangeError),
    MalformedReply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetmgrIpcRouteResolver<T> {
    transport: T,
}

impl<T> NetmgrIpcRouteResolver<T> {
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn into_transport(self) -> T {
        self.transport
    }
}

impl<T: NetmgrIpcTransport> NetmgrIpcRouteResolver<T> {
    fn query(&mut self, request: NetmgrRequest) -> Result<NetmgrResponse, NetmgrIpcResolverError> {
        let (opcode, payload) = request
            .encode()
            .map_err(|_| NetmgrIpcResolverError::RequestCodec)?;
        let message = Message::with_header(0, opcode, 0, None, &payload)
            .map_err(|_| NetmgrIpcResolverError::Message)?;
        let reply = self
            .transport
            .exchange(&message)
            .map_err(NetmgrIpcResolverError::Exchange)?;
        if reply.opcode != opcode {
            return Err(NetmgrIpcResolverError::MalformedReply);
        }
        NetmgrResponse::decode(reply.as_slice()).map_err(|_| NetmgrIpcResolverError::MalformedReply)
    }

    fn query_route(&mut self, destination: u32) -> Result<ResolvedIpv4Route, RouteResolveError> {
        let response = self
            .query(NetmgrRequest::LookupRoute { destination })
            .map_err(|_| RouteResolveError::Unsupported)?;
        match response.status {
            NetmgrStatus::Ok => {}
            NetmgrStatus::NotFound if plain_status_response(response) => {
                return Err(RouteResolveError::NoRoute);
            }
            NetmgrStatus::LinkDown if plain_status_response(response) => {
                return Err(RouteResolveError::LinkDown);
            }
            NetmgrStatus::Unsupported if plain_status_response(response) => {
                return Err(RouteResolveError::Unsupported);
            }
            _ => return Err(RouteResolveError::Unsupported),
        }
        let route = response
            .route
            .filter(|_| {
                response.device.is_none()
                    && response.address.is_none()
                    && response.value == 0
                    && response.auxiliary == 0
            })
            .ok_or(RouteResolveError::Unsupported)?;
        let device_response = self
            .query(NetmgrRequest::GetDevice {
                device_id: route.device_id,
            })
            .map_err(|_| RouteResolveError::Unsupported)?;
        match device_response.status {
            NetmgrStatus::Ok => {}
            NetmgrStatus::NotFound if plain_status_response(device_response) => {
                return Err(RouteResolveError::NoRoute);
            }
            NetmgrStatus::LinkDown if plain_status_response(device_response) => {
                return Err(RouteResolveError::LinkDown);
            }
            _ => return Err(RouteResolveError::Unsupported),
        }
        let device = device_response
            .device
            .filter(|device| {
                device.device_id == route.device_id
                    && device.owner_id == route.owner_id
                    && device.generation == route.generation
                    && device_response.address.is_none()
                    && device_response.route.is_none()
                    && device_response.value == 0
                    && device_response.auxiliary == 0
            })
            .ok_or(RouteResolveError::Unsupported)?;
        if !device.link_up {
            return Err(RouteResolveError::LinkDown);
        }
        Ok(ResolvedIpv4Route {
            route_id: route.route_id,
            device_id: route.device_id,
            gateway: route.gateway,
            mtu: u32::from(device.mtu),
        })
    }
}

const fn plain_status_response(response: NetmgrResponse) -> bool {
    response.device.is_none()
        && response.address.is_none()
        && response.route.is_none()
        && response.value == 0
        && response.auxiliary == 0
}

impl<T: NetmgrIpcTransport> TcpipRouteResolver for NetmgrIpcRouteResolver<T> {
    fn lookup_ipv4_route(
        &mut self,
        destination: u32,
    ) -> Result<ResolvedIpv4Route, RouteResolveError> {
        self.query_route(destination)
    }

    fn first_ipv4_address(&mut self, device_id: u32) -> Option<u32> {
        let response = self
            .query(NetmgrRequest::GetFirstIpv4AddressForDevice { device_id })
            .ok()?;
        if response.status != NetmgrStatus::Ok {
            return None;
        }
        response
            .address
            .filter(|address| {
                address.device_id == device_id
                    && response.device.is_none()
                    && response.route.is_none()
                    && response.value == 0
                    && response.auxiliary == 0
            })
            .map(|address| address.address)
    }

    fn has_ipv4_address(&mut self, device_id: u32, address: u32) -> bool {
        let Ok(response) =
            self.query(NetmgrRequest::CheckIpv4AddressOnDevice { device_id, address })
        else {
            return false;
        };
        response.status == NetmgrStatus::Ok
            && response.device.is_none()
            && response.route.is_none()
            && response.value == 0
            && response.auxiliary == 0
            && response
                .address
                .map(|record| record.device_id == device_id && record.address == address)
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::netmgr::service::NetmgrService;
    use crate::network::tcpip::service::TcpipService;
    use yarm_ipc_abi::netmgr_abi::{
        Ipv4Address, Ipv4Route, NET_DEVICE_FLAG_BROADCAST, NETMGR_DEVICE_ID_LOOPBACK,
        NETMGR_IPV4_LOOPBACK, NetDevice,
    };
    use yarm_ipc_abi::tcpip_abi::{
        Ipv4SendSpec, TCPIP_PLAN_F_SOURCE_EXPLICIT, TCPIP_PROTOCOL_UDP, TcpipRequest, TcpipStatus,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReplyMode {
        Normal,
        Malformed,
        Unsupported,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeNetmgrIpcTransport {
        service: NetmgrService,
        mode: ReplyMode,
    }

    impl FakeNetmgrIpcTransport {
        const fn new(service: NetmgrService) -> Self {
            Self {
                service,
                mode: ReplyMode::Normal,
            }
        }

        const fn with_mode(service: NetmgrService, mode: ReplyMode) -> Self {
            Self { service, mode }
        }
    }

    impl NetmgrIpcTransport for FakeNetmgrIpcTransport {
        fn exchange(&mut self, request: &Message) -> Result<Message, NetmgrIpcExchangeError> {
            match self.mode {
                ReplyMode::Malformed => Message::with_header(0, request.opcode, 0, None, &[1])
                    .map_err(|_| NetmgrIpcExchangeError::Transport),
                ReplyMode::Unsupported => {
                    let payload = NetmgrResponse::status(NetmgrStatus::Unsupported)
                        .encode()
                        .map_err(|_| NetmgrIpcExchangeError::Transport)?;
                    Message::with_header(0, request.opcode, 0, None, &payload)
                        .map_err(|_| NetmgrIpcExchangeError::Transport)
                }
                ReplyMode::Normal => {
                    let response = self
                        .service
                        .handle_wire_request(request.opcode, request.as_slice());
                    let payload = response
                        .encode()
                        .map_err(|_| NetmgrIpcExchangeError::Transport)?;
                    Message::with_header(0, request.opcode, 0, None, &payload)
                        .map_err(|_| NetmgrIpcExchangeError::Transport)
                }
            }
        }
    }

    const fn ipv4(a: u8, b: u8, c: u8, d: u8) -> u32 {
        u32::from_be_bytes([a, b, c, d])
    }

    fn device(device_id: u32, link_up: bool, mtu: u16) -> NetDevice {
        NetDevice {
            device_id,
            owner_id: u64::from(device_id) + 100,
            generation: 1,
            mac: [0x02, 0, 0, 0, 0, device_id as u8],
            mtu,
            flags: NET_DEVICE_FLAG_BROADCAST,
            link_up,
        }
    }

    fn register(service: &mut NetmgrService, device: NetDevice) {
        assert_eq!(
            service
                .handle_request(NetmgrRequest::RegisterDevice { device })
                .status,
            NetmgrStatus::Ok
        );
    }

    fn add_address(service: &mut NetmgrService, device: NetDevice, address: u32) {
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddIpv4Address {
                    address: Ipv4Address {
                        device_id: device.device_id,
                        address,
                        prefix_len: 24,
                        generation: device.generation,
                        owner_id: device.owner_id,
                    },
                })
                .status,
            NetmgrStatus::Ok
        );
    }

    fn add_route(
        service: &mut NetmgrService,
        route_id: u32,
        device: NetDevice,
        destination: u32,
        prefix_len: u8,
        gateway: u32,
    ) {
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddRoute {
                    route: Ipv4Route {
                        route_id,
                        destination,
                        prefix_len,
                        gateway,
                        device_id: device.device_id,
                        metric: 10,
                        generation: device.generation,
                        owner_id: device.owner_id,
                    },
                })
                .status,
            NetmgrStatus::Ok
        );
    }

    #[test]
    fn ipc_resolver_maps_direct_gateway_no_route_and_link_down() {
        let mut netmgr = NetmgrService::new();
        let direct_device = device(2, true, 1500);
        let gateway_device = device(3, true, 1400);
        let down_device = device(4, false, 1300);
        for device in [direct_device, gateway_device, down_device] {
            register(&mut netmgr, device);
        }
        add_route(&mut netmgr, 2, direct_device, ipv4(10, 0, 0, 0), 24, 0);
        add_route(
            &mut netmgr,
            3,
            gateway_device,
            ipv4(192, 0, 2, 0),
            24,
            ipv4(198, 51, 100, 1),
        );
        add_route(&mut netmgr, 4, down_device, ipv4(203, 0, 113, 0), 24, 0);
        let mut resolver = NetmgrIpcRouteResolver::new(FakeNetmgrIpcTransport::new(netmgr));

        assert_eq!(
            resolver.lookup_ipv4_route(ipv4(10, 0, 0, 9)),
            Ok(ResolvedIpv4Route {
                route_id: 2,
                device_id: 2,
                gateway: 0,
                mtu: 1500,
            })
        );
        assert_eq!(
            resolver.lookup_ipv4_route(ipv4(192, 0, 2, 9)),
            Ok(ResolvedIpv4Route {
                route_id: 3,
                device_id: 3,
                gateway: ipv4(198, 51, 100, 1),
                mtu: 1400,
            })
        );
        assert_eq!(
            resolver.lookup_ipv4_route(ipv4(203, 0, 113, 9)),
            Err(RouteResolveError::LinkDown)
        );
        assert_eq!(
            resolver.lookup_ipv4_route(ipv4(8, 8, 8, 8)),
            Err(RouteResolveError::NoRoute)
        );
    }

    #[test]
    fn ipc_resolver_selects_and_checks_sources_including_loopback() {
        let mut netmgr = NetmgrService::new();
        let first = device(5, true, 1500);
        let second = device(6, true, 1500);
        register(&mut netmgr, first);
        register(&mut netmgr, second);
        add_address(&mut netmgr, first, ipv4(10, 0, 0, 5));
        add_address(&mut netmgr, first, ipv4(10, 0, 0, 6));
        add_address(&mut netmgr, second, ipv4(192, 0, 2, 6));
        let mut resolver = NetmgrIpcRouteResolver::new(FakeNetmgrIpcTransport::new(netmgr));

        assert_eq!(resolver.first_ipv4_address(5), Some(ipv4(10, 0, 0, 5)));
        assert!(resolver.has_ipv4_address(5, ipv4(10, 0, 0, 6)));
        assert!(!resolver.has_ipv4_address(5, ipv4(192, 0, 2, 6)));
        assert_eq!(
            resolver.first_ipv4_address(NETMGR_DEVICE_ID_LOOPBACK),
            Some(NETMGR_IPV4_LOOPBACK)
        );
        assert!(resolver.has_ipv4_address(NETMGR_DEVICE_ID_LOOPBACK, NETMGR_IPV4_LOOPBACK));
        assert_eq!(resolver.first_ipv4_address(99), None);
    }

    #[test]
    fn ipc_resolver_rejects_malformed_and_unsupported_replies_safely() {
        for mode in [ReplyMode::Malformed, ReplyMode::Unsupported] {
            let mut resolver = NetmgrIpcRouteResolver::new(FakeNetmgrIpcTransport::with_mode(
                NetmgrService::new(),
                mode,
            ));
            assert_eq!(
                resolver.lookup_ipv4_route(NETMGR_IPV4_LOOPBACK),
                Err(RouteResolveError::Unsupported)
            );
            assert_eq!(resolver.first_ipv4_address(NETMGR_DEVICE_ID_LOOPBACK), None);
            assert!(!resolver.has_ipv4_address(NETMGR_DEVICE_ID_LOOPBACK, NETMGR_IPV4_LOOPBACK));
        }
    }

    #[test]
    fn tcpip_plan_over_ipc_resolver_matches_netmgr_metadata() {
        let mut netmgr = NetmgrService::new();
        let output = device(7, true, 1450);
        register(&mut netmgr, output);
        add_address(&mut netmgr, output, ipv4(10, 7, 0, 2));
        add_route(&mut netmgr, 70, output, 0, 0, ipv4(10, 7, 0, 1));
        let resolver = NetmgrIpcRouteResolver::new(FakeNetmgrIpcTransport::new(netmgr));
        let mut service = TcpipService::new(resolver);
        let response = service.handle_request(TcpipRequest::PlanSendIpv4 {
            spec: Ipv4SendSpec {
                request_id: 9,
                source: 0,
                destination: ipv4(198, 51, 100, 9),
                payload_len: 1200,
                protocol: TCPIP_PROTOCOL_UDP,
                ttl: 42,
                flags: 0,
            },
        });
        assert_eq!(response.status, TcpipStatus::Ok);
        assert_eq!(response.route_id, 70);
        assert_eq!(response.device_id, 7);
        assert_eq!(response.gateway, ipv4(10, 7, 0, 1));
        assert_eq!(response.next_hop, ipv4(10, 7, 0, 1));
        assert_eq!(response.source, ipv4(10, 7, 0, 2));
        assert_eq!(response.mtu, 1450);
        assert_eq!(response.effective_ttl, 42);

        let wrong_source = service.handle_request(TcpipRequest::PlanSendIpv4 {
            spec: Ipv4SendSpec {
                request_id: 10,
                source: ipv4(192, 0, 2, 10),
                destination: ipv4(198, 51, 100, 10),
                payload_len: 100,
                protocol: TCPIP_PROTOCOL_UDP,
                ttl: 42,
                flags: TCPIP_PLAN_F_SOURCE_EXPLICIT,
            },
        });
        assert_eq!(wrong_source.status, TcpipStatus::NoSourceAddr);
    }
}
