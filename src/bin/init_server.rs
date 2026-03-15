#![no_std]
extern crate std;

use yarm::kernel::bootstrap::Bootstrap;
use yarm::kernel::init_server::{CoreServiceGraph, InitServerLite};

use std::println;

fn main() {
    let mut kernel = Bootstrap::init().expect("init");
    let mut init = InitServerLite::new();
    let graph = CoreServiceGraph {
        init_tid: 1,
        process_manager_tid: 2,
        vfs_tid: 3,
        supervisor_tid: 4,
    };

    init.register_core_graph(&mut kernel, graph)
        .expect("register graph");
    init.validate_core_delegation_paths(&kernel, graph.init_tid)
        .expect("delegation paths");
    init.begin_running().expect("running");

    println!(
        "init.srv scaffold online: phase={:?}, handles={:?}",
        init.phase(),
        init.handles()
    );
}
