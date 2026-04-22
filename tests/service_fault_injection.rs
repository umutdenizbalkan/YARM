// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

extern crate yarm;

use yarm::kernel::ipc::Message;
use yarm::kernel::process::ProcessManagerError;
use yarm::kernel::process::ProcessService;
use yarm::kernel::process_abi::{PROC_OP_WAITPID_V2, WaitPidV2Args};
use yarm::yarm_driver_servers::virtio_gpu::service::VirtioGpuService;
use yarm::yarm_fs_servers::initramfs::service::{
    InitramfsService, run_request_loop as run_initramfs_request_loop,
};
use yarm::yarm_network_servers::{
    dhcp::service::DhcpService, dns::service::DnsService, netmgr::service::NetmgrService,
    tcpip::service::TcpIpService,
};
use yarm::yarm_ui_servers::display::service::{DisplayService, DisplayStats};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ServiceFaultSummary {
    control_plane_unknown_wait: ProcessManagerError,
    fs_read_only: bool,
    driver_rejected_commits: u64,
    network_link_downs: u64,
    network_route_drops: u64,
    network_lease_renewals: u64,
    ui_rejected_presents: u64,
}

fn run_service_fault_injection_matrix() -> ServiceFaultSummary {
    let mut proc = ProcessService::new();
    let wait_unknown = Message::with_header(
        0,
        PROC_OP_WAITPID_V2,
        0,
        None,
        &WaitPidV2Args::new(1, 0xDEAD).encode(),
    )
    .expect("wait");
    let control_plane_unknown_wait = proc.handle(wait_unknown).expect_err("unknown target");

    let mut initramfs =
        InitramfsService::with_backend(yarm::yarm_fs_servers::initramfs::InitramfsBackend::new(4096));
    let fs_read_only = !run_initramfs_request_loop(&mut initramfs)
        .expect("initramfs request loop")
        .write_allowed;

    let mut gpu = VirtioGpuService::new();
    gpu.commit_frame();
    gpu.mode_set();
    gpu.commit_frame();

    let mut netmgr = NetmgrService::new();
    let mut tcpip = TcpIpService::new();
    let mut dhcp = DhcpService::new();
    let mut dns = DnsService::new();
    netmgr.mark_link(true);
    dhcp.grant_lease(false);
    dns.resolve(false);
    for _ in 0..2 {
        netmgr.mark_link(false);
        tcpip.route_packet(false);
        dhcp.grant_lease(true);
        netmgr.mark_link(true);
        tcpip.route_packet(true);
    }

    let mut display = DisplayService::new();
    display.present();
    display.mode_set();
    display.present();

    ServiceFaultSummary {
        control_plane_unknown_wait,
        fs_read_only,
        driver_rejected_commits: gpu.stats().rejected_commits,
        network_link_downs: netmgr.stats().links_down,
        network_route_drops: tcpip.stats().dropped_packets,
        network_lease_renewals: dhcp.stats().lease_renewals,
        ui_rejected_presents: display.stats().rejected_presents,
    }
}

#[test]
fn deterministic_service_fault_injection_matrix_is_stable() {
    let summary = run_service_fault_injection_matrix();
    assert_eq!(
        summary.control_plane_unknown_wait,
        ProcessManagerError::PermissionDenied
    );
    assert!(summary.fs_read_only);
    assert_eq!(summary.driver_rejected_commits, 1);
    assert_eq!(summary.network_link_downs, 2);
    assert_eq!(summary.network_route_drops, 2);
    assert_eq!(summary.network_lease_renewals, 2);
    assert_eq!(summary.ui_rejected_presents, 1);
}

#[test]
fn ui_display_fault_path_rejects_present_before_modeset() {
    let mut display = DisplayService::new();
    display.present();
    display.mode_set();
    display.present();
    assert_eq!(
        display.stats(),
        DisplayStats {
            mode_sets: 1,
            frame_presents: 1,
            rejected_presents: 1,
        }
    );
}

#[test]
fn initramfs_fault_path_stays_read_only() {
    let mut initramfs =
        InitramfsService::with_backend(yarm::yarm_fs_servers::initramfs::InitramfsBackend::new(2048));
    let summary = run_initramfs_request_loop(&mut initramfs).expect("loop");
    assert_eq!(summary.write_allowed, false);
    // initramfs bootstrap loop performs: open + read + statx.
    assert_eq!(summary.handled, 3);
}
