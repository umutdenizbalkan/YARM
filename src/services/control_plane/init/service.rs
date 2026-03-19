use crate::kernel::bootstrap::Bootstrap;
use crate::kernel::init_server::{
    CoreLaunchStrategy, CoreServiceGraph, CoreServiceImagePlan, InitFaultHandoff, InitServerLite,
};

pub fn run() {
    let mut kernel = Bootstrap::init().expect("init");
    let mut init = InitServerLite::new();
    init.set_launch_strategy(CoreLaunchStrategy::SupervisorFirst);
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
    let _ = init
        .launch_core_services(
            &mut kernel,
            CoreServiceImagePlan {
                process_manager_entry: 0x8000,
                vfs_entry: 0x9000,
                supervisor_entry: 0xA000,
            },
        )
        .expect("launch");
    init.install_fault_handoff(InitFaultHandoff {
        supervisor_tid: graph.supervisor_tid,
        restart_window_ticks: 100,
    })
    .expect("handoff");
    init.begin_running().expect("running");

    crate::yarm_log!(
        "init.srv scaffold online: phase={:?}, handles={:?}",
        init.phase(),
        init.handles()
    );
}
