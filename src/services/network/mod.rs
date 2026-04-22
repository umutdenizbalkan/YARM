// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod dhcp;
pub mod dns;
pub mod netmgr;
pub mod socket;
pub mod tcpip;

pub mod sim;

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_scoped_network_modules_are_include_only_shims() {
        let dhcp_service = include_str!("dhcp/service.rs");
        let dns_service = include_str!("dns/service.rs");
        let netmgr_service = include_str!("netmgr/service.rs");
        let socket_service = include_str!("socket/service.rs");
        let tcpip_service = include_str!("tcpip/service.rs");

        assert!(dhcp_service.contains("/crates/yarm-network-servers/src/network/dhcp/service.rs"));
        assert!(dns_service.contains("/crates/yarm-network-servers/src/network/dns/service.rs"));
        assert!(
            netmgr_service.contains("/crates/yarm-network-servers/src/network/netmgr/service.rs")
        );
        assert!(
            socket_service.contains("/crates/yarm-network-servers/src/network/socket/service.rs")
        );
        assert!(tcpip_service.contains("/crates/yarm-network-servers/src/network/tcpip/service.rs"));
    }
}
