// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

pub mod network;
pub use network::{dhcp, dns, netmgr, socket, tcpip};

pub fn run_dhcp() {
    network::dhcp::run();
}

pub fn run_dns() {
    network::dns::run();
}

pub fn run_netmgr() {
    network::netmgr::run();
}

pub fn run_socket() {
    network::socket::run();
}

pub fn run_tcpip() {
    network::tcpip::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_network_impl_does_not_delegate_back_to_legacy_network_namespace() {
        let dhcp_src = include_str!("network/dhcp/service.rs");
        let dns_src = include_str!("network/dns/service.rs");
        let netmgr_src = include_str!("network/netmgr/service.rs");
        let socket_src = include_str!("network/socket/service.rs");
        let tcpip_src = include_str!("network/tcpip/service.rs");
        let legacy_network = ["yarm", "::services::", "network::"].concat();

        for src in [dhcp_src, dns_src, netmgr_src, socket_src, tcpip_src] {
            assert!(
                !src.contains(legacy_network.as_str()),
                "workspace scoped network impl must not delegate to legacy network namespace"
            );
        }
    }

    #[test]
    fn network_server_bin_parity_guard_covers_expected_entrypoints() {
        let cargo_toml = include_str!("../Cargo.toml");
        let expected_bins = [
            (
                "dhcp_srv",
                "name = \"dhcp_srv\"",
                "path = \"src/bin/dhcp_srv.rs\"",
                "bin/dhcp_srv.rs",
                "run_dhcp",
            ),
            (
                "dns_srv",
                "name = \"dns_srv\"",
                "path = \"src/bin/dns_srv.rs\"",
                "bin/dns_srv.rs",
                "run_dns",
            ),
            (
                "netmgr_srv",
                "name = \"netmgr_srv\"",
                "path = \"src/bin/netmgr_srv.rs\"",
                "bin/netmgr_srv.rs",
                "run_netmgr",
            ),
            (
                "socket_srv",
                "name = \"socket_srv\"",
                "path = \"src/bin/socket_srv.rs\"",
                "bin/socket_srv.rs",
                "run_socket",
            ),
            (
                "tcpip_srv",
                "name = \"tcpip_srv\"",
                "path = \"src/bin/tcpip_srv.rs\"",
                "bin/tcpip_srv.rs",
                "run_tcpip",
            ),
        ];

        for (bin_name, name_entry, path_entry, bin_path, run_fn) in expected_bins {
            assert!(
                cargo_toml.contains(name_entry),
                "Cargo.toml missing expected bin entry: {bin_name}"
            );
            assert!(
                cargo_toml.contains(path_entry),
                "Cargo.toml missing expected bin path for: {bin_name}"
            );

            let src = match bin_path {
                "bin/dhcp_srv.rs" => include_str!("bin/dhcp_srv.rs"),
                "bin/dns_srv.rs" => include_str!("bin/dns_srv.rs"),
                "bin/netmgr_srv.rs" => include_str!("bin/netmgr_srv.rs"),
                "bin/socket_srv.rs" => include_str!("bin/socket_srv.rs"),
                "bin/tcpip_srv.rs" => include_str!("bin/tcpip_srv.rs"),
                _ => panic!("unexpected bin path in parity table: {bin_path}"),
            };
            assert!(
                src.contains("yarm_network_servers::"),
                "{bin_name} should dispatch via yarm_network_servers crate entrypoint"
            );
            assert!(
                src.contains(run_fn),
                "{bin_name} should call {run_fn} for parity with network service mapping"
            );
        }
    }
}
