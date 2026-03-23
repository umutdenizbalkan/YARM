use crate::kernel::vfs::InMemoryBackend;
use crate::services::common::service::{run_typed_request_loop, FsService};

pub fn run() {
    let mut vfs = FsService::with_backend(InMemoryBackend::new());
    let reply = run_typed_request_loop(
        &mut vfs,
        [crate::kernel::vfs::openat_message(crate::kernel::vfs::OpenAtRequest {
            dirfd: 0,
            path_ptr: 0x1000,
            flags: 0,
            mode: 0,
        })
        .expect("request")],
    )
    .expect("vfs loop")[0];
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&reply.as_slice()[..8]);
    let fd = u64::from_le_bytes(bytes);

    crate::yarm_log!("vfs server loop: fd={}, handled={}", fd, vfs.handled_count());
}
