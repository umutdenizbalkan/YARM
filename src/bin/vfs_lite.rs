use yarm::kernel::bootstrap::Bootstrap;
use yarm::kernel::ipc::Message;
use yarm::kernel::linux_compat::{LINUX_NR_OPENAT, VFS_OP_OPENAT, dispatch};
use yarm::kernel::trapframe::TrapFrame;

fn main() {
    let mut kernel = Bootstrap::init().expect("init");

    let (_req_ep, req_send, req_recv) = kernel.create_endpoint(8).expect("req ep");
    let (_rep_ep, rep_send, rep_recv) = kernel.create_endpoint(8).expect("rep ep");
    kernel
        .register_linux_vfs_manager(req_send, rep_recv)
        .expect("register vfs manager");

    kernel
        .ipc_send(
            rep_send,
            Message::with_header(0, VFS_OP_OPENAT, 0, None, &3u64.to_le_bytes()).expect("reply"),
        )
        .expect("seed reply");

    let mut frame = TrapFrame::new(LINUX_NR_OPENAT, [0, 0x1000, 0, 0, 0, 0]);
    dispatch(&mut kernel, &mut frame);

    let request = kernel
        .ipc_recv(req_recv)
        .expect("recv")
        .expect("request msg");

    println!(
        "vfs-lite demo: opcode={}, fd={}, req_len={}",
        request.opcode,
        frame.ret0,
        request.as_slice().len()
    );
}
