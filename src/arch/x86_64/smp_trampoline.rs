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
//   0  magic:             u32
//   4  cpu_id:            u32
//   8  stack_top:         u64
//   16 kernel_state_ptr:  u64   // production: CR3/PML4 physical address
//   24 ready_flag_ptr:    u64   // diagnostic/layout only for now
//   32 ready_word:        u32   // AP assembly writes 1/2/3/254 here directly
//   36 reserved:          u32
//   40 percpu_record_ptr: u64   // Stage 183 inc.2: AP GS base (low identity handoff)
//   48 ap_stage:          u32   // Stage 183 inc.2: fine-grained AP stage trace word
//   52 <pad>:             u32   // struct is u64-aligned → size_of == 56
pub(super) const AP_HANDOFF_READY_WORD_OFFSET: usize = 32;

// Stage 183 inc.2: the AP writes an incrementing stage code here (offset 48) before
// every risky action, so a BSP admit-poll timeout can report the LAST stage the AP
// reached (`last_stage`) instead of only "it didn't finish". Distinct from ready_word
// (offset 32), which stays the coarse online(2)/admit(3)/gs_bad(254) signal.
pub(super) const AP_HANDOFF_STAGE_WORD_OFFSET: usize = 48;

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
    // BSP admit-poll can name the last stage the AP reached on timeout. u32 field; the
    // struct is u64-aligned so `size_of::<ApHandoff>() == 56`.
    pub(super) ap_stage: u32,
}

// Stage 183 inc.2 layout guard: the trampoline / AP-entry asm hardcodes these field
// offsets ([rdi+32], [rdi+40], [rdi+48]) and the `.zero 56` handoff reservation. Lock
// the Rust struct to them at compile time so a field reorder can never silently make the
// asm read/write the wrong slot.
const _: () = {
    assert!(core::mem::size_of::<ApHandoff>() == 56);
    assert!(core::mem::offset_of!(ApHandoff, ready_word) == AP_HANDOFF_READY_WORD_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, ready_word) == 32);
    assert!(core::mem::offset_of!(ApHandoff, percpu_record_ptr) == 40);
    assert!(core::mem::offset_of!(ApHandoff, ap_stage) == AP_HANDOFF_STAGE_WORD_OFFSET);
    assert!(core::mem::offset_of!(ApHandoff, ap_stage) == 48);
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
    // Reserve the FULL ApHandoff (size_of::<ApHandoff>() == 56): magic(0) cpu_id(4)
    // stack_top(8) kernel_state_ptr(16) ready_flag_ptr(24) ready_word(32) reserved(36)
    // percpu_record_ptr(40) ap_stage(48) pad(52). Under-reserving would place the AP
    // stage word past `yarm_ap_trampoline_end`, shrinking the copied trampoline `len`.
    .zero 56

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
//   'O'   AP_STAGE_GS_VERIFIED (16)       'I'             GS readback == written; entering idle
//   'B'   AP_STAGE_GS_MISMATCH (254)      'I'             GS readback != written (still idles safely)
//   '!'   AP_STAGE_HANDOFF_NULL (253)     'I'             percpu_record_ptr was 0 (BSP handoff bug)
//   'I'   AP_STAGE_BEFORE_HLT (17)        'Z'             about to cli;hlt; died before first hlt
//   'Z'   AP_STAGE_IDLE (18)              (parked)        AP parked in cli/hlt idle loop — success
//
// Preconditions carried across the Rust jump: CR4=PAE-only (no SSE), bootstrap PML4
// maps text + low identity ONLY (no .bss/.data/MMIO), interrupts masked (no AP IDT).
// The stage word / ready_word / percpu_record_ptr all live in the low identity page.

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
        // 'O' / stage 16: GS verified. Publish ready_word=3 (admit ok).
        "mov al, 0x4F", // 'O'
        "out dx, al",
        "mov dword ptr [rdi + 48], 16",
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

#[cfg(all(not(test), not(feature = "hosted-dev")))]
pub(super) fn ap_ready_word_directmap_virt(handoff_off: usize) -> *const u32 {
    let trampoline_virt = (crate::arch::platform_layout::KERNEL_BOOTSTRAP_VIRT_BASE
        + AP_TRAMPOLINE_PHYS as u64) as usize;

    (trampoline_virt + handoff_off + AP_HANDOFF_READY_WORD_OFFSET) as *const u32
}
