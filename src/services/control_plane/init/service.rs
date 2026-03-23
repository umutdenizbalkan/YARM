use crate::kernel::boot::Bootstrap;
use crate::services::init::{
    CoreLaunchStrategy, CoreServiceGraph, CoreServiceImagePlan, InitService,
};

pub fn run() {
    let mut kernel = Bootstrap::init().expect("init");
    let mut init = InitService::new();
    init.set_launch_strategy(CoreLaunchStrategy::SupervisorFirst);
    let graph = CoreServiceGraph {
        init_tid: 1,
        process_manager_tid: 2,
        vfs_tid: 3,
        supervisor_tid: 4,
    };

    init.register_core_graph(&mut kernel, graph)
        .expect("register graph");
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
    let _handoff = init
        .install_fault_handoff(&mut kernel, 100)
        .expect("handoff");
    init.begin_running().expect("running");

    crate::yarm_log!(
        "init.srv scaffold online: phase={:?}, handles={:?}, present_cpus={}, present_bitmap=0x{:x}, online_cpus={}",
        init.phase(),
        init.handles(),
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
}
