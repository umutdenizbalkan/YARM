#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
use core::arch::global_asm;
use yarm::kernel::boot::Bootstrap;
use yarm::kernel::ipc::Message;
use yarm::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
use yarm::kernel::process_abi::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args};
use yarm::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, VfsService, openat_message, read_message,
};
use yarm::services::fs::initramfs::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
global_asm!(
    r#"
    .section .note.Xen,"a",@note
    .align 4
    .long 4
    .long 4
    .long 18
    .asciz "Xen"
    .align 4
    .long _start

    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack:
    .skip 16384
boot_stack_end:

    .section .text.boot,"ax",@progbits
    .global _start
    .type _start,@function
_start:
    cli
    lea boot_stack_end(%rip), %rsp
    xor %rbp, %rbp
    mov %rbx, %rdi
    call kernel_entry_x86_64
1:
    hlt
    jmp 1b
    "#
);

#[inline]
fn run() {
    let kernel = Bootstrap::init().expect("kernel init");
    yarm::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );

    let mut proc = ProcessService::new();
    let spawn = Message::with_header(
        0,
        PROC_OP_SPAWN_V2,
        0,
        None,
        &SpawnV2Args::new(1, 99).encode(),
    )
    .expect("spawn");
    let spawn_rep = proc.handle(spawn).expect("spawn rep");
    let child = SpawnV2Result::decode(spawn_rep.as_slice()).expect("child");
    proc.mark_exit(child.pid, 7).expect("mark exit");

    let wait = Message::with_header(
        0,
        PROC_OP_WAITPID_V2,
        0,
        None,
        &WaitPidV2Args::new(1, child.pid.0).encode(),
    )
    .expect("wait");
    let wait_rep = proc.handle(wait).expect("wait rep");
    let waited = WaitPidV2Result::decode(wait_rep.as_slice()).expect("waited");

    let mut vfs = VfsService::with_backend(InitramfsBackend::new(4096));
    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: INITRAMFS_BUSYBOX_PATH_PTR,
        flags: 0,
        mode: 0,
    })
    .expect("open");
    let open_rep = vfs.handle_request(open).expect("open rep");
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);
    let read = read_message(ReadWriteRequest {
        fd,
        buf_ptr: 0,
        len: 16,
    })
    .expect("read");
    let read_rep = vfs.handle_request(read).expect("read rep");

    yarm::yarm_log!(
        "YARM_PROC_VFS_OK pid={} exit={} read_opcode={}",
        child.pid.0,
        waited.exit_code,
        read_rep.opcode
    );
}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "x86_64")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    run();
    loop {}
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(no_mangle)]
pub extern "C" fn kernel_entry_x86_64(_pvh_start_info: usize) -> ! {
    run();
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_boot_markers_run() {
        run();
    }
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
