// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! x86_64 AP trampoline: 16/32/64-bit startup assembly + trampoline-page
//! encoding, split from the Rust SMP bring-up logic in `smp.rs`.
//!
//! Originally split out in Stage 108 / Milestone 2 Pass 1 per
//! `doc/AI_AGENT_RULES.md §5.2`; `smp.rs` keeps the Rust bring-up logic and
//! this file owns the trampoline asm + the naked AP entry.
//!
//! Current AP state machine (Stage 183 increment 3):
//! SIPI vector 0x07 -> real-mode entry at 0x7000 -> protected mode -> long
//! mode -> stack from the handoff -> ready_word=1 then 2 -> jmp into the
//! NAKED Rust entry `yarm_x86_64_ap_entry` (`@` breadcrumb) -> GS base
//! wrmsr + rdmsr verify -> kernel-CR3 reload + .bss canary -> per-AP
//! lgdt + kernel CS/SS reload -> ltr (per-AP TSS) -> LAPIC ID readback ->
//! idle-context publish -> ready_word=3 -> interrupt-masked cli/hlt IDLE.
//! The AP has NO IDT and takes NO interrupts; it is NOT scheduler-runnable
//! (`online_cpu_count()` stays 1). Every step publishes a breadcrumb byte +
//! a low-memory stage word so a BSP admit-poll timeout names the exact
//! failing transition (see the trace-map table below and
//! doc/KERNEL_UNLOCKING.md Stage 183).

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
//   0  magic:             u32
//   4  cpu_id:            u32
//   8  stack_top:         u64
//   16 kernel_state_ptr:  u64   // production: CR3/PML4 physical address
//   24 ready_flag_ptr:    u64   // diagnostic/layout only for now
//   32 ready_word:        u32   // AP assembly writes 1/2/3/254 here directly
//   36 reserved:          u32
//   40 percpu_record_ptr: u64   // Stage 183 inc.2: AP GS base (low identity handoff)
//   48 ap_stage:          u32   // Stage 183 inc.2: fine-grained AP stage trace word
//   52 <pad>:             u32
//   56 kernel_cr3:        u64   // Stage 183 inc.3: full kernel CR3 for the controlled reload
//   64 gdtr_image:        [u8;10] // inc.3: per-AP GDTR (limit u16 + base u64, LE) for lgdt
//   74 <pad>:             [u8;6]
//   80 lapic_id_reg_va:   u64   // inc.3: VA of the LAPIC ID register (0 = skip the read)
//   88 env_flags:         u32   // inc.3: AP-written env-step bitmask (see AP_ENV_* bits)
//   92 lapic_id_out:      u32   // inc.3: AP-written LAPIC ID readback (0xFFFF_FFFF = unread)
//   96 idtr_image:        [u8;10] // inc.4: AP IDTR (limit u16 + base u64, LE) for lidt
//   106 <pad>:            [u8;6]
//   112 bsp_cr4:          u64  // inc.4: BSP CR4 for the AP control-state sync (0 = skip)
//   120 svr_out:          u32  // inc.4 fix: AP-written SVR readback after sw-enable
//   124 tpr_out:          u32  // inc.4 fix: AP-written TPR readback
//   128 esr_out:          u32  // inc.4 fix: AP-written ESR readback after write-clear
//   132 wake_reenter_out: u32  // 183.5: AP-written count of sched-idle wake re-entries
//   → size_of == 136
pub(super) const AP_HANDOFF_READY_WORD_OFFSET: usize = 32;

// Stage 183 inc.2: the AP writes an incrementing stage code here (offset 48) before
// every risky action, so a BSP admit-poll timeout can report the LAST stage the AP
// reached (`last_stage`) instead of only "it didn't finish". Distinct from ready_word
// (offset 32), which stays the coarse online(2)/admit(3)/gs_bad(254) signal.
pub(super) const AP_HANDOFF_STAGE_WORD_OFFSET: usize = 48;

// Stage 183 inc.3: AP-written env-step results (offsets hardcoded in the entry asm).
pub(super) const AP_HANDOFF_ENV_FLAGS_OFFSET: usize = 88;
pub(super) const AP_HANDOFF_LAPIC_ID_OUT_OFFSET: usize = 92;
// Stage 183 inc.4 fix: AP-written LAPIC interrupt-delivery readiness readbacks.
pub(super) const AP_HANDOFF_SVR_OUT_OFFSET: usize = 120;
pub(super) const AP_HANDOFF_TPR_OUT_OFFSET: usize = 124;
pub(super) const AP_HANDOFF_ESR_OUT_OFFSET: usize = 128;
// Stage 183.5: AP-written scheduler-idle wake re-entry count.
pub(super) const AP_HANDOFF_WAKE_REENTER_OFFSET: usize = 132;

/// Stage 183 inc.3: bits the AP entry asm ORs into `env_flags` ([handoff+88], low
/// identity memory — always writable) as each per-CPU environment step completes.
/// The BSP admit-poll reads them to emit the per-step OK/FAIL markers.
pub(super) mod ap_env {
    pub const KERNEL_CR3_RELOADED: u32 = 1 << 0; // mov cr3 + post-reload fetch survived
    pub const GDT_LOADED: u32 = 1 << 1; // lgdt + SS/DS/ES + far-return CS=0x08
    pub const TSS_LOADED: u32 = 1 << 2; // ltr 0x28 (per-AP TSS)
    pub const LAPIC_READ: u32 = 1 << 3; // LAPIC ID register MMIO read done
    pub const IDLE_CTX_PUBLISHED: u32 = 1 << 4; // canary + live rsp stored via gs:
    // Stage 183 inc.4 (interrupt-safe idle):
    pub const CR4_SYNCED: u32 = 1 << 5; // mov cr4 = handoff.bsp_cr4 survived
    pub const IDT_LOADED: u32 = 1 << 6; // lidt of the AP-safe IDT done
    // Stage 183 inc.4 fix (183.4 host failure — LAPIC was software-DISABLED after
    // INIT, dropping fixed IPIs): AP wrote SVR (sw-enable | spurious 0xFF), TPR=0,
    // ESR write-clear, and published the readbacks.
    pub const LAPIC_SW_ENABLED: u32 = 1 << 7;
}

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

    // Stage 183 increment 2 (AP idle admission): the per-CPU record base VA for THIS AP,
    // computed BSP-side and passed via the (low, identity-mapped) handoff so the AP can
    // `wrmsr IA32_GS_BASE` with it WITHOUT touching higher-half `.bss` (the bootstrap PML4
    // the AP runs on maps only text + low identity). Offset 40 in the struct.
    pub(super) percpu_record_ptr: u64,

    // Stage 183 increment 2 (AP idle admission): fine-grained stage trace. The AP entry
    // asm writes an `AP_STAGE_*` code here (offset 48) before every risky action so the
    // BSP admit-poll can name the last stage the AP reached on timeout.
    pub(super) ap_stage: u32,
    pub(super) _pad_stage: u32,

    // Stage 183 increment 3 (scheduler-admission prerequisites), all BSP-written unless
    // noted. kernel_cr3: the full kernel CR3 the AP reloads (controlled transition; the
    // kernel address space maps text + low identity + .bss + LAPIC MMIO — everything the
    // BSP itself uses). gdtr_image: the 10-byte GDTR (limit LE16 + base LE64) of this
    // AP's per-CPU GDT for `lgdt [rdi+64]`. lapic_id_reg_va: VA of the LAPIC ID register
    // (0 ⇒ AP skips the MMIO read). env_flags / lapic_id_out: AP-written results.
    pub(super) kernel_cr3: u64,
    pub(super) gdtr_image: [u8; 10],
    pub(super) _pad_gdtr: [u8; 6],
    pub(super) lapic_id_reg_va: u64,
    pub(super) env_flags: u32,
    pub(super) lapic_id_out: u32,

    // Stage 183 increment 4 (interrupt-safe idle), BSP-written: the AP-safe IDT's
    // GDTR-style image (limit LE16 + base LE64) for `lidt [rdi+96]`, and the BSP's
    // CR4 value the AP mirrors (`mov cr4`) so its control state converges on the
    // BSP's (PGE/OSFXSR/…; 0 ⇒ skip). The IDT gates are pure-asm gs:-recording
    // stubs — see descriptor_tables.rs `prepare_ap_idt`.
    pub(super) idtr_image: [u8; 10],
    pub(super) _pad_idtr: [u8; 6],
    pub(super) bsp_cr4: u64,

    // Stage 183 inc.4 fix, AP-written: LAPIC interrupt-delivery readiness readbacks
    // (SVR after software-enable, TPR after =0, ESR after write-clear). The BSP grades
    // them into X86_AP_LAPIC_SVR_OK / TPR_OK / ESR_OK / INTERRUPT_READY.
    pub(super) svr_out: u32,
    pub(super) tpr_out: u32,
    pub(super) esr_out: u32,
    // Stage 183.5, AP-written: incremented each time the managed scheduler-idle
    // loop observes a remote wake (handler ran) and re-enters idle. The BSP wake
    // proof requires exactly one increment per sent wake (no lost/dup).
    pub(super) wake_reenter_out: u32,
}

// Stage 183 inc.2/inc.3 layout guard: the trampoline / AP-entry asm hardcodes these
// field offsets ([rdi+32/40/48/56/64/80/88/92]) and the `.zero 96` handoff reservation.
// Lock the Rust struct to them at compile time so a field reorder can never silently
// make the asm read/write the wrong slot.
const _: () = {
    assert!(core::mem::size_of::<ApHandoff>() == 136);
    assert!(core::mem::offset_of!(ApHandoff, ready_word) == AP_HANDOFF_READY_WORD_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, ready_word) == 32);
    assert!(core::mem::offset_of!(ApHandoff, percpu_record_ptr) == 40);
    assert!(core::mem::offset_of!(ApHandoff, ap_stage) == AP_HANDOFF_STAGE_WORD_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, ap_stage) == 48);
    assert!(core::mem::offset_of!(ApHandoff, kernel_cr3) == 56);
    assert!(core::mem::offset_of!(ApHandoff, gdtr_image) == 64);
    assert!(core::mem::offset_of!(ApHandoff, lapic_id_reg_va) == 80);
    assert!(core::mem::offset_of!(ApHandoff, env_flags) == AP_HANDOFF_ENV_FLAGS_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, env_flags) == 88);
    assert!(core::mem::offset_of!(ApHandoff, lapic_id_out) == AP_HANDOFF_LAPIC_ID_OUT_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, lapic_id_out) == 92);
    assert!(core::mem::offset_of!(ApHandoff, idtr_image) == 96);
    assert!(core::mem::offset_of!(ApHandoff, bsp_cr4) == 112);
    assert!(core::mem::offset_of!(ApHandoff, svr_out) == AP_HANDOFF_SVR_OUT_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, svr_out) == 120);
    assert!(core::mem::offset_of!(ApHandoff, tpr_out) == 124);
    assert!(core::mem::offset_of!(ApHandoff, esr_out) == AP_HANDOFF_ESR_OUT_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, esr_out) == 128);
    assert!(core::mem::offset_of!(ApHandoff, wake_reenter_out) == AP_HANDOFF_WAKE_REENTER_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, wake_reenter_out) == 132);
};

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

    // Stage 183 inc.2 fix: publish stage 9 (rust_jump) through the KNOWN absolute
    // low address BEFORE transferring to Rust — deliberately NOT via rdi, so this
    // stage is recorded even if the jump target or the register handoff is broken.
    // last_stage=rust_jump on a BSP timeout then means "died between jmp rax and
    // the Rust entry's first store".
    mov dword ptr [AP_TRAMPOLINE_BASE + AP_OFF_HANDOFF + 48], 9

    mov al, '>'        // about to jmp into the Rust AP entry
    out dx, al

    // Jump into Rust AP entry. The Rust function is a diverging NAKED
    // `extern "C" fn` taking the handoff pointer in rdi per the SysV
    // calling convention. Because the entry is naked (no compiler
    // prologue), the `mov rdi, ...` below IS the argument-passing
    // contract — the first instruction of the entry sees rdi exactly as
    // set here (same transfer pattern as Redox's trampoline→kstart_ap).
    // We use `movabs rax, OFFSET sym; jmp rax` so the linker resolves the
    // absolute 64-bit virtual address of the Rust entry — no
    // handoff-field patching required. The bootstrap PML4 maps the
    // higher-half kernel text (`debug_root_maps_virt(ap_entry)` is
    // verified at prepare time before SIPI).
    //
    // FALLBACK SAFETY: if for any reason Rust returns (it shouldn't with
    // -> !), fall through to the assembly cli/hlt park loop. This
    // preserves the AP from runaway execution.
    //
    // Stage 183 inc.2 root cause: `add rdi, AP_OFF_HANDOFF` (bare, no OFFSET)
    // assembled as `add rdi, QWORD PTR [0x140]` — GAS Intel syntax treats a
    // bare symbol-difference `.set` as a MEMORY operand, so rdi got
    // 0x7000 + <8 bytes of BIOS IVT junk at phys 0x140> instead of
    // 0x7000 + 0x140. The first store through rdi in the Rust entry then
    // faulted (no AP IDT → triple fault → silence after '@'). `OFFSET`
    // forces the immediate, same idiom as the movabs above. Pure-numeric
    // `.set`s (AP_TRAMPOLINE_BASE) and bracketed uses were never affected.
    movabs rax, OFFSET yarm_x86_64_ap_entry
    mov rdi, rbx
    add rdi, OFFSET AP_OFF_HANDOFF
    jmp rax

    // Fallback assembly park (unreachable if Rust does not return).
    cli
1:
    hlt
    jmp 1b

    .align 8
yarm_ap_trampoline_handoff:
    // Reserve the FULL ApHandoff (size_of::<ApHandoff>() == 120): magic(0) cpu_id(4)
    // stack_top(8) kernel_state_ptr(16) ready_flag_ptr(24) ready_word(32) reserved(36)
    // percpu_record_ptr(40) ap_stage(48) pad(52) kernel_cr3(56) gdtr_image(64) pad(74)
    // lapic_id_reg_va(80) env_flags(88) lapic_id_out(92) idtr_image(96) pad(106)
    // bsp_cr4(112) svr_out(120) tpr_out(124) esr_out(128) pad(132). Under-reserving
    // would place AP-written fields past `yarm_ap_trampoline_end`, shrinking the
    // copied `len`.
    .zero 136

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
    crate::arch::platform_constants::MAX_CPUS] = [const { core::sync::atomic::AtomicBool::new(false) };
    crate::arch::platform_constants::MAX_CPUS];

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
/// Stage 183 increment 2 (AP idle admission): internal proof path that admits an AP to a
/// GS-initialized, interrupt-masked Rust idle loop instead of a bare park. This is NOT a
/// user boot knob and selects NO production fallback; it only chooses whether the AP does
/// the GS-base MSR init before idling. `-smp 1` boots have no APs, so this never runs
/// there — production behavior is unchanged. TSS / local-APIC / APIC-timer stay DEFERRED
/// this increment (they require a CR3 switch to the full kernel map + MMIO, which the
/// bootstrap-PML4-only AP cannot safely do yet), so the AP idles with interrupts masked.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_IDLE_ADMIT_PROOF: bool = true;

/// AP ready_word stage values (published in the low, identity-mapped handoff slot at
/// offset 32 so the BSP can poll them without any AP-side higher-half access).
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_RUST_ONLINE: u32 = 2; // set by the trampoline asm before entry
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IDLE_ADMIT_OK: u32 = 3; // GS written+verified, entered idle
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IDLE_ADMIT_GS_BAD: u32 = 254; // GS readback mismatch (still idles)

// ---------------------------------------------------------------------------
// Stage 183 increment 2 — AP breadcrumb ⇄ stage trace map (offset 48 stage word)
// ---------------------------------------------------------------------------
// The AP path emits a serial breadcrumb byte to COM1 (port 0x3F8) AND writes a
// numeric AP_STAGE_* code to the low-memory stage word at [handoff+48] before
// every risky action. The breadcrumb bytes are INTENTIONAL, not corruption; the
// stage word turns the interleaved serial stream into a deterministic trace the
// BSP can name on timeout (`last_stage=<name> last_stage_raw=<hex>`).
//
// Trampoline breadcrumbs (pre-Rust, no stage word — asm in the global_asm! above):
//   byte  asm block / instruction boundary          meaning / precondition
//   'g'   yarm_ap_trampoline_start (real mode)       AP executing @ phys 0x7000
//   'G'   after ds/es/ss=0, sp=0x7c00                real-mode segments loaded
//   'p'   after `lgdt cs:[AP_OFF_GDTR]`              GDTR loaded from low page
//   'P'   after CR0.PE=1                              protected mode enabled
//   'h'   ap_protected_entry (.code32)               reached 32-bit protected mode
//   '4'   before CR4.PAE                              about to set PAE
//   'A'   after CR4.PAE=1                             PAE on (no other CR4 bits!)
//   '3'   before CR3 load                             about to load handoff.kernel_state_ptr
//   'C'   after `mov cr3, eax`                        bootstrap PML4 installed
//   'e'   before EFER.LME                             about to enable long mode
//   'E'   after EFER.LME=1                            IA-32e mode enabled
//   'x'   before CR0.PG                               about to enable paging
//   'X'   after CR0.PG=1                              paging on
//   'j'   before long-mode far jump                   about to jump to .code64
//   'L'   ap_long_entry (.code64)                     reached 64-bit long mode
//   's'   before stack load                           about to load handoff.stack_top
//   'S'   after `mov rsp,[..8]; and rsp,-16`          AP stack installed + aligned
//   'R'   after `[handoff+32]=1`                      ready_word=1 (trampoline reached)
//   '2'   after `[handoff+32]=2`                      ready_word=2 (Rust online published)
//   '>'   AP_STAGE_RUST_JUMP (9) written via the      about to `movabs rax,OFFSET
//         ABSOLUTE low address (not rdi)              yarm_x86_64_ap_entry; jmp rax`
//
// Rust AP entry breadcrumbs (paired with stage word [handoff+48], naked_asm in
// yarm_x86_64_ap_entry below). "failure after this byte" = the AP died between
// this stage and the next expected one:
//   byte  stage word                     next expected   failure-after meaning
//   '@'   AP_STAGE_RUST_ENTERED (10)      'H'             higher-half text ran; died before handoff load
//         (failure BETWEEN '>' and '@' → last_stage=rust_jump: the jmp target or
//          the rdi register handoff broke — see the naked-entry contract below)
//   'H'   AP_STAGE_HANDOFF_LOADED (11)    'V'/'!'         read [rdi+40]; died before null check
//   'V'   AP_STAGE_HANDOFF_VALIDATED (12) 'W'             percpu_record_ptr non-null; died before wrmsr
//   'W'   AP_STAGE_BEFORE_WRMSR (13)      'w'             about to wrmsr IA32_GS_BASE; #GP/fault in wrmsr
//   'w'   AP_STAGE_AFTER_WRMSR (14)       'r'             wrmsr returned; died before rdmsr readback
//   'r'   AP_STAGE_AFTER_RDMSR (15)       'O'/'B'         rdmsr returned; died before compare
//   'O'   AP_STAGE_GS_VERIFIED (16)       'K'             GS readback == written; env steps begin
//   'B'   AP_STAGE_GS_MISMATCH (254)      'I'             GS readback != written (skips env; idles)
//   '!'   AP_STAGE_HANDOFF_NULL (253)     'I'             percpu_record_ptr was 0 (BSP handoff bug)
//
// Stage 183 inc.3 env steps (between 'O' and 'I'; skipped on the 'B'/'!' paths):
//   'K'   AP_STAGE_KCR3_BEGIN (19)        'k'             about to `mov cr3, handoff.kernel_cr3`;
//                                                         died ⇒ kernel CR3 does not map this text
//   'k'   AP_STAGE_KCR3_LIVE (20)         'D'             kernel CR3 live (post-reload fetch + low
//                                                         store worked); died ⇒ gs: canary store
//                                                         hit unmapped .bss under the kernel CR3
//   'D'   AP_STAGE_GDT_LOADED (21)        'T'             about to lgdt + SS/DS/ES + far-return to
//                                                         CS=0x08; died ⇒ per-AP GDT/CS reload bad
//   'T'   AP_STAGE_TSS_LOADED (22)        'l'             about to `ltr 0x28`; died ⇒ TSS desc bad
//   'l'   AP_STAGE_LAPIC_CHECKED (23)     'n'             about to read the LAPIC ID register;
//                                                         died ⇒ LAPIC MMIO VA bad under kCR3
//   'n'   AP_STAGE_LAPIC_ENABLED (29)     'i'             about to SW-ENABLE the LAPIC (SVR
//                                                         0x1FF, TPR 0, ESR clear) — fixed IPIs
//                                                         are DROPPED until this (183.4 root
//                                                         cause); died ⇒ LAPIC MMIO write bad
//   'i'   AP_STAGE_IDT_LOADED (26)        'y'             about to lidt the AP-safe IDT
//   'y'   AP_STAGE_IDLE_CTX_PUBLISHED (24) 'u'            about to store the live rsp via gs:
//   'u'   AP_STAGE_IRQ_SMOKE_WAIT (27)    'v'             ready_word=3 published; waiting in the
//                                                         race-free sti;hlt window for the BSP's
//                                                         one controlled smoke IPI (vector 0xF0)
//   'v'   AP_STAGE_IRQ_SMOKE_DONE (28)    'q'             smoke handled (gs: count+vector, EOI,
//                                                         iretq); interrupts masked again
//   'q'   AP_STAGE_SCHED_IDLE (30)        'z'/loop        183.5 scheduler-owned interruptible idle
//                                                         (current=idle tid 0; sti;hlt; wake-capable)
//   'z'   AP_STAGE_SCHED_WAKE_REENTER (31) 'q'            remote wake (vector 0xF1) observed;
//                                                         wake_reenter_out++ then back to idle
//   (note: 'c' AP_STAGE_CR4_SYNCED (25) runs between 'k' and the gs: canary —
//    numbering is by increment, not time order)
//
//   'I'   AP_STAGE_BEFORE_HLT (17)        'Z'             about to cli;hlt; died before first hlt
//   'Z'   AP_STAGE_IDLE (18)              (parked)        AP parked in cli/hlt idle loop — success
//
// Preconditions carried across the Rust jump: CR4=PAE-only (no SSE), interrupts masked
// (no AP IDT — every env step must be fault-free or the stage trace names it). The
// stage word / ready_word / env_flags / percpu_record_ptr live in the low identity
// page; the kernel CR3 additionally maps kernel text, .bss (per-CPU records, AP
// GDT/TSS), and the LAPIC MMIO — everything the BSP itself uses under that root.

// Fine-grained AP stage-word codes (written to [handoff+48] by yarm_x86_64_ap_entry,
// except RUST_JUMP which the trampoline writes via the absolute low address).
// The BSP reads this on admit-poll timeout to name the last stage the AP reached.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_RUST_JUMP: u32 = 9;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_RUST_ENTERED: u32 = 10;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_HANDOFF_LOADED: u32 = 11;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_HANDOFF_VALIDATED: u32 = 12;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_BEFORE_WRMSR: u32 = 13;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_AFTER_WRMSR: u32 = 14;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_AFTER_RDMSR: u32 = 15;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_GS_VERIFIED: u32 = 16;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_BEFORE_HLT: u32 = 17;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IDLE: u32 = 18;
// Stage 183 inc.3 env-step stages (executed between GS_VERIFIED and BEFORE_HLT).
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_KCR3_BEGIN: u32 = 19;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_KCR3_LIVE: u32 = 20;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_GDT_LOADED: u32 = 21;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_TSS_LOADED: u32 = 22;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_LAPIC_CHECKED: u32 = 23;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IDLE_CTX_PUBLISHED: u32 = 24;
// Stage 183 inc.4 (interrupt-safe idle) stages.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_CR4_SYNCED: u32 = 25;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IDT_LOADED: u32 = 26;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IRQ_SMOKE_WAIT: u32 = 27;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IRQ_SMOKE_DONE: u32 = 28;
// Stage 183 inc.4 fix: LAPIC software-enable (SVR/TPR/ESR) before the smoke window.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_LAPIC_ENABLED: u32 = 29;
// Stage 183.5: managed, scheduler-owned interruptible idle loop states.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_SCHED_IDLE: u32 = 30;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_SCHED_WAKE_REENTER: u32 = 31;
// Stage 183.5 fix (no_resume_after_handler): fine-grained smoke handler/resume
// sub-stages. 32-34 are written by the 0xF0 handler stub via gs:[112]
// (PerCpuRecord.irq_stage); 35/36 by the AP main flow into the stage word.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IRQ_HANDLER_ENTER: u32 = 32;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IRQ_HANDLER_EOI: u32 = 33;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IRQ_HANDLER_IRET: u32 = 34;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IRQ_RESUMED: u32 = 35;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_IRQ_ACK_WRITTEN: u32 = 36;
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_HANDOFF_NULL: u32 = 253; // percpu_record_ptr == 0 (BSP bug)
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) const AP_STAGE_GS_MISMATCH: u32 = 254; // rdmsr readback != value written

/// Name a fine-grained AP stage-word code for the BSP admit-poll timeout log.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_stage_name(raw: u32) -> &'static str {
    match raw {
        0 => "none",
        AP_STAGE_RUST_ONLINE => "rust_online",
        AP_STAGE_IDLE_ADMIT_OK => "idle_admit_ok",
        AP_STAGE_RUST_JUMP => "rust_jump",
        AP_STAGE_RUST_ENTERED => "rust_entered",
        AP_STAGE_HANDOFF_LOADED => "handoff_loaded",
        AP_STAGE_HANDOFF_VALIDATED => "handoff_validated",
        AP_STAGE_BEFORE_WRMSR => "before_wrmsr",
        AP_STAGE_AFTER_WRMSR => "after_wrmsr",
        AP_STAGE_AFTER_RDMSR => "after_rdmsr",
        AP_STAGE_GS_VERIFIED => "gs_verified",
        AP_STAGE_BEFORE_HLT => "before_hlt",
        AP_STAGE_IDLE => "idle",
        AP_STAGE_KCR3_BEGIN => "kcr3_begin",
        AP_STAGE_KCR3_LIVE => "kcr3_live",
        AP_STAGE_GDT_LOADED => "gdt_loaded",
        AP_STAGE_TSS_LOADED => "tss_loaded",
        AP_STAGE_LAPIC_CHECKED => "lapic_checked",
        AP_STAGE_IDLE_CTX_PUBLISHED => "idle_ctx_published",
        AP_STAGE_CR4_SYNCED => "cr4_synced",
        AP_STAGE_IDT_LOADED => "idt_loaded",
        AP_STAGE_IRQ_SMOKE_WAIT => "irq_smoke_wait",
        AP_STAGE_IRQ_SMOKE_DONE => "irq_smoke_done",
        AP_STAGE_LAPIC_ENABLED => "lapic_enabled",
        AP_STAGE_SCHED_IDLE => "sched_idle",
        AP_STAGE_SCHED_WAKE_REENTER => "sched_wake_reenter",
        AP_STAGE_IRQ_HANDLER_ENTER => "irq_handler_enter",
        AP_STAGE_IRQ_HANDLER_EOI => "irq_handler_eoi",
        AP_STAGE_IRQ_HANDLER_IRET => "irq_handler_iret",
        AP_STAGE_IRQ_RESUMED => "irq_resumed",
        AP_STAGE_IRQ_ACK_WRITTEN => "irq_ack_written",
        AP_STAGE_HANDOFF_NULL => "handoff_null",
        AP_STAGE_GS_MISMATCH => "gs_mismatch",
        _ => "unknown",
    }
}

/// Stage 183 inc.2 `@ → H` root-cause fix: this entry is now a **naked function**.
///
/// The previous non-naked form read `rdi` inside a regular `asm!` block WITHOUT an
/// `in("rdi") handoff_ptr` operand. Per the Rust reference inline-assembly rule
/// `asm.rules.reg-not-input` — "Any registers not specified as inputs will contain
/// an undefined value on entry to the assembly code" — `rdi` was formally undefined
/// at the asm boundary, so the first instruction after the `@` breadcrumb
/// (`mov dword ptr [rdi + 48], 10`) stored through an undefined pointer. On an AP
/// with no IDT that faults → triple fault → the observed silent death after `@`
/// (`let _ = handoff_ptr;` binds nothing at the register level).
///
/// Per the naked-functions reference, a `#[unsafe(naked)]` function has NO compiler
/// prologue/epilogue and "may assume that the call stack and register state are
/// valid on entry as per the signature and calling convention" — so `rdi ==
/// handoff_ptr` is guaranteed at the FIRST instruction, and the trampoline's
/// `mov rdi, rbx; add rdi, AP_OFF_HANDOFF; jmp rax` IS the whole ABI contract
/// (the same trampoline→naked-entry transfer Redox uses for `kstart_ap`). This
/// also removes any compiler-generated prologue that could touch the stack/SSE.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub(super) extern "C" fn yarm_x86_64_ap_entry(handoff_ptr: *const ApHandoff) -> ! {
    // ApHandoff offsets (rdi = handoff_ptr, guaranteed by the naked calling-convention
    // contract): ready_word=32, percpu_record_ptr=40, ap_stage=48. Every risky action is
    // preceded by a paired serial breadcrumb byte AND a stage-word write to [rdi+48], so
    // a BSP timeout can name the last stage. `dx` is reloaded to 0x3F8 before each `out`
    // because rdmsr/wrmsr clobber rdx. See the "AP breadcrumb ⇄ stage trace map" table
    // above for the byte↔stage↔block mapping and "failure after this byte" semantics.
    // `AP_IDLE_ADMIT_PROOF` (compile-time, non-knob) gates the BSP-side admit poll; this
    // naked body IS the admit path (GS wrmsr/rdmsr verify → interrupt-masked idle).
    core::arch::naked_asm!(
        "cli",
        // '@' / stage 10: the Rust-entered breadcrumb — higher-half Rust text ran.
        "mov dx, 0x3F8",
        "mov al, 0x40", // '@'
        "out dx, al",
        "mov dword ptr [rdi + 48], 10",
        // 'H' / stage 11: load handoff pointer (percpu_record_ptr → GS base).
        "mov rsi, [rdi + 40]",
        "mov al, 0x48", // 'H'
        "out dx, al",
        "mov dword ptr [rdi + 48], 11",
        // Validate the handoff pointer: null ⇒ BSP bug, skip wrmsr, idle at 253.
        "test rsi, rsi",
        "jz 6f",
        // 'V' / stage 12: handoff pointer validated (non-null).
        "mov al, 0x56", // 'V'
        "out dx, al",
        "mov dword ptr [rdi + 48], 12",
        // 'W' / stage 13: about to wrmsr IA32_GS_BASE.
        "mov al, 0x57", // 'W'
        "out dx, al",
        "mov dword ptr [rdi + 48], 13",
        // wrmsr IA32_GS_BASE(0xC000_0101) = rsi (edx:eax = hi:lo).
        "mov ecx, 0xC0000101",
        "mov rax, rsi",
        "mov rdx, rsi",
        "shr rdx, 32",
        "wrmsr",
        // 'w' / stage 14: wrmsr returned.
        "mov dx, 0x3F8",
        "mov al, 0x77", // 'w'
        "out dx, al",
        "mov dword ptr [rdi + 48], 14",
        // rdmsr IA32_GS_BASE readback → reconstruct into rax.
        "mov ecx, 0xC0000101",
        "rdmsr",
        "shl rdx, 32",
        "or rax, rdx",
        // 'r' / stage 15: rdmsr returned (rax = readback).
        "mov dx, 0x3F8",
        "mov r8, rax",  // preserve readback across the breadcrumb
        "mov al, 0x72", // 'r'
        "out dx, al",
        "mov dword ptr [rdi + 48], 15",
        // Compare readback (r8) with the value written (rsi).
        "cmp r8, rsi",
        "jne 8f",
        // 'O' / stage 16: GS verified. Env steps follow; ready_word=3 is published
        // only after them (label 60), so a mid-env fault shows up as an admit
        // timeout whose last_stage names the env step that died.
        "mov al, 0x4F", // 'O'
        "out dx, al",
        "mov dword ptr [rdi + 48], 16",
        // ---- Stage 183 inc.3 env steps (results OR'd into env_flags [rdi+88]) ----
        // 'K' / stage 19: controlled reload of the FULL kernel CR3.
        "mov al, 0x4B", // 'K'
        "out dx, al",
        "mov dword ptr [rdi + 48], 19",
        "mov rax, [rdi + 56]",
        "test rax, rax",
        "jz 60f", // kernel_cr3 not provided → skip env; BSP grades the gap honestly
        "mov cr3, rax",
        // 'k' / stage 20: kernel CR3 live — this very instruction fetch plus the low
        // stores below prove text and low identity are mapped under the new root.
        "mov al, 0x6B", // 'k'
        "out dx, al",
        "mov dword ptr [rdi + 48], 20",
        "or dword ptr [rdi + 88], 1", // env: KERNEL_CR3_RELOADED
        // 'c' / stage 25 (inc.4): sync CR4 with the BSP's value (PGE/OSFXSR/… — the
        // AP's control state converges on the BSP's; prerequisite for any future
        // compiled-Rust execution on the AP). 0 ⇒ BSP said skip.
        "mov al, 0x63", // 'c'
        "out dx, al",
        "mov dword ptr [rdi + 48], 25",
        "mov rax, [rdi + 112]",
        "test rax, rax",
        "jz 63f",
        "mov cr4, rax",
        "or dword ptr [rdi + 88], 32", // env: CR4_SYNCED
        "63:",
        // gs: canary store — proves higher-half .bss is writable on the AP under the
        // kernel CR3 AND that GS-relative addressing works (gs:[48] =
        // PerCpuRecord.env_canary; value = percpu::AP_ENV_CANARY).
        "mov dword ptr gs:[48], 0x0183C0DE",
        // 'D' / stage 21: per-AP GDT — lgdt from the handoff GDTR image, reload
        // SS/DS/ES with the kernel data selector, then far-return to the kernel code
        // selector. This converges the AP onto the BSP BOOT_GDT selector layout
        // (0x08 kernel code / 0x10 kernel data / 0x28 TSS).
        "mov al, 0x44", // 'D'
        "out dx, al",
        "mov dword ptr [rdi + 48], 21",
        "lgdt [rdi + 64]",
        "mov ax, 0x10",
        "mov ss, ax",
        "mov ds, ax",
        "mov es, ax",
        "lea rax, [rip + 61f]",
        "push 0x08",
        "push rax",
        "retfq",
        "61:",
        "or dword ptr [rdi + 88], 2", // env: GDT_LOADED
        // 'T' / stage 22: load the task register with the per-AP TSS selector. ltr
        // writes the BUSY bit into this AP's GDT in .bss — the BSP reads it back.
        "mov al, 0x54", // 'T'
        "out dx, al",
        "mov dword ptr [rdi + 48], 22",
        "mov ax, 0x28",
        "ltr ax",
        "or dword ptr [rdi + 88], 4", // env: TSS_LOADED
        // 'l' / stage 23: LAPIC access proof — read THIS AP's LAPIC ID register (VA
        // from the handoff; 0 = BSP found it unmapped, skip) and publish the id.
        "mov al, 0x6C", // 'l'
        "out dx, al",
        "mov dword ptr [rdi + 48], 23",
        "mov rax, [rdi + 80]",
        "test rax, rax",
        "jz 62f",
        "mov eax, [rax]",
        "shr eax, 24",
        "mov [rdi + 92], eax",        // lapic_id_out
        "or dword ptr [rdi + 88], 8", // env: LAPIC_READ
        "62:",
        // 'n' / stage 29 (inc.4 fix): software-ENABLE this AP's local APIC for
        // interrupt delivery. After INIT the LAPIC resets to SVR=0xFF with bit 8
        // (APIC software enable) CLEAR — a software-disabled LAPIC accepts only
        // INIT/SIPI/NMI/SMI and silently DROPS fixed IPIs, which is exactly why the
        // 183.4 smoke IPI (vector 0xF0) never reached the handler on the host.
        // Write SVR = 0x1FF (enable | spurious vector 0xFF — parked by the catch-all
        // stub if it ever fires), TPR = 0 (accept all priority classes incl. 0xF0),
        // write-clear ESR, and publish all three readbacks for BSP grading.
        "mov al, 0x6E", // 'n'
        "out dx, al",
        "mov dword ptr [rdi + 48], 29",
        "mov rax, [rdi + 80]",
        "test rax, rax",
        "jz 64f",        // LAPIC VA not provided → flag stays clear; BSP grades BAD
        "sub rax, 0x20", // LAPIC MMIO base (id reg VA - 0x20)
        "mov dword ptr [rax + 0xF0], 0x1FF", // SVR: software enable | spurious 0xFF
        "mov dword ptr [rax + 0x80], 0", // TPR: accept all vectors
        "mov dword ptr [rax + 0x280], 0", // ESR: write to latch/clear
        "mov r8d, [rax + 0xF0]",
        "mov [rdi + 120], r8d", // svr_out
        "mov r8d, [rax + 0x80]",
        "mov [rdi + 124], r8d", // tpr_out
        "mov r8d, [rax + 0x280]",
        "mov [rdi + 128], r8d",         // esr_out
        "or dword ptr [rdi + 88], 128", // env: LAPIC_SW_ENABLED
        "64:",
        // 'i' / stage 26 (inc.4): load the AP-safe IDT (catch-all park stubs +
        // the smoke-vector handler; see descriptor_tables.rs). From here an
        // unexpected interrupt/exception parks deterministically instead of
        // triple-faulting. Interrupts remain MASKED until the smoke window.
        "mov al, 0x69", // 'i'
        "out dx, al",
        "mov dword ptr [rdi + 48], 26",
        "lidt [rdi + 96]",
        "or dword ptr [rdi + 88], 64", // env: IDT_LOADED
        // 'y' / stage 24: publish the live idle context — store the current rsp via
        // gs: (gs:[56] = PerCpuRecord.saved_rsp) for BSP validation against the
        // recorded idle metadata.
        "mov al, 0x79", // 'y'
        "out dx, al",
        "mov dword ptr [rdi + 48], 24",
        "mov qword ptr gs:[56], rsp",
        "or dword ptr [rdi + 88], 16", // env: IDLE_CTX_PUBLISHED
        // 'u' / stage 27 (inc.4): interrupt-smoke wait window. Publish ready_word=3,
        // then wait INTERRUPTIBLY for the BSP's one controlled smoke IPI. The
        // `sti; hlt` pair is the race-free idiom: sti's interrupt shadow defers
        // delivery until hlt has begun, so an IPI sent any time after ready_word=3
        // either wakes hlt or was already handled (gs:[96] != 0) — no lost wake.
        "mov al, 0x75", // 'u'
        "out dx, al",
        "mov dword ptr [rdi + 32], 3",
        "73:",
        "mov dword ptr [rdi + 48], 27", // (re)enter irq_smoke_wait
        "sti",
        "hlt",
        "cli",
        // 'IRQ_RESUMED' (35): hlt returned (iretq resumed the window), IF masked.
        "mov dword ptr [rdi + 48], 35",
        "cmp dword ptr gs:[96], 0", // irq_hit_count (smoke handler ran?)
        "je 73b",                   // spurious wake without the handler: re-wait
        // Handler confirmed: write the PERSISTENT ACK (gs:[116] = irq_ack). The
        // BSP polls THIS — never a transient stage — so its serial-log latency
        // can no longer lose the race against the AP's fast stage transitions
        // (the 183.5 host failure: RECV printed for ~ms while the AP passed the
        // transient stage 28 and parked at 30, so a 28|17|18 poll always missed).
        "mov al, 0x61", // 'a'
        "out dx, al",
        "mov dword ptr gs:[116], 1",    // irq_ack = 1 (persistent)
        "mov dword ptr [rdi + 48], 36", // IRQ_ACK_WRITTEN
        "mov al, 0x64",                 // 'd'
        "out dx, al",
        // 'v' / stage 28 (inc.4): smoke handled (handler counted + EOI'd + iretq'd
        // back into the window). Interrupts are masked again; fall through to the
        // MANAGED scheduler-owned idle loop (183.5) — NOT a bare cli/hlt park.
        "mov al, 0x76", // 'v'
        "out dx, al",
        "mov dword ptr [rdi + 48], 28",
        // 'q' / stage 30 (183.5): scheduler-owned idle. The BSP installs
        // current=idle(tid 0) for this CPU and brings it scheduler-online; this
        // loop IS that idle task's body: interruptible (sti;hlt — race-free
        // pending-IPI delivery), wake-capable (vector 0xF1 handler increments
        // gs:[108]), and it RETURNS TO IDLE after every observed wake, publishing
        // the re-entry count into [rdi+132] for the BSP's lost/dup-wake grading.
        "mov al, 0x71", // 'q'
        "out dx, al",
        // Publish sched-idle state BOTH into the low handoff stage word (boot-CR3
        // trace) AND the per-CPU record via gs: (kernel .bss — the ONLY thing the
        // post-boot admission polls: the low identity VAs are unmapped on the task
        // address space the admission runs on; polling them page-faulted, #PF
        // CR2=0x7170).
        "mov dword ptr [rdi + 48], 30",
        "mov dword ptr gs:[120], 30", // sched_stage mirror
        "xor r9d, r9d",               // last observed remote_wake_count
        "75:",
        "sti",
        "hlt",
        "cli",
        "mov r8d, dword ptr gs:[108]", // remote_wake_count (wake stub increments)
        "cmp r8d, r9d",
        "je 75b", // hlt woke without a new remote wake -> re-idle
        "mov r9d, r8d",
        // 'z' / stage 31 (183.5): remote wake observed — publish the re-entry
        // evidence (low trace + gs mirrors), then return to the scheduler idle
        // state.
        "mov al, 0x7A", // 'z'
        "out dx, al",
        "mov dword ptr [rdi + 48], 31",
        "mov dword ptr gs:[120], 31",   // sched_stage mirror
        "add dword ptr [rdi + 132], 1", // wake_reenter_out++
        "add dword ptr gs:[124], 1",    // wake_reenter mirror++
        "mov dword ptr [rdi + 48], 30", // back to sched_idle
        "mov dword ptr gs:[120], 30",   // sched_stage mirror
        "jmp 75b",
        "60:",
        // Env skipped (kernel_cr3 == 0 ⇒ no GDT/IDT prep): publish ready_word=3 and
        // idle with interrupts MASKED — no sti without a loaded AP IDT, ever.
        "mov dword ptr [rdi + 32], 3",
        "jmp 5f",
        "8:",
        // 'B' / stage 254: GS readback mismatch. Publish ready_word=254; still idle.
        "mov al, 0x42", // 'B'
        "out dx, al",
        "mov dword ptr [rdi + 48], 254",
        "mov dword ptr [rdi + 32], 254",
        "jmp 5f",
        "6:",
        // '!' / stage 253: handoff pointer was null (BSP bug). ready_word=254; idle.
        "mov al, 0x21", // '!'
        "out dx, al",
        "mov dword ptr [rdi + 48], 253",
        "mov dword ptr [rdi + 32], 254",
        "5:",
        // 'I' / stage 17: about to enter the cli/hlt idle loop.
        "mov al, 0x49", // 'I'
        "out dx, al",
        "mov dword ptr [rdi + 48], 17",
        // Idle loop: interrupts masked (no AP IDT/LAPIC yet).
        "cli",
        // 'Z' / stage 18: parked in idle.
        "mov al, 0x5A", // 'Z'
        "out dx, al",
        "mov dword ptr [rdi + 48], 18",
        "7:",
        "hlt",
        "jmp 7b",
        // naked_asm! takes no operands/options: the body IS the function, it
        // diverges (hlt loop), and register use is governed by the naked
        // calling-convention contract (rdi = handoff_ptr at entry).
    )
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
    patched.ap_stage = 0;
    // Stage 183 inc.3: AP-written env results start cleared; lapic_id_out carries a
    // sentinel so a skipped/failed MMIO read can never alias a real LAPIC id.
    patched.env_flags = 0;
    patched.lapic_id_out = 0xFFFF_FFFF;
    // Stage 183 inc.4 fix: LAPIC readiness readbacks start cleared.
    patched.svr_out = 0;
    patched.tpr_out = 0;
    patched.esr_out = 0;
    patched.wake_reenter_out = 0;

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

/// Stage 183 inc.2: low identity-mapped VA of the AP fine-grained stage word (offset 48).
/// Read by the BSP admit-poll so a timeout can name the last stage the AP reached without
/// any AP-side higher-half access.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_stage_word_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_STAGE_WORD_OFFSET) as *const u32
}

/// Stage 183 inc.3: low identity-mapped VA of the AP env-step result bitmask (offset 88).
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_env_flags_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_ENV_FLAGS_OFFSET) as *const u32
}

/// Stage 183 inc.3: low identity-mapped VA of the AP LAPIC-id readback (offset 92).
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_lapic_id_out_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_LAPIC_ID_OUT_OFFSET) as *const u32
}

/// Stage 183 inc.4 fix: low identity-mapped VAs of the AP LAPIC readiness readbacks.
#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_svr_out_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_SVR_OUT_OFFSET) as *const u32
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_tpr_out_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_TPR_OUT_OFFSET) as *const u32
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_esr_out_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_ESR_OUT_OFFSET) as *const u32
}

/// Stage 183.5: low identity-mapped VA of the AP sched-idle wake re-entry count.
/// Boot-CR3-only diagnostic alias; the post-boot admission polls the per-CPU
/// record MIRROR instead (the low identity VAs are unmapped on task address
/// spaces — the 183.5 #PF CR2=0x7170 root cause).
#[cfg(all(not(test), not(feature = "hosted-dev")))]
#[allow(dead_code)]
pub(super) fn ap_wake_reenter_low_virt(handoff_off: usize) -> *const u32 {
    (AP_TRAMPOLINE_PHYS + handoff_off + AP_HANDOFF_WAKE_REENTER_OFFSET) as *const u32
}

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_ready_word_directmap_virt(handoff_off: usize) -> *const u32 {
    let trampoline_virt = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        + AP_TRAMPOLINE_PHYS as u64) as usize;

    (trampoline_virt + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as *const u32
}
