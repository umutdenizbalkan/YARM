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
}
