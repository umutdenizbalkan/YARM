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
    .long pvh_start32

    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack:
    .skip 16384
boot_stack_end:

    .section .data.boot,"aw",@progbits
    .align 4096
boot_pml4:
    .quad boot_pdpt + 0x3
    .zero 4088

    .align 4096
boot_pdpt:
    .quad boot_pd + 0x3
    .zero 4088

    .align 4096
boot_pd:
    .set page_flags, 0x83
    .quad 0x00000000 | page_flags
    .quad 0x00200000 | page_flags
    .quad 0x00400000 | page_flags
    .quad 0x00600000 | page_flags
    .quad 0x00800000 | page_flags
    .quad 0x00a00000 | page_flags
    .quad 0x00c00000 | page_flags
    .quad 0x00e00000 | page_flags
    .quad 0x01000000 | page_flags
    .quad 0x01200000 | page_flags
    .quad 0x01400000 | page_flags
    .quad 0x01600000 | page_flags
    .quad 0x01800000 | page_flags
    .quad 0x01a00000 | page_flags
    .quad 0x01c00000 | page_flags
    .quad 0x01e00000 | page_flags
    .quad 0x02000000 | page_flags
    .quad 0x02200000 | page_flags
    .quad 0x02400000 | page_flags
    .quad 0x02600000 | page_flags
    .quad 0x02800000 | page_flags
    .quad 0x02a00000 | page_flags
    .quad 0x02c00000 | page_flags
    .quad 0x02e00000 | page_flags
    .quad 0x03000000 | page_flags
    .quad 0x03200000 | page_flags
    .quad 0x03400000 | page_flags
    .quad 0x03600000 | page_flags
    .quad 0x03800000 | page_flags
    .quad 0x03a00000 | page_flags
    .quad 0x03c00000 | page_flags
    .quad 0x03e00000 | page_flags
    .quad 0x04000000 | page_flags
    .quad 0x04200000 | page_flags
    .quad 0x04400000 | page_flags
    .quad 0x04600000 | page_flags
    .quad 0x04800000 | page_flags
    .quad 0x04a00000 | page_flags
    .quad 0x04c00000 | page_flags
    .quad 0x04e00000 | page_flags
    .quad 0x05000000 | page_flags
    .quad 0x05200000 | page_flags
    .quad 0x05400000 | page_flags
    .quad 0x05600000 | page_flags
    .quad 0x05800000 | page_flags
    .quad 0x05a00000 | page_flags
    .quad 0x05c00000 | page_flags
    .quad 0x05e00000 | page_flags
    .quad 0x06000000 | page_flags
    .quad 0x06200000 | page_flags
    .quad 0x06400000 | page_flags
    .quad 0x06600000 | page_flags
    .quad 0x06800000 | page_flags
    .quad 0x06a00000 | page_flags
    .quad 0x06c00000 | page_flags
    .quad 0x06e00000 | page_flags
    .quad 0x07000000 | page_flags
    .quad 0x07200000 | page_flags
    .quad 0x07400000 | page_flags
    .quad 0x07600000 | page_flags
    .quad 0x07800000 | page_flags
    .quad 0x07a00000 | page_flags
    .quad 0x07c00000 | page_flags
    .quad 0x07e00000 | page_flags
    .quad 0x08000000 | page_flags
    .quad 0x08200000 | page_flags
    .quad 0x08400000 | page_flags
    .quad 0x08600000 | page_flags
    .quad 0x08800000 | page_flags
    .quad 0x08a00000 | page_flags
    .quad 0x08c00000 | page_flags
    .quad 0x08e00000 | page_flags
    .quad 0x09000000 | page_flags
    .quad 0x09200000 | page_flags
    .quad 0x09400000 | page_flags
    .quad 0x09600000 | page_flags
    .quad 0x09800000 | page_flags
    .quad 0x09a00000 | page_flags
    .quad 0x09c00000 | page_flags
    .quad 0x09e00000 | page_flags
    .quad 0x0a000000 | page_flags
    .quad 0x0a200000 | page_flags
    .quad 0x0a400000 | page_flags
    .quad 0x0a600000 | page_flags
    .quad 0x0a800000 | page_flags
    .quad 0x0aa00000 | page_flags
    .quad 0x0ac00000 | page_flags
    .quad 0x0ae00000 | page_flags
    .quad 0x0b000000 | page_flags
    .quad 0x0b200000 | page_flags
    .quad 0x0b400000 | page_flags
    .quad 0x0b600000 | page_flags
    .quad 0x0b800000 | page_flags
    .quad 0x0ba00000 | page_flags
    .quad 0x0bc00000 | page_flags
    .quad 0x0be00000 | page_flags
    .quad 0x0c000000 | page_flags
    .quad 0x0c200000 | page_flags
    .quad 0x0c400000 | page_flags
    .quad 0x0c600000 | page_flags
    .quad 0x0c800000 | page_flags
    .quad 0x0ca00000 | page_flags
    .quad 0x0cc00000 | page_flags
    .quad 0x0ce00000 | page_flags
    .quad 0x0d000000 | page_flags
    .quad 0x0d200000 | page_flags
    .quad 0x0d400000 | page_flags
    .quad 0x0d600000 | page_flags
    .quad 0x0d800000 | page_flags
    .quad 0x0da00000 | page_flags
    .quad 0x0dc00000 | page_flags
    .quad 0x0de00000 | page_flags
    .quad 0x0e000000 | page_flags
    .quad 0x0e200000 | page_flags
    .quad 0x0e400000 | page_flags
    .quad 0x0e600000 | page_flags
    .quad 0x0e800000 | page_flags
    .quad 0x0ea00000 | page_flags
    .quad 0x0ec00000 | page_flags
    .quad 0x0ee00000 | page_flags
    .quad 0x0f000000 | page_flags
    .quad 0x0f200000 | page_flags
    .quad 0x0f400000 | page_flags
    .quad 0x0f600000 | page_flags
    .quad 0x0f800000 | page_flags
    .quad 0x0fa00000 | page_flags
    .quad 0x0fc00000 | page_flags
    .quad 0x0fe00000 | page_flags
    .quad 0x10000000 | page_flags
    .quad 0x10200000 | page_flags
    .quad 0x10400000 | page_flags
    .quad 0x10600000 | page_flags
    .quad 0x10800000 | page_flags
    .quad 0x10a00000 | page_flags
    .quad 0x10c00000 | page_flags
    .quad 0x10e00000 | page_flags
    .quad 0x11000000 | page_flags
    .quad 0x11200000 | page_flags
    .quad 0x11400000 | page_flags
    .quad 0x11600000 | page_flags
    .quad 0x11800000 | page_flags
    .quad 0x11a00000 | page_flags
    .quad 0x11c00000 | page_flags
    .quad 0x11e00000 | page_flags
    .quad 0x12000000 | page_flags
    .quad 0x12200000 | page_flags
    .quad 0x12400000 | page_flags
    .quad 0x12600000 | page_flags
    .quad 0x12800000 | page_flags
    .quad 0x12a00000 | page_flags
    .quad 0x12c00000 | page_flags
    .quad 0x12e00000 | page_flags
    .quad 0x13000000 | page_flags
    .quad 0x13200000 | page_flags
    .quad 0x13400000 | page_flags
    .quad 0x13600000 | page_flags
    .quad 0x13800000 | page_flags
    .quad 0x13a00000 | page_flags
    .quad 0x13c00000 | page_flags
    .quad 0x13e00000 | page_flags
    .quad 0x14000000 | page_flags
    .quad 0x14200000 | page_flags
    .quad 0x14400000 | page_flags
    .quad 0x14600000 | page_flags
    .quad 0x14800000 | page_flags
    .quad 0x14a00000 | page_flags
    .quad 0x14c00000 | page_flags
    .quad 0x14e00000 | page_flags
    .quad 0x15000000 | page_flags
    .quad 0x15200000 | page_flags
    .quad 0x15400000 | page_flags
    .quad 0x15600000 | page_flags
    .quad 0x15800000 | page_flags
    .quad 0x15a00000 | page_flags
    .quad 0x15c00000 | page_flags
    .quad 0x15e00000 | page_flags
    .quad 0x16000000 | page_flags
    .quad 0x16200000 | page_flags
    .quad 0x16400000 | page_flags
    .quad 0x16600000 | page_flags
    .quad 0x16800000 | page_flags
    .quad 0x16a00000 | page_flags
    .quad 0x16c00000 | page_flags
    .quad 0x16e00000 | page_flags
    .quad 0x17000000 | page_flags
    .quad 0x17200000 | page_flags
    .quad 0x17400000 | page_flags
    .quad 0x17600000 | page_flags
    .quad 0x17800000 | page_flags
    .quad 0x17a00000 | page_flags
    .quad 0x17c00000 | page_flags
    .quad 0x17e00000 | page_flags
    .quad 0x18000000 | page_flags
    .quad 0x18200000 | page_flags
    .quad 0x18400000 | page_flags
    .quad 0x18600000 | page_flags
    .quad 0x18800000 | page_flags
    .quad 0x18a00000 | page_flags
    .quad 0x18c00000 | page_flags
    .quad 0x18e00000 | page_flags
    .quad 0x19000000 | page_flags
    .quad 0x19200000 | page_flags
    .quad 0x19400000 | page_flags
    .quad 0x19600000 | page_flags
    .quad 0x19800000 | page_flags
    .quad 0x19a00000 | page_flags
    .quad 0x19c00000 | page_flags
    .quad 0x19e00000 | page_flags
    .quad 0x1a000000 | page_flags
    .quad 0x1a200000 | page_flags
    .quad 0x1a400000 | page_flags
    .quad 0x1a600000 | page_flags
    .quad 0x1a800000 | page_flags
    .quad 0x1aa00000 | page_flags
    .quad 0x1ac00000 | page_flags
    .quad 0x1ae00000 | page_flags
    .quad 0x1b000000 | page_flags
    .quad 0x1b200000 | page_flags
    .quad 0x1b400000 | page_flags
    .quad 0x1b600000 | page_flags
    .quad 0x1b800000 | page_flags
    .quad 0x1ba00000 | page_flags
    .quad 0x1bc00000 | page_flags
    .quad 0x1be00000 | page_flags
    .quad 0x1c000000 | page_flags
    .quad 0x1c200000 | page_flags
    .quad 0x1c400000 | page_flags
    .quad 0x1c600000 | page_flags
    .quad 0x1c800000 | page_flags
    .quad 0x1ca00000 | page_flags
    .quad 0x1cc00000 | page_flags
    .quad 0x1ce00000 | page_flags
    .quad 0x1d000000 | page_flags
    .quad 0x1d200000 | page_flags
    .quad 0x1d400000 | page_flags
    .quad 0x1d600000 | page_flags
    .quad 0x1d800000 | page_flags
    .quad 0x1da00000 | page_flags
    .quad 0x1dc00000 | page_flags
    .quad 0x1de00000 | page_flags
    .quad 0x1e000000 | page_flags
    .quad 0x1e200000 | page_flags
    .quad 0x1e400000 | page_flags
    .quad 0x1e600000 | page_flags
    .quad 0x1e800000 | page_flags
    .quad 0x1ea00000 | page_flags
    .quad 0x1ec00000 | page_flags
    .quad 0x1ee00000 | page_flags
    .quad 0x1f000000 | page_flags
    .quad 0x1f200000 | page_flags
    .quad 0x1f400000 | page_flags
    .quad 0x1f600000 | page_flags
    .quad 0x1f800000 | page_flags
    .quad 0x1fa00000 | page_flags
    .quad 0x1fc00000 | page_flags
    .quad 0x1fe00000 | page_flags
    .zero 2048

    .align 8
gdt64:
    .quad 0x0000000000000000
    .quad 0x00af9a000000ffff
    .quad 0x00af92000000ffff
gdt64_end:

gdt64_ptr:
    .word gdt64_end - gdt64 - 1
    .long gdt64
    .long 0

    .section .text.boot32,"ax",@progbits
    .code32
    .global pvh_start32
    .type pvh_start32,@function
pvh_start32:
    cli
    mov esi, ebx
    mov esp, offset boot_stack_end
    mov eax, offset boot_pml4
    mov cr3, eax
    mov eax, cr4
    or eax, 0x20
    mov cr4, eax
    mov ecx, 0xC0000080
    rdmsr
    or eax, 0x100
    wrmsr
    lgdt [gdt64_ptr]
    mov eax, cr0
    or eax, 0x80000001
    mov cr0, eax
    push 0x08
    mov eax, offset long_mode_entry
    push eax
    retf

    .section .text.boot,"ax",@progbits
    .code64

    .global _start
    .type _start,@function
_start:
long_mode_entry:
    cli
    lea rsp, [rip + boot_stack_end]
    xor rbp, rbp
    mov edi, esi
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
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
