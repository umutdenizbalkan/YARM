// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::netmgr_abi::{
    Ipv4Address, Ipv4Route, NetDevice, NetmgrCodecError, NetmgrRequest, NetmgrResponse,
    NetmgrStatus, ipv4_prefix_matches,
};

pub const MAX_NET_DEVICES: usize = 16;
pub const MAX_IPV4_ADDRS: usize = 32;
pub const MAX_ROUTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetmgrService {
    devices: [Option<NetDevice>; MAX_NET_DEVICES],
    addresses: [Option<Ipv4Address>; MAX_IPV4_ADDRS],
    routes: [Option<Ipv4Route>; MAX_ROUTES],
}

impl NetmgrService {
    pub const fn new() -> Self {
        Self {
            devices: [None; MAX_NET_DEVICES],
            addresses: [None; MAX_IPV4_ADDRS],
            routes: [None; MAX_ROUTES],
        }
    }

    pub fn device_count(&self) -> usize {
        self.devices.iter().flatten().count()
    }

    pub fn address_count(&self) -> usize {
        self.addresses.iter().flatten().count()
    }

    pub fn route_count(&self) -> usize {
        self.routes.iter().flatten().count()
    }

    pub fn handle_request(&mut self, request: NetmgrRequest) -> NetmgrResponse {
        match request {
            NetmgrRequest::RegisterDevice { device } => self.register_device(device),
            NetmgrRequest::UnregisterDevice {
                device_id,
                owner_id,
                generation,
            } => self.unregister_device(device_id, owner_id, generation),
            NetmgrRequest::GetDevice { device_id } => self.get_device(device_id),
            NetmgrRequest::ListDevices { start_index } => self.list_devices(start_index),
            NetmgrRequest::SetLinkState {
                device_id,
                owner_id,
                generation,
                link_up,
            } => self.set_link_state(device_id, owner_id, generation, link_up),
            NetmgrRequest::AddIpv4Address { address } => self.add_ipv4_address(address),
            NetmgrRequest::RemoveIpv4Address { address } => self.remove_ipv4_address(address),
            NetmgrRequest::AddRoute { route } => self.add_route(route),
            NetmgrRequest::RemoveRoute {
                route_id,
                owner_id,
                generation,
            } => self.remove_route(route_id, owner_id, generation),
            NetmgrRequest::LookupRoute { destination } => self.lookup_route(destination),
            NetmgrRequest::GetStatus => self.get_status(),
        }
    }

    pub fn handle_wire_request(&mut self, opcode: u16, payload: &[u8]) -> NetmgrResponse {
        match NetmgrRequest::decode(opcode, payload) {
            Ok(request) => self.handle_request(request),
            Err(NetmgrCodecError::UnsupportedOpcode) => {
                NetmgrResponse::status(NetmgrStatus::Unsupported)
            }
            Err(_) => NetmgrResponse::status(NetmgrStatus::BadRequest),
        }
    }

    fn register_device(&mut self, device: NetDevice) -> NetmgrResponse {
        if self.device_index(device.device_id).is_some() {
            return NetmgrResponse::status(NetmgrStatus::AlreadyExists);
        }
        let Some(index) = self.devices.iter().position(Option::is_none) else {
            return NetmgrResponse::status(NetmgrStatus::TableFull);
        };
        self.devices[index] = Some(device);
        response_with_device(NetmgrStatus::Ok, device)
    }

    fn unregister_device(
        &mut self,
        device_id: u32,
        owner_id: u64,
        generation: u32,
    ) -> NetmgrResponse {
        let index = match self.authorize_device(device_id, owner_id, generation) {
            Ok(index) => index,
            Err(status) => return NetmgrResponse::status(status),
        };
        let device = self.devices[index]
            .take()
            .expect("authorized device exists");
        for address in &mut self.addresses {
            if address
                .map(|address| address.device_id == device_id)
                .unwrap_or(false)
            {
                *address = None;
            }
        }
        for route in &mut self.routes {
            if route
                .map(|route| route.device_id == device_id)
                .unwrap_or(false)
            {
                *route = None;
            }
        }
        response_with_device(NetmgrStatus::Ok, device)
    }

    fn get_device(&self, device_id: u32) -> NetmgrResponse {
        self.device(device_id)
            .map(|device| response_with_device(NetmgrStatus::Ok, device))
            .unwrap_or_else(|| NetmgrResponse::status(NetmgrStatus::NotFound))
    }

    fn list_devices(&self, start_index: u16) -> NetmgrResponse {
        let start = usize::from(start_index);
        if start >= MAX_NET_DEVICES {
            return NetmgrResponse::status(NetmgrStatus::NotFound);
        }
        let Some((index, device)) = self
            .devices
            .iter()
            .enumerate()
            .skip(start)
            .find_map(|(index, entry)| entry.map(|device| (index, device)))
        else {
            return NetmgrResponse::status(NetmgrStatus::NotFound);
        };
        let mut response = response_with_device(NetmgrStatus::Ok, device);
        response.value = self.devices[index + 1..]
            .iter()
            .position(Option::is_some)
            .map(|offset| (index + 1 + offset) as u32)
            .unwrap_or(u32::MAX);
        response.auxiliary = self.device_count() as u32;
        response
    }

    fn set_link_state(
        &mut self,
        device_id: u32,
        owner_id: u64,
        generation: u32,
        link_up: bool,
    ) -> NetmgrResponse {
        let index = match self.authorize_device(device_id, owner_id, generation) {
            Ok(index) => index,
            Err(status) => return NetmgrResponse::status(status),
        };
        let device = self.devices[index]
            .as_mut()
            .expect("authorized device exists");
        device.link_up = link_up;
        response_with_device(NetmgrStatus::Ok, *device)
    }

    fn add_ipv4_address(&mut self, address: Ipv4Address) -> NetmgrResponse {
        if let Err(status) =
            self.authorize_device(address.device_id, address.owner_id, address.generation)
        {
            return NetmgrResponse::status(status);
        }
        if self.addresses.iter().flatten().any(|current| {
            current.device_id == address.device_id
                && current.address == address.address
                && current.prefix_len == address.prefix_len
        }) {
            return NetmgrResponse::status(NetmgrStatus::AlreadyExists);
        }
        let Some(slot) = self.addresses.iter_mut().find(|slot| slot.is_none()) else {
            return NetmgrResponse::status(NetmgrStatus::TableFull);
        };
        *slot = Some(address);
        response_with_address(NetmgrStatus::Ok, address)
    }

    fn remove_ipv4_address(&mut self, address: Ipv4Address) -> NetmgrResponse {
        if let Err(status) =
            self.authorize_device(address.device_id, address.owner_id, address.generation)
        {
            return NetmgrResponse::status(status);
        }
        let Some(slot) = self.addresses.iter_mut().find(|slot| {
            slot.map(|current| {
                current.device_id == address.device_id
                    && current.address == address.address
                    && current.prefix_len == address.prefix_len
            })
            .unwrap_or(false)
        }) else {
            return NetmgrResponse::status(NetmgrStatus::NotFound);
        };
        let removed = slot.take().expect("matched address exists");
        response_with_address(NetmgrStatus::Ok, removed)
    }

    fn add_route(&mut self, route: Ipv4Route) -> NetmgrResponse {
        if let Err(status) =
            self.authorize_device(route.device_id, route.owner_id, route.generation)
        {
            return NetmgrResponse::status(status);
        }
        if self
            .routes
            .iter()
            .flatten()
            .any(|current| current.route_id == route.route_id)
        {
            return NetmgrResponse::status(NetmgrStatus::AlreadyExists);
        }
        let Some(slot) = self.routes.iter_mut().find(|slot| slot.is_none()) else {
            return NetmgrResponse::status(NetmgrStatus::TableFull);
        };
        *slot = Some(route);
        response_with_route(NetmgrStatus::Ok, route)
    }

    fn remove_route(&mut self, route_id: u32, owner_id: u64, generation: u32) -> NetmgrResponse {
        let Some(index) = self.route_index(route_id) else {
            return NetmgrResponse::status(NetmgrStatus::NotFound);
        };
        let route = self.routes[index].expect("route index contains route");
        if route.owner_id != owner_id {
            return NetmgrResponse::status(NetmgrStatus::OwnerMismatch);
        }
        if route.generation != generation {
            return NetmgrResponse::status(NetmgrStatus::StaleGeneration);
        }
        self.routes[index] = None;
        response_with_route(NetmgrStatus::Ok, route)
    }

    fn lookup_route(&self, destination: u32) -> NetmgrResponse {
        let mut best: Option<Ipv4Route> = None;
        let mut matched_link_down = false;
        for route in self.routes.iter().flatten().copied() {
            if !ipv4_prefix_matches(destination, route.destination, route.prefix_len) {
                continue;
            }
            let Some(device) = self.device(route.device_id) else {
                continue;
            };
            if !device.link_up {
                matched_link_down = true;
                continue;
            }
            let replace = best
                .map(|current| {
                    route.prefix_len > current.prefix_len
                        || (route.prefix_len == current.prefix_len
                            && (route.metric < current.metric
                                || (route.metric == current.metric
                                    && route.route_id < current.route_id)))
                })
                .unwrap_or(true);
            if replace {
                best = Some(route);
            }
        }
        if let Some(route) = best {
            response_with_route(NetmgrStatus::Ok, route)
        } else if matched_link_down {
            NetmgrResponse::status(NetmgrStatus::LinkDown)
        } else {
            NetmgrResponse::status(NetmgrStatus::NotFound)
        }
    }

    fn get_status(&self) -> NetmgrResponse {
        NetmgrResponse {
            status: NetmgrStatus::Ok,
            device: None,
            address: None,
            route: None,
            value: self.device_count() as u32,
            auxiliary: ((self.address_count() as u32) << 16) | self.route_count() as u32,
        }
    }

    fn authorize_device(
        &self,
        device_id: u32,
        owner_id: u64,
        generation: u32,
    ) -> Result<usize, NetmgrStatus> {
        let Some(index) = self.device_index(device_id) else {
            return Err(NetmgrStatus::NotFound);
        };
        let device = self.devices[index].expect("device index contains device");
        if device.owner_id != owner_id {
            Err(NetmgrStatus::OwnerMismatch)
        } else if device.generation != generation {
            Err(NetmgrStatus::StaleGeneration)
        } else {
            Ok(index)
        }
    }

    fn device(&self, device_id: u32) -> Option<NetDevice> {
        self.device_index(device_id)
            .and_then(|index| self.devices[index])
    }

    fn device_index(&self, device_id: u32) -> Option<usize> {
        self.devices.iter().position(|entry| {
            entry
                .map(|device| device.device_id == device_id)
                .unwrap_or(false)
        })
    }

    fn route_index(&self, route_id: u32) -> Option<usize> {
        self.routes.iter().position(|entry| {
            entry
                .map(|route| route.route_id == route_id)
                .unwrap_or(false)
        })
    }
}

impl Default for NetmgrService {
    fn default() -> Self {
        Self::new()
    }
}

fn response_with_device(status: NetmgrStatus, device: NetDevice) -> NetmgrResponse {
    NetmgrResponse {
        status,
        device: Some(device),
        address: None,
        route: None,
        value: 0,
        auxiliary: 0,
    }
}

fn response_with_address(status: NetmgrStatus, address: Ipv4Address) -> NetmgrResponse {
    NetmgrResponse {
        status,
        device: None,
        address: Some(address),
        route: None,
        value: 0,
        auxiliary: 0,
    }
}

fn response_with_route(status: NetmgrStatus, route: Ipv4Route) -> NetmgrResponse {
    NetmgrResponse {
        status,
        device: None,
        address: None,
        route: Some(route),
        value: 0,
        auxiliary: 0,
    }
}

pub fn run() {
    yarm_user_rt::user_log!("NETMGR_SRV_ENTRY");
    let mut service = NetmgrService::new();
    yarm_user_rt::user_log!(
        "NETMGR_READY devices={} addresses={} routes={}",
        MAX_NET_DEVICES,
        MAX_IPV4_ADDRS,
        MAX_ROUTES
    );

    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("NETMGR_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("NETMGR_RECV_LOOP cap={}", recv_cap);
    loop {
        // SAFETY: netmgr_srv owns its startup-provided service receive endpoint.
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
            Err(error) => yarm_user_rt::user_log!("NETMGR_RECV_ERR err={:?}", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::netmgr_abi::{
        NET_DEVICE_FLAG_BROADCAST, NETMGR_OP_GET_STATUS, NETMGR_WIRE_LEN, mask_ipv4,
    };

    fn ipv4(a: u8, b: u8, c: u8, d: u8) -> u32 {
        u32::from_be_bytes([a, b, c, d])
    }

    fn device(device_id: u32, owner_id: u64, generation: u32) -> NetDevice {
        NetDevice {
            device_id,
            owner_id,
            generation,
            mac: [0x02, 0, 0, 0, (device_id >> 8) as u8, device_id as u8],
            mtu: 1500,
            flags: NET_DEVICE_FLAG_BROADCAST,
            link_up: true,
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

    fn address(device: NetDevice, address: u32, prefix_len: u8) -> Ipv4Address {
        Ipv4Address {
            device_id: device.device_id,
            address,
            prefix_len,
            generation: device.generation,
            owner_id: device.owner_id,
        }
    }

    fn route(
        route_id: u32,
        device: NetDevice,
        destination: u32,
        prefix_len: u8,
        metric: u32,
    ) -> Ipv4Route {
        Ipv4Route {
            route_id,
            destination: mask_ipv4(destination, prefix_len),
            prefix_len,
            gateway: 0,
            device_id: device.device_id,
            metric,
            generation: device.generation,
            owner_id: device.owner_id,
        }
    }

    #[test]
    fn netmgr_register_duplicate_get_and_unregister_device() {
        let mut service = NetmgrService::new();
        let device = device(1, 10, 1);
        register(&mut service, device);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::RegisterDevice { device })
                .status,
            NetmgrStatus::AlreadyExists
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::GetDevice { device_id: 1 })
                .device,
            Some(device)
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::UnregisterDevice {
                    device_id: 1,
                    owner_id: 10,
                    generation: 1,
                })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(service.device_count(), 0);
    }

    #[test]
    fn netmgr_unregister_cascades_addresses_and_routes() {
        let mut service = NetmgrService::new();
        let device = device(2, 20, 3);
        register(&mut service, device);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddIpv4Address {
                    address: address(device, ipv4(10, 0, 0, 2), 24),
                })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddRoute {
                    route: route(1, device, ipv4(10, 0, 0, 0), 24, 0),
                })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::UnregisterDevice {
                    device_id: 2,
                    owner_id: 20,
                    generation: 3,
                })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!((service.address_count(), service.route_count()), (0, 0));
    }

    #[test]
    fn netmgr_link_state_and_generation_checks() {
        let mut service = NetmgrService::new();
        let device = device(3, 30, 4);
        register(&mut service, device);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::SetLinkState {
                    device_id: 3,
                    owner_id: 30,
                    generation: 3,
                    link_up: false,
                })
                .status,
            NetmgrStatus::StaleGeneration
        );
        let response = service.handle_request(NetmgrRequest::SetLinkState {
            device_id: 3,
            owner_id: 30,
            generation: 4,
            link_up: false,
        });
        assert_eq!(response.status, NetmgrStatus::Ok);
        assert_eq!(response.device.map(|device| device.link_up), Some(false));
    }

    #[test]
    fn netmgr_adds_and_removes_ipv4_address() {
        let mut service = NetmgrService::new();
        let device = device(4, 40, 1);
        let address = address(device, ipv4(192, 0, 2, 1), 24);
        register(&mut service, device);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddIpv4Address { address })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(service.address_count(), 1);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::RemoveIpv4Address { address })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(service.address_count(), 0);
    }

    #[test]
    fn netmgr_adds_removes_and_looks_up_direct_route() {
        let mut service = NetmgrService::new();
        let device = device(5, 50, 1);
        let route = route(5, device, ipv4(198, 51, 100, 0), 24, 10);
        register(&mut service, device);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddRoute { route })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::LookupRoute {
                    destination: ipv4(198, 51, 100, 25),
                })
                .route,
            Some(route)
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::RemoveRoute {
                    route_id: 5,
                    owner_id: 50,
                    generation: 1,
                })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(service.route_count(), 0);
    }

    #[test]
    fn netmgr_default_route_is_used_when_no_specific_route_matches() {
        let mut service = NetmgrService::new();
        let device = device(6, 60, 1);
        let default = route(6, device, 0, 0, 100);
        register(&mut service, device);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddRoute { route: default })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::LookupRoute {
                    destination: ipv4(203, 0, 113, 9),
                })
                .route,
            Some(default)
        );
    }

    #[test]
    fn netmgr_longest_prefix_then_lowest_metric_wins() {
        let mut service = NetmgrService::new();
        let device = device(7, 70, 1);
        register(&mut service, device);
        let routes = [
            route(1, device, ipv4(10, 0, 0, 0), 8, 1),
            route(2, device, ipv4(10, 1, 0, 0), 16, 20),
            route(3, device, ipv4(10, 1, 0, 0), 16, 10),
        ];
        for route in routes {
            assert_eq!(
                service
                    .handle_request(NetmgrRequest::AddRoute { route })
                    .status,
                NetmgrStatus::Ok
            );
        }
        assert_eq!(
            service
                .handle_request(NetmgrRequest::LookupRoute {
                    destination: ipv4(10, 1, 2, 3),
                })
                .route,
            Some(routes[2])
        );
    }

    #[test]
    fn netmgr_link_down_route_returns_link_down() {
        let mut service = NetmgrService::new();
        let mut device = device(8, 80, 1);
        device.link_up = false;
        register(&mut service, device);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddRoute {
                    route: route(8, device, 0, 0, 0),
                })
                .status,
            NetmgrStatus::Ok
        );
        assert_eq!(
            service
                .handle_request(NetmgrRequest::LookupRoute {
                    destination: ipv4(1, 1, 1, 1),
                })
                .status,
            NetmgrStatus::LinkDown
        );
    }

    #[test]
    fn netmgr_route_to_missing_device_is_rejected() {
        let mut service = NetmgrService::new();
        let missing = device(9, 90, 1);
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddRoute {
                    route: route(9, missing, 0, 0, 0),
                })
                .status,
            NetmgrStatus::NotFound
        );
    }

    #[test]
    fn netmgr_device_table_enforces_capacity() {
        let mut service = NetmgrService::new();
        for index in 0..MAX_NET_DEVICES {
            register(&mut service, device(index as u32 + 1, index as u64 + 1, 1));
        }
        assert_eq!(
            service
                .handle_request(NetmgrRequest::RegisterDevice {
                    device: device(100, 100, 1),
                })
                .status,
            NetmgrStatus::TableFull
        );
    }

    #[test]
    fn netmgr_lists_devices_with_cursor() {
        let mut service = NetmgrService::new();
        let first = device(20, 200, 1);
        let second = device(21, 201, 1);
        register(&mut service, first);
        register(&mut service, second);
        let first_response = service.handle_request(NetmgrRequest::ListDevices { start_index: 0 });
        assert_eq!(first_response.device, Some(first));
        assert_eq!(first_response.value, 1);
        let second_response = service.handle_request(NetmgrRequest::ListDevices {
            start_index: first_response.value as u16,
        });
        assert_eq!(second_response.device, Some(second));
        assert_eq!(second_response.value, u32::MAX);
        assert_eq!(second_response.auxiliary, 2);
    }

    #[test]
    fn netmgr_address_and_route_tables_enforce_capacity() {
        let mut service = NetmgrService::new();
        let device = device(22, 220, 1);
        register(&mut service, device);
        for index in 0..MAX_IPV4_ADDRS {
            assert_eq!(
                service
                    .handle_request(NetmgrRequest::AddIpv4Address {
                        address: address(device, ipv4(10, 0, 0, index as u8 + 1), 24),
                    })
                    .status,
                NetmgrStatus::Ok
            );
        }
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddIpv4Address {
                    address: address(device, ipv4(10, 0, 1, 1), 24),
                })
                .status,
            NetmgrStatus::TableFull
        );
        for index in 0..MAX_ROUTES {
            assert_eq!(
                service
                    .handle_request(NetmgrRequest::AddRoute {
                        route: route(
                            index as u32 + 1,
                            device,
                            ipv4(172, 16, index as u8, 0),
                            24,
                            index as u32,
                        ),
                    })
                    .status,
                NetmgrStatus::Ok
            );
        }
        assert_eq!(
            service
                .handle_request(NetmgrRequest::AddRoute {
                    route: route(100, device, ipv4(192, 0, 2, 0), 24, 0),
                })
                .status,
            NetmgrStatus::TableFull
        );
    }

    #[test]
    fn netmgr_wire_errors_map_to_service_statuses() {
        let mut service = NetmgrService::new();
        let mut malformed = [0u8; NETMGR_WIRE_LEN];
        malformed[127] = 1;
        assert_eq!(
            service
                .handle_wire_request(NETMGR_OP_GET_STATUS, &malformed)
                .status,
            NetmgrStatus::BadRequest
        );
        assert_eq!(
            service
                .handle_wire_request(0xffff, &[0; NETMGR_WIRE_LEN])
                .status,
            NetmgrStatus::Unsupported
        );
    }
}
