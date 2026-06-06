// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::tcpip_abi::{
    Ipv4SendSpec, TCPIP_DEFAULT_TTL, TCPIP_IPV4_HEADER_ALLOWANCE, TcpipCodecError, TcpipRequest,
    TcpipResponse, TcpipStatus, valid_destination,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedIpv4Route {
    pub route_id: u32,
    pub device_id: u32,
    pub gateway: u32,
    pub mtu: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteResolveError {
    Unsupported,
    NoRoute,
    LinkDown,
}

pub trait TcpipRouteResolver {
    fn lookup_ipv4_route(
        &mut self,
        destination: u32,
    ) -> Result<ResolvedIpv4Route, RouteResolveError>;

    fn first_ipv4_address(&mut self, device_id: u32) -> Option<u32>;

    fn has_ipv4_address(&mut self, device_id: u32, address: u32) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnsupportedRouteResolver;

impl TcpipRouteResolver for UnsupportedRouteResolver {
    fn lookup_ipv4_route(
        &mut self,
        _destination: u32,
    ) -> Result<ResolvedIpv4Route, RouteResolveError> {
        Err(RouteResolveError::Unsupported)
    }

    fn first_ipv4_address(&mut self, _device_id: u32) -> Option<u32> {
        None
    }

    fn has_ipv4_address(&mut self, _device_id: u32, _address: u32) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpipStats {
    pub planned: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpipService<R> {
    resolver: R,
    default_ttl: u8,
    stats: TcpipStats,
}

impl<R: TcpipRouteResolver> TcpipService<R> {
    pub const fn new(resolver: R) -> Self {
        Self {
            resolver,
            default_ttl: TCPIP_DEFAULT_TTL,
            stats: TcpipStats {
                planned: 0,
                failed: 0,
            },
        }
    }

    pub const fn default_ttl(&self) -> u8 {
        self.default_ttl
    }

    pub const fn stats(&self) -> TcpipStats {
        self.stats
    }

    pub fn handle_request(&mut self, request: TcpipRequest) -> TcpipResponse {
        match request {
            TcpipRequest::RouteIpv4 {
                request_id,
                destination,
            } => self.route_ipv4(request_id, destination),
            TcpipRequest::PlanSendIpv4 { spec } => self.plan_send_ipv4(spec),
            TcpipRequest::GetLocalIpv4 {
                request_id,
                device_id,
            } => self.get_local_ipv4(request_id, device_id),
            TcpipRequest::SetDefaultTtl { request_id, ttl } => {
                self.default_ttl = ttl;
                let mut response = TcpipResponse::status(TcpipStatus::Ok, request_id);
                response.effective_ttl = ttl;
                response
            }
            TcpipRequest::GetStatus { request_id } => self.get_status(request_id),
        }
    }

    pub fn handle_wire_request(&mut self, opcode: u16, payload: &[u8]) -> TcpipResponse {
        match TcpipRequest::decode(opcode, payload) {
            Ok(request) => self.handle_request(request),
            Err(TcpipCodecError::UnsupportedOpcode | TcpipCodecError::UnsupportedProtocol) => {
                TcpipResponse::status(TcpipStatus::Unsupported, 0)
            }
            Err(TcpipCodecError::InvalidTtl) => TcpipResponse::status(TcpipStatus::InvalidTtl, 0),
            Err(TcpipCodecError::InvalidAddress) => {
                TcpipResponse::status(TcpipStatus::InvalidAddress, 0)
            }
            Err(_) => TcpipResponse::status(TcpipStatus::BadRequest, 0),
        }
    }

    fn route_ipv4(&mut self, request_id: u64, destination: u32) -> TcpipResponse {
        match self.resolver.lookup_ipv4_route(destination) {
            Ok(route) => {
                let mut response = route_response(TcpipStatus::Ok, request_id, destination, route);
                response.effective_ttl = self.default_ttl;
                response
            }
            Err(error) => TcpipResponse::status(map_route_error(error), request_id),
        }
    }

    fn plan_send_ipv4(&mut self, spec: Ipv4SendSpec) -> TcpipResponse {
        let route = match self.resolver.lookup_ipv4_route(spec.destination) {
            Ok(route) => route,
            Err(error) => return self.plan_failure(map_route_error(error), spec),
        };
        if route.mtu <= TCPIP_IPV4_HEADER_ALLOWANCE
            || spec.payload_len > route.mtu - TCPIP_IPV4_HEADER_ALLOWANCE
        {
            let mut response = self.plan_failure(TcpipStatus::MtuExceeded, spec);
            apply_route(&mut response, route, spec.destination);
            return response;
        }
        let source = if spec.source == 0 {
            match self.resolver.first_ipv4_address(route.device_id) {
                Some(address) if valid_destination(address) => address,
                _ => {
                    let mut response = self.plan_failure(TcpipStatus::NoSourceAddr, spec);
                    apply_route(&mut response, route, spec.destination);
                    return response;
                }
            }
        } else if self.resolver.has_ipv4_address(route.device_id, spec.source) {
            spec.source
        } else {
            let mut response = self.plan_failure(TcpipStatus::NoSourceAddr, spec);
            apply_route(&mut response, route, spec.destination);
            return response;
        };

        self.stats.planned = self.stats.planned.saturating_add(1);
        let mut response =
            route_response(TcpipStatus::Ok, spec.request_id, spec.destination, route);
        response.source = source;
        response.payload_len = spec.payload_len;
        response.effective_ttl = spec.ttl;
        response.protocol = spec.protocol;
        response.planned_count = self.stats.planned;
        response.failed_count = self.stats.failed;
        response
    }

    fn plan_failure(&mut self, status: TcpipStatus, spec: Ipv4SendSpec) -> TcpipResponse {
        self.stats.failed = self.stats.failed.saturating_add(1);
        let mut response = TcpipResponse::status(status, spec.request_id);
        response.source = spec.source;
        response.destination = spec.destination;
        response.payload_len = spec.payload_len;
        response.effective_ttl = spec.ttl;
        response.protocol = spec.protocol;
        response.planned_count = self.stats.planned;
        response.failed_count = self.stats.failed;
        response
    }

    fn get_local_ipv4(&mut self, request_id: u64, device_id: u32) -> TcpipResponse {
        match self.resolver.first_ipv4_address(device_id) {
            Some(source) if valid_destination(source) => {
                let mut response = TcpipResponse::status(TcpipStatus::Ok, request_id);
                response.device_id = device_id;
                response.source = source;
                response
            }
            _ => TcpipResponse::status(TcpipStatus::NoSourceAddr, request_id),
        }
    }

    fn get_status(&self, request_id: u64) -> TcpipResponse {
        let mut response = TcpipResponse::status(TcpipStatus::Ok, request_id);
        response.effective_ttl = self.default_ttl;
        response.planned_count = self.stats.planned;
        response.failed_count = self.stats.failed;
        response
    }
}

fn map_route_error(error: RouteResolveError) -> TcpipStatus {
    match error {
        RouteResolveError::Unsupported => TcpipStatus::Unsupported,
        RouteResolveError::NoRoute => TcpipStatus::NoRoute,
        RouteResolveError::LinkDown => TcpipStatus::LinkDown,
    }
}

fn route_response(
    status: TcpipStatus,
    request_id: u64,
    destination: u32,
    route: ResolvedIpv4Route,
) -> TcpipResponse {
    let mut response = TcpipResponse::status(status, request_id);
    apply_route(&mut response, route, destination);
    response
}

fn apply_route(response: &mut TcpipResponse, route: ResolvedIpv4Route, destination: u32) {
    response.route_id = route.route_id;
    response.device_id = route.device_id;
    response.gateway = route.gateway;
    response.destination = destination;
    response.next_hop = if route.gateway == 0 {
        destination
    } else {
        route.gateway
    };
    response.mtu = route.mtu;
}

pub fn run() {
    yarm_user_rt::user_log!("TCPIP_SRV_ENTRY");
    let mut service = TcpipService::new(UnsupportedRouteResolver);
    yarm_user_rt::user_log!(
        "TCPIP_READY mode=planning-only default_ttl={}",
        service.default_ttl()
    );

    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("TCPIP_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("TCPIP_RECV_LOOP cap={}", recv_cap);
    loop {
        // SAFETY: tcpip_srv owns its startup-provided service receive endpoint.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let response = service
                    .handle_wire_request(received.message.opcode, received.message.as_slice());
                let Some(reply_cap) = received.reply_cap else {
                    continue;
                };
                if let Ok(reply) = yarm_user_rt::ipc::Message::with_header(
                    0,
                    received.message.opcode,
                    0,
                    None,
                    &response.encode(),
                ) {
                    // SAFETY: the reply capability accompanied this received request.
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                }
            }
            Ok(None) => {}
            Err(error) => yarm_user_rt::user_log!("TCPIP_RECV_ERR err={:?}", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::netmgr::service::NetmgrService;
    use yarm_ipc_abi::netmgr_abi::{
        Ipv4Address, Ipv4Route, NET_DEVICE_FLAG_BROADCAST, NetDevice, NetmgrRequest, NetmgrStatus,
        mask_ipv4,
    };
    use yarm_ipc_abi::tcpip_abi::{
        TCPIP_OP_GET_STATUS, TCPIP_PLAN_F_SOURCE_EXPLICIT, TCPIP_PROTOCOL_UDP, TCPIP_WIRE_LEN,
    };

    const MAX_TEST_LOCAL_ADDRS: usize = 8;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeNetmgrResolver {
        netmgr: NetmgrService,
        locals: [Option<(u32, u32)>; MAX_TEST_LOCAL_ADDRS],
    }

    impl FakeNetmgrResolver {
        const fn new() -> Self {
            Self {
                netmgr: NetmgrService::new(),
                locals: [None; MAX_TEST_LOCAL_ADDRS],
            }
        }

        fn register_device(&mut self, device: NetDevice) {
            assert_eq!(
                self.netmgr
                    .handle_request(NetmgrRequest::RegisterDevice { device })
                    .status,
                NetmgrStatus::Ok
            );
        }

        fn add_address(&mut self, device: NetDevice, address: u32) {
            assert_eq!(
                self.netmgr
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
            let slot = self
                .locals
                .iter_mut()
                .find(|entry| entry.is_none())
                .expect("test local address capacity");
            *slot = Some((device.device_id, address));
        }

        fn add_route(
            &mut self,
            route_id: u32,
            device: NetDevice,
            destination: u32,
            prefix_len: u8,
            gateway: u32,
            metric: u32,
        ) {
            assert_eq!(
                self.netmgr
                    .handle_request(NetmgrRequest::AddRoute {
                        route: Ipv4Route {
                            route_id,
                            destination: mask_ipv4(destination, prefix_len),
                            prefix_len,
                            gateway,
                            device_id: device.device_id,
                            metric,
                            generation: device.generation,
                            owner_id: device.owner_id,
                        },
                    })
                    .status,
                NetmgrStatus::Ok
            );
        }
    }

    impl TcpipRouteResolver for FakeNetmgrResolver {
        fn lookup_ipv4_route(
            &mut self,
            destination: u32,
        ) -> Result<ResolvedIpv4Route, RouteResolveError> {
            let response = self
                .netmgr
                .handle_request(NetmgrRequest::LookupRoute { destination });
            match response.status {
                NetmgrStatus::Ok => {
                    let route = response.route.expect("successful lookup includes route");
                    let device = self
                        .netmgr
                        .handle_request(NetmgrRequest::GetDevice {
                            device_id: route.device_id,
                        })
                        .device
                        .expect("route device exists");
                    Ok(ResolvedIpv4Route {
                        route_id: route.route_id,
                        device_id: route.device_id,
                        gateway: route.gateway,
                        mtu: u32::from(device.mtu),
                    })
                }
                NetmgrStatus::LinkDown => Err(RouteResolveError::LinkDown),
                NetmgrStatus::NotFound => Err(RouteResolveError::NoRoute),
                _ => Err(RouteResolveError::Unsupported),
            }
        }

        fn first_ipv4_address(&mut self, device_id: u32) -> Option<u32> {
            self.locals
                .iter()
                .flatten()
                .find_map(|(current_device, address)| {
                    (*current_device == device_id).then_some(*address)
                })
        }

        fn has_ipv4_address(&mut self, device_id: u32, address: u32) -> bool {
            self.locals
                .iter()
                .flatten()
                .any(|entry| *entry == (device_id, address))
        }
    }

    fn ipv4(a: u8, b: u8, c: u8, d: u8) -> u32 {
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

    fn spec(source: u32, destination: u32, payload_len: u32) -> Ipv4SendSpec {
        Ipv4SendSpec {
            request_id: 9,
            source,
            destination,
            payload_len,
            protocol: TCPIP_PROTOCOL_UDP,
            ttl: 40,
            flags: if source == 0 {
                0
            } else {
                TCPIP_PLAN_F_SOURCE_EXPLICIT
            },
        }
    }

    #[test]
    fn tcpip_no_route_returns_no_route() {
        let mut service = TcpipService::new(FakeNetmgrResolver::new());
        assert_eq!(
            service
                .handle_request(TcpipRequest::PlanSendIpv4 {
                    spec: spec(0, ipv4(10, 0, 0, 1), 10),
                })
                .status,
            TcpipStatus::NoRoute
        );
    }

    #[test]
    fn tcpip_link_down_route_returns_link_down() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(11, false, 1500);
        resolver.register_device(device);
        resolver.add_route(1, device, 0, 0, 0, 0);
        let mut service = TcpipService::new(resolver);
        assert_eq!(
            service
                .handle_request(TcpipRequest::RouteIpv4 {
                    request_id: 1,
                    destination: ipv4(10, 0, 0, 1),
                })
                .status,
            TcpipStatus::LinkDown
        );
    }

    #[test]
    fn tcpip_direct_and_gateway_routes_select_next_hop() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(2, true, 1500);
        resolver.register_device(device);
        resolver.add_route(1, device, ipv4(10, 0, 0, 0), 24, 0, 0);
        resolver.add_route(2, device, 0, 0, ipv4(192, 0, 2, 1), 100);
        let mut service = TcpipService::new(resolver);
        let direct_destination = ipv4(10, 0, 0, 9);
        let direct = service.handle_request(TcpipRequest::RouteIpv4 {
            request_id: 1,
            destination: direct_destination,
        });
        assert_eq!(direct.next_hop, direct_destination);
        assert_eq!(direct.gateway, 0);
        let gateway = service.handle_request(TcpipRequest::RouteIpv4 {
            request_id: 2,
            destination: ipv4(203, 0, 113, 9),
        });
        assert_eq!(gateway.next_hop, ipv4(192, 0, 2, 1));
        assert_eq!(gateway.gateway, ipv4(192, 0, 2, 1));
    }

    #[test]
    fn tcpip_observes_netmgr_longest_prefix_and_metric_selection() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(3, true, 1500);
        resolver.register_device(device);
        resolver.add_route(1, device, ipv4(10, 0, 0, 0), 8, 0, 1);
        resolver.add_route(2, device, ipv4(10, 1, 0, 0), 16, 0, 20);
        resolver.add_route(3, device, ipv4(10, 1, 0, 0), 16, 0, 10);
        let mut service = TcpipService::new(resolver);
        let response = service.handle_request(TcpipRequest::RouteIpv4 {
            request_id: 1,
            destination: ipv4(10, 1, 2, 3),
        });
        assert_eq!(response.status, TcpipStatus::Ok);
        assert_eq!(response.route_id, 3);
    }

    #[test]
    fn tcpip_explicit_source_must_belong_to_output_device() {
        let mut resolver = FakeNetmgrResolver::new();
        let first = device(4, true, 1500);
        let second = device(5, true, 1500);
        resolver.register_device(first);
        resolver.register_device(second);
        resolver.add_address(first, ipv4(10, 0, 0, 2));
        resolver.add_address(second, ipv4(192, 0, 2, 2));
        resolver.add_route(1, first, 0, 0, 0, 0);
        let mut service = TcpipService::new(resolver);
        assert_eq!(
            service
                .handle_request(TcpipRequest::PlanSendIpv4 {
                    spec: spec(ipv4(192, 0, 2, 2), ipv4(8, 8, 8, 8), 10),
                })
                .status,
            TcpipStatus::NoSourceAddr
        );
        let accepted = service.handle_request(TcpipRequest::PlanSendIpv4 {
            spec: spec(ipv4(10, 0, 0, 2), ipv4(8, 8, 8, 8), 10),
        });
        assert_eq!(accepted.status, TcpipStatus::Ok);
        assert_eq!(accepted.source, ipv4(10, 0, 0, 2));
    }

    #[test]
    fn tcpip_zero_source_selects_first_local_address() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(6, true, 1500);
        resolver.register_device(device);
        resolver.add_address(device, ipv4(10, 0, 0, 6));
        resolver.add_address(device, ipv4(10, 0, 0, 7));
        resolver.add_route(1, device, 0, 0, 0, 0);
        let mut service = TcpipService::new(resolver);
        let response = service.handle_request(TcpipRequest::PlanSendIpv4 {
            spec: spec(0, ipv4(1, 1, 1, 1), 100),
        });
        assert_eq!(response.status, TcpipStatus::Ok);
        assert_eq!(response.source, ipv4(10, 0, 0, 6));
    }

    #[test]
    fn tcpip_missing_source_returns_no_source_address() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(7, true, 1500);
        resolver.register_device(device);
        resolver.add_route(1, device, 0, 0, 0, 0);
        let mut service = TcpipService::new(resolver);
        assert_eq!(
            service
                .handle_request(TcpipRequest::PlanSendIpv4 {
                    spec: spec(0, ipv4(1, 1, 1, 1), 100),
                })
                .status,
            TcpipStatus::NoSourceAddr
        );
    }

    #[test]
    fn tcpip_payload_exceeding_mtu_is_rejected() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(8, true, 576);
        resolver.register_device(device);
        resolver.add_address(device, ipv4(10, 0, 0, 8));
        resolver.add_route(1, device, 0, 0, 0, 0);
        let mut service = TcpipService::new(resolver);
        let response = service.handle_request(TcpipRequest::PlanSendIpv4 {
            spec: spec(0, ipv4(1, 1, 1, 1), 557),
        });
        assert_eq!(response.status, TcpipStatus::MtuExceeded);
        assert_eq!(response.mtu, 576);
    }

    #[test]
    fn tcpip_valid_plan_returns_route_device_ttl_and_mtu() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(9, true, 1400);
        resolver.register_device(device);
        resolver.add_address(device, ipv4(192, 0, 2, 9));
        resolver.add_route(11, device, 0, 0, ipv4(192, 0, 2, 1), 5);
        let mut service = TcpipService::new(resolver);
        let response = service.handle_request(TcpipRequest::PlanSendIpv4 {
            spec: spec(0, ipv4(203, 0, 113, 10), 1000),
        });
        assert_eq!(response.status, TcpipStatus::Ok);
        assert_eq!(response.route_id, 11);
        assert_eq!(response.device_id, 9);
        assert_eq!(response.gateway, ipv4(192, 0, 2, 1));
        assert_eq!(response.next_hop, ipv4(192, 0, 2, 1));
        assert_eq!(response.mtu, 1400);
        assert_eq!(response.effective_ttl, 40);
        assert_eq!(response.planned_count, 1);
    }

    #[test]
    fn tcpip_get_local_and_default_ttl_are_deterministic() {
        let mut resolver = FakeNetmgrResolver::new();
        let device = device(10, true, 1500);
        resolver.register_device(device);
        resolver.add_address(device, ipv4(10, 0, 0, 10));
        let mut service = TcpipService::new(resolver);
        let local = service.handle_request(TcpipRequest::GetLocalIpv4 {
            request_id: 1,
            device_id: 10,
        });
        assert_eq!(local.status, TcpipStatus::Ok);
        assert_eq!(local.source, ipv4(10, 0, 0, 10));
        let set = service.handle_request(TcpipRequest::SetDefaultTtl {
            request_id: 2,
            ttl: 99,
        });
        assert_eq!(set.effective_ttl, 99);
        let status = service.handle_request(TcpipRequest::GetStatus { request_id: 3 });
        assert_eq!(status.effective_ttl, 99);
    }

    #[test]
    fn tcpip_default_resolver_is_explicitly_unsupported() {
        let mut service = TcpipService::new(UnsupportedRouteResolver);
        assert_eq!(
            service
                .handle_request(TcpipRequest::RouteIpv4 {
                    request_id: 1,
                    destination: ipv4(10, 0, 0, 1),
                })
                .status,
            TcpipStatus::Unsupported
        );
    }

    #[test]
    fn tcpip_wire_errors_map_to_service_statuses() {
        let mut service = TcpipService::new(UnsupportedRouteResolver);
        let mut malformed = [0u8; TCPIP_WIRE_LEN];
        malformed[127] = 1;
        assert_eq!(
            service
                .handle_wire_request(TCPIP_OP_GET_STATUS, &malformed)
                .status,
            TcpipStatus::BadRequest
        );
        assert_eq!(
            service
                .handle_wire_request(0xffff, &[0; TCPIP_WIRE_LEN])
                .status,
            TcpipStatus::Unsupported
        );
    }
}
