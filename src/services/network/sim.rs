#[cfg(test)]
mod tests {
    use crate::services::network::{
        dhcp::service::DhcpService, dns::service::DnsService, netmgr::service::NetmgrService,
        socket::service::SocketAdapterService, tcpip::service::TcpIpService,
    };

    #[test]
    fn deterministic_network_bootstrap_flow_is_stable() {
        let mut netmgr = NetmgrService::new();
        let mut tcpip = TcpIpService::new();
        let mut dns = DnsService::new();
        let mut dhcp = DhcpService::new();
        let mut socket = SocketAdapterService::new();

        netmgr.mark_link(true);
        dhcp.grant_lease(false);
        tcpip.route_packet(true);
        dns.resolve(false);
        socket.open();
        socket.close();

        assert_eq!(netmgr.stats().links_up, 1);
        assert_eq!(dhcp.stats().leases_granted, 1);
        assert_eq!(tcpip.stats().routed_packets, 1);
        assert_eq!(dns.stats().upstream_queries, 1);
        assert_eq!(socket.stats().opens, 1);
        assert_eq!(socket.stats().closes, 1);
    }

    #[test]
    fn link_flap_dhcp_rebind_and_socket_recovery_is_deterministic() {
        let mut netmgr = NetmgrService::new();
        let mut tcpip = TcpIpService::new();
        let mut dhcp = DhcpService::new();
        let mut socket = SocketAdapterService::new();

        netmgr.mark_link(true);
        dhcp.grant_lease(false);
        tcpip.route_packet(true);
        socket.open();

        netmgr.mark_link(false);
        tcpip.route_packet(false);
        dhcp.grant_lease(true);
        netmgr.mark_link(true);
        tcpip.route_packet(true);
        socket.close();

        assert_eq!(netmgr.stats().links_up, 2);
        assert_eq!(netmgr.stats().links_down, 1);
        assert_eq!(dhcp.stats().leases_granted, 1);
        assert_eq!(dhcp.stats().lease_renewals, 1);
        assert_eq!(tcpip.stats().routed_packets, 2);
        assert_eq!(tcpip.stats().dropped_packets, 1);
        assert_eq!(socket.stats().opens, 1);
        assert_eq!(socket.stats().closes, 1);
    }

    #[test]
    fn repeated_route_loss_recovery_sequence_is_stable() {
        let mut netmgr = NetmgrService::new();
        let mut tcpip = TcpIpService::new();
        let mut dhcp = DhcpService::new();

        netmgr.mark_link(true);
        dhcp.grant_lease(false);

        for _ in 0..3 {
            netmgr.mark_link(false);
            tcpip.route_packet(false);
            dhcp.grant_lease(true);
            netmgr.mark_link(true);
            tcpip.route_packet(true);
        }

        assert_eq!(netmgr.stats().links_up, 4);
        assert_eq!(netmgr.stats().links_down, 3);
        assert_eq!(dhcp.stats().leases_granted, 1);
        assert_eq!(dhcp.stats().lease_renewals, 3);
        assert_eq!(tcpip.stats().routed_packets, 3);
        assert_eq!(tcpip.stats().dropped_packets, 3);
    }
}
