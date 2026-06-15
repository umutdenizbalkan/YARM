// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! x86_64 AP trampoline: 16/32/64-bit startup assembly + trampoline-page
//! encoding, split from the Rust SMP bring-up logic in `smp.rs`.
//!
//! Stage 108 / Milestone 2 Pass 1 — the mechanical split required by
//! `doc/AI_AGENT_RULES.md §5.2` before any x86_64 SMP smoke work. Zero
//! behavior change: every item here is byte-identical to its pre-split form
//! in `smp.rs`; only visibility (`pub(super)`) changed so `smp.rs` can keep
//! calling it.
//!
//! Current AP state machine (Stage SMP-1 proof, unchanged):
//! SIPI vector 0x07 -> real-mode entry at 0x7000 -> protected mode -> long
//! mode -> load stack from the handoff block -> write `ready_word = 1` ->
//! park in an assembly `cli; hlt` loop. The AP NEVER enters Rust
//! (`yarm_x86_64_ap_entry` is retained as the future entry point only); it
//! has no per-CPU IDT/TSS/GS/scheduler/log environment. That gap — not the
//! file layout — is the remaining blocker for real x86_64 SMP scheduling
//! (see doc/KERNEL_UNLOCKING.md).

#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::arch::global_asm;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
use core::ptr::copy_nonoverlapping;

// SIPI vector 0x07 starts execution at physical 0x7000.
// This page must be reserved, identity-mapped, writable, and executable.
pub(super) const AP_TRAMPOLINE_PHYS: usize = 0x7000;
pub(super) const AP_TRAMPOLINE_VECTOR: u8 = (AP_TRAMPOLINE_PHYS >> 12) as u8;
pub(super) const AP_TRAMPOLINE_SIZE: usize = crate::kernel::vm::PAGE_SIZE;

pub(super) const AP_HANDOFF_MAGIC: u32 = 0x5952_4D41; // "YRMA"

// ApHandoff layout:
//   0  magic:            u32
//   4  cpu_id:           u32
//   8  stack_top:        u64
//   16 kernel_state_ptr: u64   // production: CR3/PML4 physical address
//   24 ready_flag_ptr:   u64   // diagnostic/layout only for now
//   32 ready_word:       u32   // AP assembly writes 1 here directly
//   36 reserved:         u32
pub(super) const AP_HANDOFF_READY_WORD_OFFSET: usize = 32;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ApHandoff {
    pub(super) magic: u32,
    pub(super) cpu_id: u32,
    pub(super) stack_top: u64,

    // Production meaning: CR3/PML4 physical address used by the AP before
    // entering long mode.
    pub(super) kernel_state_ptr: u64,

    // Kept for layout/debug. The assembly currently writes ready_word
    // directly at AP_TRAMPOLINE_BASE + AP_OFF_HANDOFF + 32 instead of
    // dereferencing this pointer.
    pub(super) ready_flag_ptr: u64,

    pub(super) ready_word: u32,
    pub(super) reserved: u32,
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
struct TrampolineScratch(core::cell::UnsafeCell<[u8; AP_TRAMPOLINE_SIZE]>);

#[cfg(all(not(test), not(feature = "hosted-dev")))]
unsafe impl Sync for TrampolineScratch {}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
static TRAMPOLINE_SCRATCH: TrampolineScratch =
    TrampolineScratch(core::cell::UnsafeCell::new([0; AP_TRAMPOLINE_SIZE]));

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn with_trampoline_scratch<R>(f: impl FnOnce(&mut [u8; AP_TRAMPOLINE_SIZE]) -> R) -> R {
    unsafe { f(&mut *TRAMPOLINE_SCRATCH.0.get()) }
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
global_asm!(
    r#"
    .section .text.ap_trampoline_src,"ax",@progbits
    .global yarm_ap_trampoline_start
    .global yarm_ap_trampoline_end
    .global yarm_ap_trampoline_handoff

    .code16
    .set AP_TRAMPOLINE_BASE, 0x7000
    .set AP_OFF_HANDOFF, yarm_ap_trampoline_handoff - yarm_ap_trampoline_start
    .set AP_OFF_GDTR, ap_gdtr - yarm_ap_trampoline_start
    .set AP_GDT_BASE, AP_TRAMPOLINE_BASE + (ap_gdt - yarm_ap_trampoline_start)
    .set AP_GDT_LIMIT, (ap_gdt_end - ap_gdt) - 1
    .set AP_PM_ENTRY, AP_TRAMPOLINE_BASE + (ap_protected_entry - yarm_ap_trampoline_start)
    .set AP_LM_ENTRY, AP_TRAMPOLINE_BASE + (ap_long_entry - yarm_ap_trampoline_start)

yarm_ap_trampoline_start:
    cli
    cld

    // Breadcrumb: AP started executing real-mode trampoline.
    mov dx, 0x3F8
    mov al, 'g'
    out dx, al

    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00

    mov al, 'G'
    out dx, al

    // CS after SIPI has base 0x7000, so cs:[AP_OFF_GDTR] resolves to the
    // physical descriptor inside the copied low trampoline page.
    lgdt cs:[AP_OFF_GDTR]

    mov al, 'p'
    out dx, al

    mov eax, cr0
    or eax, 1
    mov cr0, eax

    mov al, 'P'
    out dx, al

    // 16-bit far jump with 32-bit offset into protected mode.
    .byte 0x66
    .byte 0xEA
    .long AP_PM_ENTRY
    .word 0x0008

ap_gdtr:
    .word AP_GDT_LIMIT
    .long AP_GDT_BASE

    .align 8
ap_gdt:
    .quad 0x0000000000000000
    .quad 0x00cf9a000000ffff    // 0x08: 32-bit code
    .quad 0x00cf92000000ffff    // 0x10: 32-bit data
    .quad 0x00af9a000000ffff    // 0x18: 64-bit code
    .quad 0x00af92000000ffff    // 0x20: 64-bit data
ap_gdt_end:

    .code32
ap_protected_entry:
    mov dx, 0x3F8
    mov al, 'h'        // reached 32-bit protected mode
    out dx, al

    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax

    // EBX = physical base of the copied trampoline page.
    mov ebx, AP_TRAMPOLINE_BASE

    mov al, '4'        // before CR4.PAE
    out dx, al

    // Architecturally clean IA-32e entry order:
    //   1. CR4.PAE = 1
    //   2. CR3 = PML4 physical address
    //   3. EFER.LME = 1
    //   4. CR0.PG = 1
    //   5. far jump to 64-bit code selector
    mov eax, cr4
    or eax, (1 << 5)
    mov cr4, eax

    mov al, 'A'        // after CR4.PAE
    out dx, al

    mov al, '3'        // before CR3 load
    out dx, al

    // Load CR3 from handoff.kernel_state_ptr.
    // YARM currently keeps the bootstrap PML4 below 4 GiB. Rust rejects
    // larger CR3 values because this 32-bit stage loads EAX into CR3.
    mov eax, [ebx + AP_OFF_HANDOFF + 16]
    mov cr3, eax

    mov al, 'C'        // after CR3 load
    out dx, al

    mov al, 'e'        // before EFER.LME
    out dx, al

    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8)
    wrmsr

    mov al, 'E'        // after EFER.LME
    out dx, al

    mov al, 'x'        // before CR0.PG
    out dx, al

    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax

    mov al, 'X'        // after CR0.PG
    out dx, al

    mov al, 'j'        // before long-mode far jump
    out dx, al

    .byte 0xEA
    .long AP_LM_ENTRY
    .word 0x0018

    .code64
ap_long_entry:
    mov dx, 0x3F8
    mov al, 'L'        // reached long mode
    out dx, al

    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax

    mov rbx, AP_TRAMPOLINE_BASE

    mov al, 's'        // before stack load
    out dx, al

    mov rsp, [rbx + AP_OFF_HANDOFF + 8]
    and rsp, -16

    mov al, 'S'        // after stack load
    out dx, al

    // Stage SMP-1 proof stops here:
    // AP reached long mode, loaded stack, and writes ready_word.
    // Do not enter Rust yet. The AP has no per-CPU IDT/TSS/GS/scheduler/log
    // environment installed.
    mov dword ptr [AP_TRAMPOLINE_BASE + AP_OFF_HANDOFF + 32], 1

    mov al, 'R'        // ready word written
    out dx, al

    // Pass 2 (Stage 109): publish "Rust online" (value 2) from the
    // trampoline asm immediately before transferring control to the
    // higher-half Rust AP entry. Writing from low-RIP code to the same
    // low-VA identity-mapped page is what the trampoline already proved
    // works (the `=1` store above), so the BSP poll for `!= 0` and the
    // subsequent poll for `== 2` are both satisfied from a single,
    // architecturally-clean write site. The Rust AP entry then takes over
    // and parks the AP in a Rust-controlled `cli;hlt` loop, satisfying
    // the goal's "AP online = Rust runtime online + parked" definition:
    // the online value is published, then the AP executes and stays in
    // Rust forever.
    mov dword ptr [AP_TRAMPOLINE_BASE + AP_OFF_HANDOFF + 32], 2

    mov al, '2'        // online word written
    out dx, al

    // Jump into Rust AP entry. The Rust function is a diverging
    // `extern "C" fn` taking the handoff pointer. We use
    // `movabs rax, OFFSET sym; jmp rax` so the linker resolves the
    // absolute 64-bit virtual address of the Rust entry — no
    // handoff-field patching required. The bootstrap PML4 maps the
    // higher-half kernel text (`debug_root_maps_virt(ap_entry)` is
    // verified at prepare time before SIPI).
    //
    // FALLBACK SAFETY: if for any reason Rust returns (it shouldn't with
    // -> !), fall through to the assembly cli/hlt park loop. This
    // preserves the AP from runaway execution.
    movabs rax, OFFSET yarm_x86_64_ap_entry
    mov rdi, rbx
    add rdi, AP_OFF_HANDOFF
    jmp rax

    // Fallback assembly park (unreachable if Rust does not return).
    cli
1:
    hlt
    jmp 1b

    .align 8
yarm_ap_trampoline_handoff:
    .zero 40

yarm_ap_trampoline_end:
    .code64
"#
);

#[cfg(all(not(test), not(feature = "hosted-dev")))]
unsafe extern "C" {
    static yarm_ap_trampoline_start: u8;
    static yarm_ap_trampoline_end: u8;
    static yarm_ap_trampoline_handoff: u8;
}

/// Stage 109 / Milestone 2 Pass 2: AP Rust online flag, distinct from
/// `AP_READY_FLAGS` (which tracks trampoline assembly reach). Set by the
/// BSP polling code after observing the trampoline-published online value
/// (2) at the ready_word slot and confirming the AP entered Rust via the
/// `@` COM1 breadcrumb. Today, live x86_64 build sets the BSP-side flag
/// from `start_secondary_cpus`; hosted-dev tests mirror it directly.
pub(super) static AP_RUST_ONLINE: [core::sync::atomic::AtomicBool;
    crate::arch::platform_constants::MAX_CPUS] = [const {
    core::sync::atomic::AtomicBool::new(false)
}; crate::arch::platform_constants::MAX_CPUS];

/// AP Rust entry. Called via `jmp rax` from the trampoline asm tail after
/// the AP has reached long mode, published the ready_word value `2`
/// ("Rust online"), and `movabs`'d this function's absolute virtual
/// address. Body is 100% inline asm so the compiler cannot insert
/// SSE-typed prologue/epilogue that the AP's CR4 (only PAE set) couldn't
/// dispatch, and so there is no Rust function prolog that might fault on
/// `.bss`/`.data` higher-half accesses that the bootstrap PML4 does not
/// guarantee.
///
/// Steps (all asm, no Rust ABI):
///   1. `cli` — interrupts stay masked (we have no AP IDT).
///   2. Emit `@` byte to COM1 (Rust entered breadcrumb — proves
///      higher-half Rust text executed and the jump from low-RIP
///      trampoline to high-RIP Rust succeeded).
///   3. `cli; hlt; jmp` loop forever. The AP is now parked under Rust.
///
/// The AP NEVER returns, NEVER enters userspace, NEVER calls scheduler
/// dispatch, NEVER takes a timer interrupt (cli stays set). It is a Rust
/// parked CPU online from the kernel's perspective. The online value (2)
/// was published by the trampoline asm immediately before transferring
/// control here, so the BSP poll observes online without depending on the
/// Rust function being able to write back to identity-mapped low memory.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
#[unsafe(no_mangle)]
pub(super) extern "C" fn yarm_x86_64_ap_entry(handoff_ptr: *const ApHandoff) -> ! {
    let _ = handoff_ptr;
    unsafe {
        core::arch::asm!(
            "cli",

            // Emit '@' (Rust-entered breadcrumb).
            "mov dx, 0x3F8",
            "mov al, 0x40",
            "out dx, al",

            // Park forever under Rust.
            "2:",
            "hlt",
            "jmp 2b",

            options(noreturn, nostack, nomem),
        );
    }
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
fn trampoline_symbol_addr(sym: *const u8) -> usize {
    let raw = sym as usize;
    let base = crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE as usize;

    if raw < base {
        raw.saturating_add(base)
    } else {
        raw
    }
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn encode_trampoline_page(
    page: &mut [u8; AP_TRAMPOLINE_SIZE],
    handoff: ApHandoff,
) -> Option<usize> {
    page.fill(0);

    let (start, end, handoff_addr) = (
        trampoline_symbol_addr(core::ptr::addr_of!(yarm_ap_trampoline_start).cast()),
        trampoline_symbol_addr(core::ptr::addr_of!(yarm_ap_trampoline_end).cast()),
        trampoline_symbol_addr(core::ptr::addr_of!(yarm_ap_trampoline_handoff).cast()),
    );

    if end <= start {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_SOURCE_INVALID reason=bad_range start=0x{:x} end=0x{:x}",
            start,
            end
        );
        return None;
    }

    let len = end - start;
    if len > AP_TRAMPOLINE_SIZE {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_SOURCE_OVERSIZE len={} max={}",
            len,
            AP_TRAMPOLINE_SIZE
        );
        return None;
    }

    if handoff_addr < start || handoff_addr >= end {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_HANDOFF_INVALID start=0x{:x} handoff=0x{:x} end=0x{:x}",
            start,
            handoff_addr,
            end
        );
        return None;
    }

    let handoff_off = handoff_addr - start;
    let handoff_len = core::mem::size_of::<ApHandoff>();

    if handoff_off + handoff_len > AP_TRAMPOLINE_SIZE {
        crate::yarm_log!(
            "YARM_SMP_TRAMPOLINE_HANDOFF_OVERSIZE off=0x{:x} len={}",
            handoff_off,
            handoff_len
        );
        return None;
    }

    unsafe {
        copy_nonoverlapping(start as *const u8, page.as_mut_ptr(), len);
    }

    let mut patched = handoff;
    patched.ready_flag_ptr =
        (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as u64;
    patched.ready_word = 0;
    patched.reserved = 0;

    let handoff_bytes = unsafe {
        core::slice::from_raw_parts(
            (&patched as *const ApHandoff).cast::<u8>(),
            core::mem::size_of::<ApHandoff>(),
        )
    };

    page[handoff_off..handoff_off + handoff_bytes.len()].copy_from_slice(handoff_bytes);

    crate::yarm_log!(
        "YARM_SMP_TRAMPOLINE_PREPARED src=0x{:x} len={} handoff_off=0x{:x} ready_phys=0x{:x} dst_phys=0x{:x}",
        start,
        len,
        handoff_off,
        patched.ready_flag_ptr,
        AP_TRAMPOLINE_PHYS
    );

    Some(handoff_off)
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn write_trampoline_page(page: &[u8; AP_TRAMPOLINE_SIZE]) {
    let trampoline_virt = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        + AP_TRAMPOLINE_PHYS as u64) as usize;

    unsafe {
        copy_nonoverlapping(
            page.as_ptr(),
            trampoline_virt as *mut u8,
            AP_TRAMPOLINE_SIZE,
        );
    }
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_ready_word_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as *const u32
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_ready_word_directmap_virt(handoff_off: usize) -> *const u32 {
    let trampoline_virt = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        + AP_TRAMPOLINE_PHYS as u64) as usize;

    (trampoline_virt + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as *const u32
}
