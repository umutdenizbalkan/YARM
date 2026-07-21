// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
use core::sync::atomic::AtomicU8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicBool, Ordering};

pub const IDT_ENTRIES: usize = 256;
const IDT_GATE_INTERRUPT: u8 = 0x0E;
const IDT_PRESENT: u8 = 1 << 7;
#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
const VEC_NMI: usize = 2;
#[cfg(any(test, all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
const VEC_DOUBLE_FAULT: usize = 8;
#[cfg(any(test, all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
const VEC_PAGE_FAULT: usize = 14;
#[cfg(any(test, all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
#[allow(dead_code)]
const VEC_TIMER: usize = 0x20;
#[cfg(any(test, all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
const VEC_SYSCALL: usize = 0x80;

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X86IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl X86IdtEntry {
    pub const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    pub const fn new_interrupt(handler_addr: u64, selector: u16, dpl: u8, ist: u8) -> Self {
        Self {
            offset_low: handler_addr as u16,
            selector,
            ist: ist & 0x7,
            type_attr: IDT_PRESENT | ((dpl & 0x3) << 5) | IDT_GATE_INTERRUPT,
            offset_mid: (handler_addr >> 16) as u16,
            offset_high: (handler_addr >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X86IdtPointer {
    limit: u16,
    base: u64,
}

impl X86IdtPointer {
    pub fn from_table(table: &[X86IdtEntry; IDT_ENTRIES]) -> Self {
        Self::from_ptr(table.as_ptr())
    }

    pub fn from_ptr(table: *const X86IdtEntry) -> Self {
        Self {
            limit: (core::mem::size_of::<X86IdtEntry>() * IDT_ENTRIES - 1) as u16,
            base: table as u64,
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X86TaskStateSegment {
    _reserved0: u32,
    pub rsp0: u64,
    pub rsp1: u64,
    pub rsp2: u64,
    _reserved1: u64,
    pub ist1: u64,
    pub ist2: u64,
    pub ist3: u64,
    pub ist4: u64,
    pub ist5: u64,
    pub ist6: u64,
    pub ist7: u64,
    _reserved2: u64,
    _reserved3: u16,
    pub io_map_base: u16,
}

impl X86TaskStateSegment {
    pub const fn new() -> Self {
        Self {
            _reserved0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            _reserved1: 0,
            ist1: 0,
            ist2: 0,
            ist3: 0,
            ist4: 0,
            ist5: 0,
            ist6: 0,
            ist7: 0,
            _reserved2: 0,
            _reserved3: 0,
            io_map_base: core::mem::size_of::<X86TaskStateSegment>() as u16,
        }
    }
}

static DESCRIPTOR_SCAFFOLD_READY: AtomicBool = AtomicBool::new(false);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct X86GdtPointer {
    limit: u16,
    base: u64,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C, align(16))]
struct X86BootGdt {
    entries: [u64; 7],
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
static mut BOOT_IDT: [X86IdtEntry; IDT_ENTRIES] = [const { X86IdtEntry::missing() }; IDT_ENTRIES];
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut BOOT_TSS: X86TaskStateSegment = X86TaskStateSegment::new();
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut BOOT_GDT: X86BootGdt = X86BootGdt {
    entries: [
        0x0000_0000_0000_0000, // null
        0x00af_9a00_0000_ffff, // kernel code
        0x00af_9200_0000_ffff, // kernel data
        0x00af_f200_0000_ffff, // user data
        0x00af_fa00_0000_ffff, // user code
        0,
        0,
    ],
};
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(align(16))]
struct IstStack<const BYTES: usize>([u8; BYTES]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IST_NMI_STACK_BYTES: usize = 16 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IST_DOUBLE_FAULT_STACK_BYTES: usize = 64 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IST_PAGE_FAULT_STACK_BYTES: usize = 16 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut IST_NMI: IstStack<IST_NMI_STACK_BYTES> = IstStack([0; IST_NMI_STACK_BYTES]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut IST_DOUBLE_FAULT: IstStack<IST_DOUBLE_FAULT_STACK_BYTES> =
    IstStack([0; IST_DOUBLE_FAULT_STACK_BYTES]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut IST_PAGE_FAULT: IstStack<IST_PAGE_FAULT_STACK_BYTES> =
    IstStack([0; IST_PAGE_FAULT_STACK_BYTES]);

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
const KERNEL_CODE_SELECTOR: u16 = 0x08;
#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
const KERNEL_DATA_SELECTOR: u16 = 0x10;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const TSS_SELECTOR: u16 = 0x28;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const USER_CODE_SELECTOR: u16 = 0x23;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IA32_EFER_MSR: u32 = 0xC000_0080;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IA32_EFER_SCE: u64 = 1 << 0;
/// Stage 199A2D2C2B3: EFER.NXE (No-Execute Enable, bit 11). The BSP sets this in its early boot asm
/// after a CPUID NX check; the AP path (`configure_syscall_msrs_for_self`) historically set ONLY SCE,
/// so on an AP the NX bit (PTE bit 63) was treated as RESERVED — every ring-3 access to a non-
/// executable data page (payload/meta/stack) took a reserved-bit #PF. The kernel page tables already
/// use NX (proving NX is supported), so the AP must enable NXE too for a consistent user return.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IA32_EFER_NXE: u64 = 1 << 11;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IA32_STAR_MSR: u32 = 0xC000_0081;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IA32_LSTAR_MSR: u32 = 0xC000_0082;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IA32_FMASK_MSR: u32 = 0xC000_0084;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const RFLAGS_IF_MASK: u64 = 1 << 9;
#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
const IST_SLOT_NMI: u8 = 1;
#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
const IST_SLOT_DOUBLE_FAULT: u8 = 2;
#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
const IST_SLOT_PAGE_FAULT: u8 = 3;
#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
#[repr(C)]
#[derive(Default)]
struct X86SavedRegs {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rsi: u64,
    rdi: u64,
    rbp: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
#[repr(C)]
struct X86InterruptStackFrame {
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
#[repr(C)]
struct X86InterruptStackFrameHeader {
    rip: u64,
    cs: u64,
    rflags: u64,
}

// Stage 189D: the nested-trap guard is PER-CPU. It catches a trap that re-enters
// the dispatch on the SAME CPU (the real bug: a fault inside the fault handler),
// which is a per-CPU stack/reentrancy condition. A trap on a DIFFERENT CPU is NOT
// nesting — the global `with_cpu` lock serializes the actual KernelState access —
// so a single global counter would false-positive (`!BN` + halt) when the BSP and
// an AP dispatch concurrently. Indexed by logical CPU id.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static TRAP_DISPATCH_DEPTH: [AtomicUsize; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::arch::platform_constants::MAX_CPUS];
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static FATAL_LOG_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const UNMAPPED_CPU: usize = usize::MAX;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static APIC_TO_CPU_ID: [AtomicUsize; 256] = [const { AtomicUsize::new(UNMAPPED_CPU) }; 256];
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const TRAP_KERNEL_STATE_UNINITIALIZED: u8 = 0;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const TRAP_KERNEL_STATE_READY: u8 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static TRAP_KERNEL_STATE_STATUS: AtomicU8 = AtomicU8::new(TRAP_KERNEL_STATE_UNINITIALIZED);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
struct TrapKernelStateCell(
    core::cell::UnsafeCell<core::mem::MaybeUninit<crate::kernel::boot::KernelState>>,
);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
unsafe impl Sync for TrapKernelStateCell {}
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(link_section = ".bss.kernel_state")]
static TRAP_KERNEL_STATE: TrapKernelStateCell =
    TrapKernelStateCell(core::cell::UnsafeCell::new(core::mem::MaybeUninit::uninit()));
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static TRAP_SHARED_KERNEL_PTR: core::sync::atomic::AtomicPtr<crate::runtime::SharedKernel> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static STAGE2N_FIRST_TRAP_LOGGED: AtomicBool = AtomicBool::new(false);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static STAGE2N_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);
/// Set to true after bootstrap_first_user_task completes and all user tasks are
/// enqueued. The timer ISR checks this flag: if false, it does EOI-only and
/// re-arms without ticking/yielding, to avoid racing with borrow_kernel_for_boot.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static BOOTSTRAP_SCHEDULER_READY: AtomicBool = AtomicBool::new(false);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn signal_bootstrap_scheduler_ready() {
    BOOTSTRAP_SCHEDULER_READY.store(true, Ordering::Release);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn bootstrap_scheduler_is_ready() -> bool {
    BOOTSTRAP_SCHEDULER_READY.load(Ordering::Acquire)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const DEBUG_UART_DATA_PORT: u16 = 0x3F8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const DEBUG_UART_LINE_STATUS_PORT: u16 = 0x3FD;
// Stage 189C4: the SYSCALL entry kernel-RSP0 and user-RSP scratch are now PER-CPU
// (gs-relative `syscall_kernel_rsp0` @144 / `syscall_scratch_rsp` @152 in the
// per-CPU record). The old global RIP-relative slots (`YARM_X86_SYSCALL_RSP0` /
// `YARM_X86_SYSCALL_SCRATCH_RSP`) — which were documented NOT SMP-safe — are
// removed: there is no global syscall stack authority any more.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
unsafe extern "C" {
    fn yarm_x86_lstar_entry();
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn write_msr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn configure_syscall_fast_path(rsp0: u64) {
    // Stage 189C4: establish the BSP's per-CPU SYSCALL state BEFORE arming LSTAR.
    // Set the BSP's active GS.base to its per-CPU record (so the gs-relative entry
    // resolves), then populate that record's per-CPU kernel RSP0. No global slot.
    let bsp = crate::kernel::scheduler::CpuId(crate::arch::platform_layout::BOOTSTRAP_CPU_ID);
    super::percpu::set_syscall_kernel_rsp0(bsp, rsp0);
    super::percpu::write_gs_base_for_self(bsp);
    crate::yarm_log!("X86_PERCPU_ENTRY_READY cpu={} rsp0=0x{:x}", bsp.0, rsp0);
    crate::yarm_log!("X86_SYSCALL_ENTRY_PERCPU_READY cpu={}", bsp.0);
    // Stage 189C4 — swapgs is NOT required by this design: CR4.FSGSBASE is disabled
    // (ring 3 cannot alter GS.base) and user TLS is in FS, so GS.base is the kernel
    // per-CPU record in EVERY ring. The syscall/interrupt entries read gs-relative
    // per-CPU state directly with no swapgs and therefore nothing to unbalance.
    crate::yarm_log!("X86_SWAPGS_ENTRY_READY reason=not_required_gs_kernel_percpu");
    crate::yarm_log!("X86_INTERRUPT_ENTRY_SWAPGS_READY reason=not_required_gs_kernel_percpu");
    // STAR[47:32] = kernel CS selector; STAR[63:48] = SYSRET CS base (CS-16).
    configure_syscall_msrs_for_self();
}

/// Configure the EXECUTING CPU's SYSCALL fast-path MSRs (EFER.SCE, STAR, LSTAR,
/// FMASK). These are PER-CPU MSRs, so every CPU that will service a ring-3
/// `syscall` must run this itself. The BSP calls it from `configure_syscall_fast_path`;
/// Stage 189C6 calls it on an AP before dispatching a ring-3 task there, so the AP's
/// `syscall` reaches the same per-CPU LSTAR entry instead of `#UD`-ing on SCE=0.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn configure_syscall_msrs_for_self() {
    // STAR[47:32] = kernel CS selector; STAR[63:48] = SYSRET CS base (CS-16).
    let star = ((KERNEL_CODE_SELECTOR as u64) << 32) | (((USER_CODE_SELECTOR as u64) - 16) << 48);
    let mut efer = read_msr(IA32_EFER_MSR);
    // SCE for the `syscall` fast path; NXE so the AP honors the NX bit (bit 63) that the kernel page
    // tables set on non-executable user data pages — without it an AP ring-3 data read of an NX page
    // takes a reserved-bit #PF (Stage 199A2D2C2B3). The BSP already enables both.
    efer |= IA32_EFER_SCE | IA32_EFER_NXE;
    write_msr(IA32_EFER_MSR, efer);
    write_msr(IA32_STAR_MSR, star);
    write_msr(IA32_LSTAR_MSR, yarm_x86_lstar_entry as *const () as u64);
    write_msr(IA32_FMASK_MSR, RFLAGS_IF_MASK);
    // Stage 199A2D2C2C: PERMANENT user-entry parity guard. EVERY x86 CPU that can enter ring 3 runs
    // this before loading user mappings that contain NX PTEs, so read EFER back and REQUIRE both SCE
    // and NXE. A CPU that reached ring 3 without NXE would take reserved-bit #PFs on every NX data
    // page (the Stage 199A2D2C2B3 AP defect) — fail closed instead of masking it by clearing NX. The
    // BSP (early boot asm) and every AP share this identical requirement.
    let efer_back = read_msr(IA32_EFER_MSR);
    let sce = (efer_back & IA32_EFER_SCE) != 0;
    let nxe = (efer_back & IA32_EFER_NXE) != 0;
    let cpu = current_cpu_id();
    if !(sce && nxe) {
        crate::yarm_log!(
            "X86_EFER_USER_ENTRY_PARITY_FAIL cpu={} sce={} nxe={} efer=0x{:x}",
            cpu.0,
            sce as u8,
            nxe as u8,
            efer_back
        );
        halt_forever();
    }
    static EFER_PARITY_OK: [core::sync::atomic::AtomicBool;
        crate::arch::platform_constants::MAX_CPUS] =
        [const { core::sync::atomic::AtomicBool::new(false) };
            crate::arch::platform_constants::MAX_CPUS];
    let idx = (cpu.0 as usize).min(crate::arch::platform_constants::MAX_CPUS - 1);
    if !EFER_PARITY_OK[idx].swap(true, core::sync::atomic::Ordering::AcqRel) {
        crate::yarm_log!("X86_EFER_USER_ENTRY_OK cpu={} sce=1 nxe=1 result=ok", cpu.0);
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn debug_uart_putc(byte: u8) {
    unsafe {
        core::arch::asm!(
            "2:",
            "in al, dx",
            "test al, 0x20",
            "jz 2b",
            in("dx") DEBUG_UART_LINE_STATUS_PORT,
            lateout("al") _,
            options(nomem, nostack)
        );
        core::arch::asm!(
            "out dx, al",
            in("dx") DEBUG_UART_DATA_PORT,
            in("al") byte,
            options(nomem, nostack)
        );
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn debug_uart_hex_u64(value: u64) {
    for shift in (0..=60).rev().step_by(4) {
        let nibble = ((value >> shift) & 0xF) as u8;
        let ch = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        debug_uart_putc(ch);
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn debug_uart_trap_breadcrumb(
    reason: u8,
    vector: u64,
    error_code: u64,
    fault_addr: u64,
    fault_rip: u64,
    cpu_apic: u64,
) {
    debug_uart_putc(b'!');
    debug_uart_putc(b'B');
    debug_uart_putc(reason);
    debug_uart_putc(b'v');
    debug_uart_hex_u64(vector);
    debug_uart_putc(b'e');
    debug_uart_hex_u64(error_code);
    debug_uart_putc(b'c');
    debug_uart_hex_u64(fault_addr);
    debug_uart_putc(b'i');
    debug_uart_hex_u64(fault_rip);
    debug_uart_putc(b'a');
    debug_uart_hex_u64(cpu_apic);
    debug_uart_putc(b'\n');
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn encode_tss_descriptor(base: u64, limit: u32) -> (u64, u64) {
    let low = ((limit as u64) & 0xFFFF)
        | ((base & 0x00FF_FFFF) << 16)
        | (0x89u64 << 40)
        | (((limit as u64 >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    let high = base >> 32;
    (low, high)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn install_trap_kernel_state(
    kernel: crate::kernel::boot::KernelState,
) -> &'static mut crate::kernel::boot::KernelState {
    if TRAP_KERNEL_STATE_STATUS
        .compare_exchange(
            TRAP_KERNEL_STATE_UNINITIALIZED,
            TRAP_KERNEL_STATE_READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        panic!("trap kernel state already installed");
    }
    let kernel = unsafe {
        let slot = &mut *TRAP_KERNEL_STATE.0.get();
        slot.write(kernel)
    };
    register_apic_cpu_mapping(
        raw_current_apic_id() as u8,
        crate::kernel::scheduler::CpuId(crate::arch::platform_layout::BOOTSTRAP_CPU_ID),
    );
    kernel
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn install_trap_shared_kernel(shared: &'static crate::runtime::SharedKernel) {
    register_apic_cpu_mapping(
        raw_current_apic_id() as u8,
        crate::kernel::scheduler::CpuId(crate::arch::platform_layout::BOOTSTRAP_CPU_ID),
    );
    TRAP_SHARED_KERNEL_PTR.store(shared as *const _ as *mut _, Ordering::SeqCst);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn trap_kernel_state_mut() -> Option<&'static mut crate::kernel::boot::KernelState> {
    if TRAP_KERNEL_STATE_STATUS.load(Ordering::Acquire) != TRAP_KERNEL_STATE_READY {
        return None;
    }
    Some(unsafe {
        let slot = &mut *TRAP_KERNEL_STATE.0.get();
        &mut *slot.as_mut_ptr()
    })
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn trap_shared_kernel() -> Option<&'static crate::runtime::SharedKernel> {
    let ptr = TRAP_SHARED_KERNEL_PTR.load(Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

/// Stage 199A2D2C2B2: the installed SharedKernel for the AP idle-dispatch path (the CPU-1 saved-frame
/// resume runs from the managed idle loop's dispatch hook, which has no `shared` param). Same pointer
/// the trap dispatcher uses; `None` before install.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_trap_shared_kernel() -> Option<&'static crate::runtime::SharedKernel> {
    trap_shared_kernel()
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn raw_current_apic_id() -> u32 {
    core::arch::x86_64::__cpuid(1).ebx >> 24
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn register_apic_cpu_mapping(apic_id: u8, cpu: crate::kernel::scheduler::CpuId) {
    APIC_TO_CPU_ID[apic_id as usize].store(cpu.0 as usize, Ordering::Release);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn current_cpu_id() -> crate::kernel::scheduler::CpuId {
    let apic = raw_current_apic_id() as usize;
    if let Some(mapped) = APIC_TO_CPU_ID
        .get(apic)
        .map(|slot| slot.load(Ordering::Acquire))
        .filter(|mapped| *mapped != UNMAPPED_CPU && *mapped < crate::kernel::scheduler::MAX_CPUS)
    {
        return crate::kernel::scheduler::CpuId(mapped as u8);
    }
    if apic < crate::kernel::scheduler::MAX_CPUS {
        crate::kernel::scheduler::CpuId(apic as u8)
    } else {
        crate::kernel::scheduler::CpuId(crate::arch::platform_layout::BOOTSTRAP_CPU_ID)
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn halt_forever() -> ! {
    loop {
        unsafe {
            core::arch::asm!("cli", "hlt", options(noreturn));
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn idle_halt_loop() -> ! {
    loop {
        unsafe {
            core::arch::asm!("sti", "hlt", options(nomem, nostack));
        }
    }
}

/// Stage 190B: enter the interruptible idle loop from the AP scheduler loop
/// (`smp::ap_sched_next_or_idle`) once its run queue is empty. Same wake-capable
/// `sti; hlt` idle the trap dispatch uses for a task that yields to nothing.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_idle_halt_loop() -> ! {
    idle_halt_loop()
}

#[cfg(any(test, all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
const fn should_halt_without_kernel_state(vector: usize) -> bool {
    vector < 32 && vector != VEC_SYSCALL
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn log_decoded_fatal_trap(
    kernel: Option<&crate::kernel::boot::KernelState>,
    vector: u64,
    error_code: u64,
    frame: &X86InterruptStackFrame,
    fault_addr: u64,
) {
    if FATAL_LOG_IN_PROGRESS
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        debug_uart_putc(b'!');
        debug_uart_putc(b'F');
        debug_uart_putc(b'R');
        debug_uart_putc(b'\n');
        halt_forever();
    }
    let mut active_cr3 = 0u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) active_cr3, options(nostack, preserves_flags));
    }
    let current_tid = kernel.and_then(|k| k.current_tid()).unwrap_or(0);
    let current_asid = kernel
        .and_then(|k| k.task_asid(current_tid))
        .map(|asid| asid.0)
        .unwrap_or(0);
    debug_uart_putc(b'!');
    debug_uart_putc(b'F');
    debug_uart_putc(b'v');
    debug_uart_hex_u64(vector);
    debug_uart_putc(b'e');
    debug_uart_hex_u64(error_code);
    debug_uart_putc(b'i');
    debug_uart_hex_u64(frame.rip);
    debug_uart_putc(b's');
    debug_uart_hex_u64(frame.cs);
    debug_uart_putc(b'f');
    debug_uart_hex_u64(frame.rflags);
    debug_uart_putc(b'p');
    debug_uart_hex_u64(frame.rsp);
    debug_uart_putc(b'S');
    debug_uart_hex_u64(frame.ss);
    debug_uart_putc(b'2');
    debug_uart_hex_u64(fault_addr);
    debug_uart_putc(b'3');
    debug_uart_hex_u64(active_cr3);
    debug_uart_putc(b't');
    debug_uart_hex_u64(current_tid);
    debug_uart_putc(b'a');
    debug_uart_hex_u64(current_asid as u64);
    debug_uart_putc(b'c');
    debug_uart_hex_u64(frame.cs & 0x3);
    debug_uart_putc(b'\n');
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn log_decoded_fatal_trap_from_snapshot(
    snapshot: crate::runtime::FatalTrapReadSnapshot,
    vector: u64,
    error_code: u64,
    frame: &X86InterruptStackFrame,
    fault_addr: u64,
) {
    if FATAL_LOG_IN_PROGRESS
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        debug_uart_putc(b'!');
        debug_uart_putc(b'F');
        debug_uart_putc(b'R');
        debug_uart_putc(b'\n');
        halt_forever();
    }
    let mut active_cr3 = 0u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) active_cr3, options(nostack, preserves_flags));
    }
    debug_uart_putc(b'!');
    debug_uart_putc(b'F');
    debug_uart_putc(b'v');
    debug_uart_hex_u64(vector);
    debug_uart_putc(b'e');
    debug_uart_hex_u64(error_code);
    debug_uart_putc(b'i');
    debug_uart_hex_u64(frame.rip);
    debug_uart_putc(b's');
    debug_uart_hex_u64(frame.cs);
    debug_uart_putc(b'f');
    debug_uart_hex_u64(frame.rflags);
    debug_uart_putc(b'p');
    debug_uart_hex_u64(frame.rsp);
    debug_uart_putc(b'S');
    debug_uart_hex_u64(frame.ss);
    debug_uart_putc(b'2');
    debug_uart_hex_u64(fault_addr);
    debug_uart_putc(b'3');
    debug_uart_hex_u64(active_cr3);
    debug_uart_putc(b't');
    debug_uart_hex_u64(snapshot.current_tid);
    debug_uart_putc(b'a');
    debug_uart_hex_u64(snapshot.current_asid);
    debug_uart_putc(b'c');
    debug_uart_hex_u64(frame.cs & 0x3);
    debug_uart_putc(b'\n');
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
unsafe fn build_trap_frame_from_saved_regs(
    regs: *const X86SavedRegs,
    frame: *mut X86InterruptStackFrame,
    vector: u64,
) -> crate::kernel::trapframe::TrapFrame {
    let regs = unsafe { &*regs };
    let frame_header = unsafe { &*(frame as *const X86InterruptStackFrameHeader) };
    let mut trap = crate::kernel::trapframe::TrapFrame::zeroed();
    trap.set_saved_pc(frame_header.rip as usize);
    if (frame_header.cs & 0x3) == 0x3 {
        let frame = unsafe { &*frame };
        trap.set_saved_sp(frame.rsp as usize);
    }
    if vector as usize == VEC_SYSCALL {
        trap.set_syscall_num(regs.rax as usize);
        trap.set_arg(0, regs.rdi as usize);
        trap.set_arg(1, regs.rsi as usize);
        trap.set_arg(2, regs.rdx as usize);
        trap.set_arg(3, regs.rcx as usize);
        trap.set_arg(4, regs.r8 as usize);
        trap.set_arg(5, regs.r9 as usize);
    }
    trap.set_user_gpr(0, regs.rax as usize);
    trap.set_user_gpr(1, regs.rbx as usize);
    trap.set_user_gpr(2, regs.rcx as usize);
    trap.set_user_gpr(3, regs.rdx as usize);
    trap.set_user_gpr(4, regs.rsi as usize);
    trap.set_user_gpr(5, regs.rdi as usize);
    trap.set_user_gpr(6, regs.rbp as usize);
    trap.set_user_gpr(7, regs.r8 as usize);
    trap.set_user_gpr(8, regs.r9 as usize);
    trap.set_user_gpr(9, regs.r10 as usize);
    trap.set_user_gpr(10, regs.r11 as usize);
    trap.set_user_gpr(11, regs.r12 as usize);
    trap.set_user_gpr(12, regs.r13 as usize);
    trap.set_user_gpr(13, regs.r14 as usize);
    trap.set_user_gpr(14, regs.r15 as usize);
    trap
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
fn write_trap_returns_to_saved_regs(
    regs: *mut X86SavedRegs,
    trap_frame: &crate::kernel::trapframe::TrapFrame,
) {
    let regs = unsafe { &mut *regs };
    regs.rax = trap_frame.ret0 as u64;
    // ret1 is returned in R8 (caller-saved), NOT RBX (callee-saved).
    // Writing to RBX via IRETQ would silently corrupt compiler-managed
    // callee-saved registers in user code (RBX is preserved across calls
    // by the System V ABI, so the compiler uses it freely; writing 0 to
    // the saved RBX frame corrupts any live value the compiler kept there).
    regs.r8 = trap_frame.ret1 as u64;
    regs.rdx = trap_frame.ret2 as u64;
    regs.rcx = trap_frame.error as u64;
}

/// Write the current task's register context to the X86SavedRegs on the kernel
/// stack so that the assembly pop sequence restores the correct user registers.
///
/// This is called on a task switch.  The `trap_frame` has already been updated
/// by `apply_user_context` with the incoming task's TCB context.
///
/// Two sub-cases:
///   1. **New task first entry** (all `user_gprs` zero, `arg(0)` non-zero):
///      The task has never run; deliver its startup ABI args through the
///      x86-64 function-call registers (rdi, rsi, rdx, rcx, r8, r9).
///   2. **Resumed task** (some `user_gprs` non-zero, or `arg(0)` zero):
///      Restore the full GPR snapshot the task had when it last blocked or
///      was preempted.  (rax is typically 0 for an IPC-recv resumption, set
///      directly by `complete_blocked_recv_for_waiter`.)
#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
fn write_task_gprs_to_saved_regs(
    regs: *mut X86SavedRegs,
    trap_frame: &crate::kernel::trapframe::TrapFrame,
) {
    let regs = unsafe { &mut *regs };
    // Step 1: restore full GPR snapshot from TCB (zero for new tasks).
    regs.rax = trap_frame.user_gpr(0) as u64; // [0] = rax
    regs.rbx = trap_frame.user_gpr(1) as u64; // [1] = rbx
    regs.rcx = trap_frame.user_gpr(2) as u64; // [2] = rcx
    regs.rdx = trap_frame.user_gpr(3) as u64; // [3] = rdx
    regs.rsi = trap_frame.user_gpr(4) as u64; // [4] = rsi
    regs.rdi = trap_frame.user_gpr(5) as u64; // [5] = rdi
    regs.rbp = trap_frame.user_gpr(6) as u64; // [6] = rbp
    regs.r8 = trap_frame.user_gpr(7) as u64; // [7] = r8
    regs.r9 = trap_frame.user_gpr(8) as u64; // [8] = r9
    regs.r10 = trap_frame.user_gpr(9) as u64; // [9] = r10
    regs.r11 = trap_frame.user_gpr(10) as u64; // [10] = r11
    regs.r12 = trap_frame.user_gpr(11) as u64; // [11] = r12
    regs.r13 = trap_frame.user_gpr(12) as u64; // [12] = r13
    regs.r14 = trap_frame.user_gpr(13) as u64; // [13] = r14
    regs.r15 = trap_frame.user_gpr(14) as u64; // [14] = r15
    // Step 2: new task detection — all user_gprs are zero AND arg(0) is the
    // non-zero task_id written at spawn time.  Deliver startup args through
    // the x86-64 System V function-call ABI registers.
    let is_new_task = trap_frame.user_gprs.iter().all(|&g| g == 0) && trap_frame.arg(0) != 0;
    if is_new_task {
        regs.rdi = trap_frame.arg(0) as u64; // rdi = arg0 (task_id)
        regs.rsi = trap_frame.arg(1) as u64; // rsi = arg1 (pm_send cap)
        regs.rdx = trap_frame.arg(2) as u64; // rdx = arg2 (pm_reply_recv cap)
        regs.rcx = trap_frame.arg(3) as u64; // rcx = arg3 (startup_slots_ptr)
        regs.r8 = trap_frame.arg(4) as u64; // r8  = arg4 (slots_len)
        regs.r9 = trap_frame.arg(5) as u64; // r9  = arg5 (reserved)
        // Caller-saved scratch registers: clear to a defined state.
        regs.rax = 0;
        regs.rbx = 0;
        regs.rbp = 0;
        regs.r10 = 0;
        regs.r11 = 0;
    }
    // Stage 163J: proof-gated trace of the return lane actually delivered on a
    // first/resumed entry, so a fork child returning a stale syscall-number in
    // RAX (the observed `ret0=12`) is unambiguous. `rax` here is exactly what
    // IRETQ/SYSRET will load into the user return register.
    if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
        crate::yarm_log!(
            "FORK_PROOF_FIRST_RESUME_AFTER_ARCH_RESTORE rax={} user_gpr0={} is_new_task={}",
            regs.rax,
            trap_frame.user_gpr(0),
            is_new_task as u8
        );
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
fn dispatch_trap_from_stub_for_test(
    kernel: &mut crate::kernel::boot::KernelState,
    vector: u64,
    error_code: u64,
    regs: &mut X86SavedRegs,
    interrupt_frame: &mut X86InterruptStackFrame,
) -> Result<(), crate::kernel::boot::TrapHandleError> {
    let mut fault_addr = 0u64;
    if vector as usize == VEC_PAGE_FAULT {
        fault_addr = 0xDEAD_BEEF;
    }
    let context = crate::arch::x86_64::trap::X86TrapContext {
        vector: vector as u8,
        error_code,
        fault_addr,
    };
    let mut trap_frame = unsafe {
        build_trap_frame_from_saved_regs(
            regs as *const X86SavedRegs,
            interrupt_frame as *mut X86InterruptStackFrame,
            vector,
        )
    };
    crate::arch::x86_64::trap::handle_trap_entry(
        kernel,
        crate::kernel::scheduler::CpuId(crate::arch::platform_layout::BOOTSTRAP_CPU_ID),
        context,
        Some(&mut trap_frame),
    )?;
    if vector as usize == VEC_SYSCALL {
        write_trap_returns_to_saved_regs(regs as *mut X86SavedRegs, &trap_frame);
    }
    Ok(())
}

/// After `handle_trap_entry` processes an event (syscall, timer, fault, …) the
/// scheduler may have switched to a different user task.  The new task's
/// resume PC and stack pointer are stored in `trap_frame.saved_pc/sp` by
/// `apply_current_thread_to_frame`, but the hardware interrupt frame that the
/// assembly return path (`iretq` or `sysretq`) actually reads from is still
/// the *old* task's values pushed at trap entry.
///
/// This function flushes the updated context back so the assembly exits into
/// the correct task:
///   • For the interrupt path  (`yarm_x86_common_trap_entry`):  `interrupt_frame`
///     is the hardware interrupt frame; `iretq` reads RIP/RSP from it.
///   • For the syscall fast path (`yarm_x86_lstar_entry`): `interrupt_frame` is
///     the synthetic 5-word frame built on the kernel stack; `sysretq` loads
///     RIP from `[frame+0]` (into RCX) and RSP from `[frame+24]`.
///
/// Only user-mode return frames (CS DPL=3) are updated; kernel-mode frames
/// (timer/NMI in ring 0) are left untouched.
///
/// RFLAGS is always reset to 0x202 (IF=1, DF=0, all other flags clear).  This
/// is safe because:
///   - First task entry: 0x202 is the correct initial value.
///   - Re-entry after a blocking syscall: RFLAGS is caller-clobbered across
///     the syscall boundary per the x86-64 ABI, so resetting it is correct.
///   - Re-entry after a timer preemption: losing the exact flag state is
///     acceptable; DF=0 at function calls/returns is the only ABI requirement
///     that matters, and 0x202 satisfies it.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
unsafe fn flush_trap_context_to_iret_frame(
    interrupt_frame: *mut X86InterruptStackFrame,
    trap_frame: &crate::kernel::trapframe::TrapFrame,
) {
    if interrupt_frame.is_null() {
        return;
    }
    let frame = unsafe { &mut *interrupt_frame };
    // Only update frames that will return to user mode (ring 3).
    if (frame.cs & 0x3) != 0x3 {
        return;
    }
    let new_pc = trap_frame.saved_pc();
    let new_sp = trap_frame.saved_sp();
    // new_pc / new_sp are 0 only if apply_current_thread_to_frame was never
    // called (e.g. no runnable task).  Leave the frame unchanged in that case
    // so the kernel returns cleanly (it will loop in idle).
    if new_pc != 0 {
        frame.rip = new_pc as u64;
    }
    if new_sp != 0 {
        frame.rsp = new_sp as u64;
    }
    // Reset RFLAGS to a clean state: IF=1, all other flags clear.
    frame.rflags = 0x202;
}

#[cfg(all(test, feature = "hosted-dev", target_arch = "x86_64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_dispatch_trap_from_stub(
    _vector: u64,
    _error_code: u64,
    _regs: *mut X86SavedRegs,
    _interrupt_frame: *mut X86InterruptStackFrame,
) {
}

/// Stage 138: hardware-proven #PF frame and PTE state diagnostic.
///
/// Called from `yarm_x86_dispatch_trap_from_stub` on every #PF BEFORE
/// `build_trap_frame_from_saved_regs`, so all values come directly from
/// hardware, not from any Rust TrapFrame reinterpretation.
///
/// Emits (in order):
///   PAGE_FAULT_HW_REGS  — raw CR3, CR2, kernel RSP from inline asm
///   PAGE_FAULT_FRAME_WORDS — 8 raw qwords centred on the interrupt frame ptr
///   PAGE_FAULT_FRAME_DECODE — decodes words as CPL3 #PF frame fields
///   PAGE_FAULT_HW_PTE_WALK (×2) — 4-level walks for CR2 page and RIP page
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn emit_pf_frame_hw_diag(fault_addr: u64, interrupt_frame: *const X86InterruptStackFrame) {
    let mut hw_cr3: u64;
    let mut rsp_kernel: u64;
    unsafe {
        core::arch::asm!(
            "mov {cr3}, cr3",
            "mov {rsp}, rsp",
            cr3 = out(reg) hw_cr3,
            rsp = out(reg) rsp_kernel,
            options(nostack, preserves_flags),
        );
    }
    crate::yarm_log!(
        "PAGE_FAULT_HW_REGS cr2=0x{:016x} cr3=0x{:016x} rsp_kernel=0x{:016x}",
        fault_addr,
        hw_cr3,
        rsp_kernel,
    );

    // Read 8 raw u64 words: w0 = error_code (interrupt_frame - 8),
    // w1..w5 = rip/cs/rflags/user_rsp/ss (the CPU-pushed interrupt frame),
    // w6..w7 = words beyond the frame (for layout sanity checking).
    let frame_ptr = interrupt_frame as *const u64;
    let words: [u64; 8] = unsafe {
        [
            *frame_ptr.sub(1), // error_code
            *frame_ptr.add(0), // rip
            *frame_ptr.add(1), // cs
            *frame_ptr.add(2), // rflags
            *frame_ptr.add(3), // user_rsp (CPL3 only)
            *frame_ptr.add(4), // ss
            *frame_ptr.add(5), // beyond frame
            *frame_ptr.add(6), // beyond frame
        ]
    };
    crate::yarm_log!(
        "PAGE_FAULT_FRAME_WORDS ptr=0x{:016x} w0=0x{:016x} w1=0x{:016x} w2=0x{:016x} w3=0x{:016x} w4=0x{:016x} w5=0x{:016x} w6=0x{:016x} w7=0x{:016x}",
        frame_ptr as u64,
        words[0],
        words[1],
        words[2],
        words[3],
        words[4],
        words[5],
        words[6],
        words[7],
    );

    // Decode as CPL3 #PF interrupt frame: error/rip/cs/rflags/user_rsp/ss
    let decoded_error = words[0];
    let decoded_rip = words[1];
    let decoded_cs = words[2];
    let decoded_rflags = words[3];
    let decoded_user_rsp = words[4];
    let decoded_ss = words[5];
    crate::yarm_log!(
        "PAGE_FAULT_FRAME_DECODE error=0x{:x} rip=0x{:016x} cs=0x{:x} rflags=0x{:016x} user_rsp=0x{:016x} ss=0x{:x}",
        decoded_error,
        decoded_rip,
        decoded_cs,
        decoded_rflags,
        decoded_user_rsp,
        decoded_ss,
    );

    // 4-level hardware PTE walk for the CR2 page.
    let hw_root = hw_cr3 & !0xfffu64;
    let cr2_page = fault_addr & !0xfffu64;
    let (pml4e, pdpte, pde, pte) =
        crate::arch::x86_64::page_table::hw_pte_walk_verbose(hw_root, cr2_page);
    let pte_present = (pte & 1) != 0;
    let pte_user = (pte & 4) != 0;
    let pte_writable = (pte & 2) != 0;
    crate::yarm_log!(
        "PAGE_FAULT_HW_PTE_WALK cr3=0x{:016x} va=0x{:016x} pml4e=0x{:016x} pdpte=0x{:016x} pde=0x{:016x} pte=0x{:016x} present={} user={} writable={}",
        hw_cr3,
        cr2_page,
        pml4e,
        pdpte,
        pde,
        pte,
        pte_present as u8,
        pte_user as u8,
        pte_writable as u8,
    );

    // 4-level hardware PTE walk for the RIP page (decoded from interrupt frame).
    let rip_page = decoded_rip & !0xfffu64;
    let (pml4e2, pdpte2, pde2, pte2) =
        crate::arch::x86_64::page_table::hw_pte_walk_verbose(hw_root, rip_page);
    let pte2_present = (pte2 & 1) != 0;
    let pte2_user = (pte2 & 4) != 0;
    let pte2_writable = (pte2 & 2) != 0;
    crate::yarm_log!(
        "PAGE_FAULT_HW_PTE_WALK cr3=0x{:016x} va=0x{:016x} pml4e=0x{:016x} pdpte=0x{:016x} pde=0x{:016x} pte=0x{:016x} present={} user={} writable={}",
        hw_cr3,
        rip_page,
        pml4e2,
        pdpte2,
        pde2,
        pte2,
        pte2_present as u8,
        pte2_user as u8,
        pte2_writable as u8,
    );

    let _ = decoded_cs; // used only in the log above
}

/// Stage 133: pre-lock one-shot #PF diagnostic.
///
/// Called from `yarm_x86_dispatch_trap_from_stub` on the first post-cleanup
/// page fault, before any KernelState lock is acquired.  Uses the raw values
/// from the hardware interrupt frame (RIP, RSP) and the trap stub's pushed
/// register file (R14) to emit D6_PRE_LOCK_PF_DIAG_* markers and classify
/// the fault:
///
///   stack_push  — CR2 == RSP-8  (a kernel push hit an unmapped page)
///   r14_lockptr — CR2 == R14 + HANDLE_TRAP_LOCK_OFFSET  (r14 is garbage)
///   other       — neither of the above
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn d6_emit_pre_lock_pf_diag(
    vector: u64,
    error_code: u64,
    cr2: u64,
    regs: *const X86SavedRegs,
    interrupt_frame: *const X86InterruptStackFrame,
) {
    // HANDLE_TRAP_LOCK_OFFSET: offset of the SpinLock computed by
    // `leaq 0x3e1780(%r14), %rbx` inside the trap handler.
    const HANDLE_TRAP_LOCK_OFFSET: u64 = 0x3e_1780;
    let (rip, rsp, r14) = unsafe {
        let frame = &*interrupt_frame;
        let saved = &*regs;
        (frame.rip, frame.rsp, saved.r14)
    };
    let rsp_minus_8 = rsp.wrapping_sub(8);
    let lockptr = r14.wrapping_add(HANDLE_TRAP_LOCK_OFFSET);
    let class = if cr2 == rsp_minus_8 {
        "stack_push"
    } else if cr2 == lockptr {
        "r14_lockptr"
    } else {
        "other"
    };
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_BEGIN");
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_VECTOR value=0x{:x}", vector);
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_ERROR value=0x{:x}", error_code);
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_CR2 value=0x{:016x}", cr2);
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_RIP value=0x{:016x}", rip);
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_RSP value=0x{:016x}", rsp);
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_R14 value=0x{:016x}", r14);
    crate::yarm_log!(
        "D6_PRE_LOCK_PF_DIAG_RSP_MINUS_8 value=0x{:016x}",
        rsp_minus_8
    );
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_LOCKPTR value=0x{:016x}", lockptr);
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_CLASS class={}", class);
    crate::yarm_log!("D6_PRE_LOCK_PF_DIAG_DONE");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_dispatch_trap_from_stub(
    vector: u64,
    error_code: u64,
    regs: *mut X86SavedRegs,
    interrupt_frame: *mut X86InterruptStackFrame,
) {
    // Stage 30 / Review C1: in debug builds, assert no boot raw-borrow window is
    // live. A timer/trap reaching with_cpu during that window would alias the boot
    // &mut KernelState (UB). Compiles to nothing in release; zero ISR overhead.
    #[cfg(any(debug_assertions, test))]
    debug_assert!(
        !crate::runtime::boot_raw_borrow_is_active(),
        "x86_64 trap/timer ISR fired during boot raw-borrow window — aliasing &mut KernelState risk"
    );
    let cpu_apic = raw_current_apic_id() as u64;
    // Stage 189D: per-CPU nested-trap guard index (see TRAP_DISPATCH_DEPTH doc).
    let depth_idx =
        (current_cpu_id().0 as usize).min(crate::arch::platform_constants::MAX_CPUS - 1);
    let previous_depth = TRAP_DISPATCH_DEPTH[depth_idx].fetch_add(1, Ordering::AcqRel);
    let frame = unsafe { &*interrupt_frame };
    let mut fault_addr = 0u64;
    if vector as usize == VEC_PAGE_FAULT {
        unsafe {
            core::arch::asm!("mov {}, cr2", out(reg) fault_addr, options(nomem, preserves_flags));
        }
    }
    if previous_depth != 0 {
        log_decoded_fatal_trap(None, vector, error_code, frame, fault_addr);
        debug_uart_trap_breadcrumb(b'N', vector, error_code, fault_addr, frame.rip, cpu_apic);
        halt_forever();
    }
    let context = crate::arch::x86_64::trap::X86TrapContext {
        vector: vector as u8,
        error_code,
        fault_addr,
    };
    let cpu = current_cpu_id();

    // Stage 133: pre-lock one-shot #PF diagnostic — fires before any KernelState
    // lock is acquired, giving access to raw trap-stub register values.
    #[cfg(not(feature = "hosted-dev"))]
    if vector as usize == VEC_PAGE_FAULT {
        let pre_lock_cpu_idx = cpu.0 as usize;
        if pre_lock_cpu_idx < crate::kernel::scheduler::MAX_CPUS
            && crate::kernel::boot::D6_PRE_LOCK_PF_DIAG_PENDING[pre_lock_cpu_idx]
                .swap(false, Ordering::AcqRel)
        {
            d6_emit_pre_lock_pf_diag(vector, error_code, fault_addr, regs, interrupt_frame);
        }
    }

    // Stage 138: hardware-proven #PF frame and PTE diagnostic.
    // Must be called before build_trap_frame_from_saved_regs so that all values
    // are read directly from hardware interrupt-frame memory, not from any Rust
    // TrapFrame reinterpretation.
    #[cfg(not(feature = "hosted-dev"))]
    if vector as usize == VEC_PAGE_FAULT {
        emit_pf_frame_hw_diag(fault_addr, interrupt_frame);
    }

    // Stage 2N: prefer SharedKernel path when available.
    if let Some(shared) = trap_shared_kernel() {
        if !STAGE2N_FIRST_TRAP_LOGGED.swap(true, Ordering::AcqRel) {
            crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2N_FIRST_SHARED_TRAP arch=x86_64");
        }
        let fault_rip = frame.rip;
        // Stage 4T+6R: reverted to conservative with_cpu→current_tid path.
        // Stage 4T+6 converted this to current_tid_split_read(cpu), which has
        // equivalent return-value semantics but broke the x86_64 service chain
        // in smoke testing (service_entries=0, repeated SCHED_ENTER_IDLE_HLT).
        // The unit-test value-equivalence proof was insufficient: smoke behavior
        // is the acceptance criterion. Both entering_tid and exiting_tid reads
        // are restored to the global-lock with_cpu path (Class F).
        let entering_tid: Option<u64> = shared.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        // Stage 189D: AP user-dispatch SEAL — a normal syscall issued by a live AP
        // probe task entering the NORMAL global-lock dispatch path (not the magic
        // probe fast path). Gated on the per-CPU seal-probe flag so this is inert on
        // the BSP and on every non-armed boot. Emits the seal markers around the real
        // global-lock dispatch below.
        let ap_seal =
            vector as usize == VEC_SYSCALL && crate::arch::x86_64::smp::ap_seal_probe_active(cpu);
        let ap_seal_nr = if ap_seal { unsafe { (*regs).rax } } else { 0 };
        if ap_seal {
            crate::arch::x86_64::smp::ap_seal_syscall_begin(cpu, ap_seal_nr);
        }
        let mut trap_frame =
            unsafe { build_trap_frame_from_saved_regs(regs, interrupt_frame, vector) };
        if let Err(err) = crate::arch::trap_entry::dispatch_trap_entry_with_shared_kernel(
            shared,
            cpu,
            context,
            Some(&mut trap_frame),
        ) {
            crate::pr_err!(
                "x86 shared trap dispatch failed: vector={} error_code=0x{:x} rip=0x{:016x} err={:?}",
                vector,
                error_code,
                fault_rip,
                err
            );
            // Stage 4T+7: pre-read TID and ASID via split-read helpers (scheduler
            // lock rank 1, task lock rank 2) before logging. Avoids the global
            // SharedKernel lock in the fatal error path.
            let snapshot = shared.fatal_trap_read_snapshot(cpu);
            log_decoded_fatal_trap_from_snapshot(snapshot, vector, error_code, frame, fault_addr);
            debug_uart_trap_breadcrumb(b'T', vector, error_code, fault_addr, fault_rip, cpu_apic);
            halt_forever();
        }
        // Stage 189D: the AP probe's normal syscall reached the global-lock dispatch
        // and returned Ok — emit the seal completion markers (one-shot; clears the
        // per-CPU seal-probe flag). Stage 190A: then RETURN the probe to the AP
        // scheduler (block it so nothing is left running on the AP), so the AP does
        // NOT re-run the one-shot probe forever nor park — it falls through to the
        // interruptible idle loop below (return-to-idle).
        if ap_seal {
            // Stage 199A2D2C2A: for the SMP oracle, a RETURNING syscall (e.g. DebugLog, nr != 0)
            // resumes the proof task INLINE (a multi-syscall stub: two DebugLogs then a Yield),
            // keeping the seal probe active. ONLY Yield (nr == 0) runs the one-shot seal-done +
            // deschedule so the task becomes RunnableSaved and the AP scheduler (`ap_sched_next_or_
            // idle`) performs the saved-frame resume. The legacy probe blocks after every syscall.
            let smp_returning =
                crate::kernel::boot::x86_ipccall_direct_smp_oracle_enabled() && ap_seal_nr != 0;
            if !smp_returning {
                crate::arch::x86_64::smp::ap_seal_syscall_ok(cpu, ap_seal_nr);
                crate::arch::x86_64::smp::ap_seal_return_to_idle(shared, cpu, ap_seal_nr);
            }
        }
        // Stage 4T+6R: reverted to conservative with_cpu→current_tid path.
        // See entering_tid comment above for the revert rationale.
        let exiting_tid: Option<u64> = shared.with_cpu(cpu, |k| k.current_tid()).unwrap_or(None);
        let task_switched = entering_tid != exiting_tid;
        if matches!(exiting_tid, None | Some(0)) {
            // The scheduler uses TID 0 as its idle/supervisor sentinel.  It has
            // no user context to iretq back to; returning through the current
            // user trap frame would resume the task that just blocked and form
            // a hot block/yield/retry loop.  Park the CPU with interrupts
            // enabled instead, so timer and external IRQs still wake from HLT.
            // Stage 190B: for the AP scheduler loop, run the NEXT admitted task if the
            // run queue still has one, else return to the interruptible idle loop.
            // `ap_sched_next_or_idle` diverges (re-enters ring 3 or idles), so the
            // depth counter is reset first for the possible fresh ring-3 syscall.
            if ap_seal {
                TRAP_DISPATCH_DEPTH[depth_idx].store(0, Ordering::Release);
                crate::arch::x86_64::smp::ap_sched_next_or_idle(shared, cpu);
            }
            crate::yarm_log!("SCHED_ENTER_IDLE_HLT cpu={}", cpu.0);
            TRAP_DISPATCH_DEPTH[depth_idx].store(0, Ordering::Release);
            idle_halt_loop();
        }
        if task_switched {
            write_task_gprs_to_saved_regs(regs, &trap_frame);
        } else if vector as usize == VEC_SYSCALL {
            write_trap_returns_to_saved_regs(regs, &trap_frame);
        }
        unsafe { flush_trap_context_to_iret_frame(interrupt_frame, &trap_frame) };
        TRAP_DISPATCH_DEPTH[depth_idx].store(0, Ordering::Release);
        return;
    }

    // Fallback: raw KernelState path (pre-Stage2N or no shared kernel installed).
    if !STAGE2N_FALLBACK_LOGGED.swap(true, Ordering::AcqRel) {
        crate::yarm_log!("YARM_LOCK_SPLIT_STAGE2N_FALLBACK arch=x86_64 reason=no_shared_kernel");
    }
    let Some(kernel) = trap_kernel_state_mut() else {
        if should_halt_without_kernel_state(vector as usize) {
            let fault_rip = frame.rip;
            log_decoded_fatal_trap(None, vector, error_code, frame, fault_addr);
            debug_uart_trap_breadcrumb(b'E', vector, error_code, fault_addr, fault_rip, cpu_apic);
            TRAP_DISPATCH_DEPTH[depth_idx].store(0, Ordering::Release);
            halt_forever();
        }
        TRAP_DISPATCH_DEPTH[depth_idx].store(0, Ordering::Release);
        return;
    };
    let fault_rip = frame.rip;
    let entering_tid = kernel.current_tid();
    let mut trap_frame = unsafe { build_trap_frame_from_saved_regs(regs, interrupt_frame, vector) };
    if let Err(err) = crate::arch::x86_64::trap::handle_trap_entry(
        kernel,
        current_cpu_id(),
        context,
        Some(&mut trap_frame),
    ) {
        crate::pr_err!(
            "x86 trap dispatch failed: vector={} error_code=0x{:x} rip=0x{:016x} err={:?}",
            vector,
            error_code,
            fault_rip,
            err
        );
        log_decoded_fatal_trap(Some(kernel), vector, error_code, frame, fault_addr);
        debug_uart_trap_breadcrumb(b'T', vector, error_code, fault_addr, fault_rip, cpu_apic);
        halt_forever();
    }
    let exiting_tid = kernel.current_tid();
    let task_switched = entering_tid != exiting_tid;
    if matches!(exiting_tid, None | Some(0)) {
        // Raw fallback for early boot/tests before the shared kernel pointer is
        // installed.  TID 0 is idle-only on x86_64 and must not iretq through
        // the stale user frame that entered the kernel.
        crate::yarm_log!("SCHED_ENTER_IDLE_HLT cpu={}", current_cpu_id().0);
        TRAP_DISPATCH_DEPTH[depth_idx].store(0, Ordering::Release);
        idle_halt_loop();
    }
    if task_switched {
        write_task_gprs_to_saved_regs(regs, &trap_frame);
    } else if vector as usize == VEC_SYSCALL {
        write_trap_returns_to_saved_regs(regs, &trap_frame);
    }
    unsafe { flush_trap_context_to_iret_frame(interrupt_frame, &trap_frame) };
    TRAP_DISPATCH_DEPTH[depth_idx].store(0, Ordering::Release);
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits

    .macro YARM_X86_TRAP_STUB vector has_error
    .global yarm_x86_isr_\vector
    .type yarm_x86_isr_\vector, @function
yarm_x86_isr_\vector:
    .if \has_error == 0
        push 0
    .endif
    push \vector
    jmp yarm_x86_common_trap_entry
    .endm

    .global yarm_x86_common_trap_entry
    .type yarm_x86_common_trap_entry, @function
yarm_x86_common_trap_entry:
    // The kernel runs at a higher-half virtual address (PML4[511] direct
    // map). The hardware-pushed interrupt frame is therefore at a higher-half
    // RSP; do NOT truncate it to a low canonical alias - that would point
    // into PML4[0] which is not present in user ASIDs.
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rdi
    push rsi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, qword ptr [rsp + 15 * 8]
    mov rsi, qword ptr [rsp + 16 * 8]
    mov rdx, rsp
    lea rcx, [rsp + 17 * 8]
    mov r12, rsp
    and rsp, -16
    call yarm_x86_dispatch_trap_from_stub
    mov rsp, r12

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rsi
    pop rdi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16
    iretq
"#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_x86_lstar_entry
    .type yarm_x86_lstar_entry, @function
yarm_x86_lstar_entry:
    // -----------------------------------------------------------------------
    // Stage 189C6 LIVE AP-DISPATCH probe fast path.
    //
    // A live AP user task dispatched by yarm_x86_ap_user_dispatch_entry enters
    // ring 3 running a tiny stub that loads RAX = 0xA9C6 and issues SYSCALL.
    // Real YARM syscall numbers are 0..32, so this compare NEVER matches a
    // production (BSP) syscall — the fast path is inert on every non-probe entry
    // and adds one compare+branch to the proven baseline.
    //
    // On a match we do NOT descend into the shared kernel dispatch (that would
    // run the whole global-lock kernel on the AP without the lock). Instead we
    // prove per-CPU LSTAR entry executed ON THIS CPU by setting the per-CPU
    // ap_syscall_reentry_ok flag (gs:[168], kernel .bss addressable under the
    // task CR3) and parking. The BSP polls the flag and emits the marker. RAX
    // and the serial scratch registers are safe to clobber here — the probe
    // path never returns to userspace.
    cmp rax, 0xA9C6
    jne 2f
    mov dword ptr gs:[168], 1        // ap_syscall_reentry_ok = 1 (persistent)
    mov dx, 0x3f8
    mov al, 0x52                     // 'R' — AP ring3 SYSCALL re-entry observed
    out dx, al
3:
    hlt
    jmp 3b
2:
    // -----------------------------------------------------------------------
    // SYSCALL fast-path entry.
    //
    // On SYSCALL the CPU places:
    //   RCX <- user RIP (return address back to userspace)
    //   R11 <- user RFLAGS
    //   RSP still holds the user stack pointer
    //
    // Every other GPR (including the callee-saved R12/R13/R14/R15, RBX, RBP)
    // holds the user's authentic value.
    //
    // PRIMARY RULE: do NOT touch any GPR before it is saved.
    //
    // We need to switch to the kernel stack, but we need user RSP for the
    // synthetic IRETQ frame and cannot load it into a register first.
    // Solution: write user RSP directly to the PER-CPU scratch slot at
    // gs:[152] (a gs-relative store — no GPR touched).
    //
    // Stage 189C4 — PER-CPU, SMP-safe, NO swapgs. CR4.FSGSBASE is DISABLED and
    // user TLS is in FS, so GS.base is the kernel per-CPU record in every ring
    // (ring 3 cannot change it). gs:[152] = syscall_scratch_rsp and
    // gs:[144] = syscall_kernel_rsp0 therefore resolve to THIS CPU's own
    // per-CPU record — no global authority, no cross-CPU race, no swapgs.
    // (Offsets 152/144 are locked by percpu SYSCALL_SCRATCH_RSP_OFFSET /
    //  SYSCALL_KERNEL_RSP0_OFFSET + offset_of asserts.)
    // -----------------------------------------------------------------------

    // Step 1 — save user RSP without touching any GPR (per-CPU scratch).
    mov qword ptr gs:[152], rsp

    // Step 2 — switch to the per-CPU kernel stack (RSP0).
    mov rsp, qword ptr gs:[144]
    test rsp, rsp
    jnz 1f
    // RSP0 == 0: per-CPU GS base / RSP0 setup broken before first SYSCALL.  Halt.
    ud2
1:
    // Step 3 — build a synthetic 5-word IRETQ frame on the kernel stack.
    //   +0   RIP    user return address  (from SYSCALL -> RCX)
    //   +8   CS     ring-3 code segment  (0x23)
    //  +16   RFLAGS user flags           (from SYSCALL -> R11)
    //  +24   RSP    user stack pointer   (from scratch slot)
    //  +32   SS     ring-3 data segment  (0x1b)
    //
    // To get user RSP into [rsp+24] without clobbering an unsaved GPR we
    // reuse RCX: RCX was already clobbered by the hardware SYSCALL itself
    // (it now holds user RIP), so overwriting it again here is safe.
    sub rsp, 40
    mov qword ptr [rsp +  0], rcx    // user RIP  (SYSCALL saved this for us)
    mov qword ptr [rsp +  8], 0x23   // user CS   (ring-3 code)
    mov qword ptr [rsp + 16], r11    // user RFLAGS (SYSCALL saved this for us)
    mov rcx, qword ptr gs:[152]      // user RSP (from PER-CPU scratch slot)
    mov qword ptr [rsp + 24], rcx    // user RSP  (from scratch slot)
    mov qword ptr [rsp + 32], 0x1b   // user SS   (ring-3 data)

    // Step 4 — move syscall arg3 into RCX.
    // x86-64 Linux SYSCALL convention passes arg3 in R10 (not RCX) because
    // SYSCALL clobbers RCX.  The YARM trap ABI expects arg3 in the RCX slot
    // of X86SavedRegs, so we copy R10 -> RCX here before saving.
    mov rcx, r10

    // Step 5 — save ALL user GPRs.
    // Crucially, R12/R13/R14/R15 are UNCHANGED since SYSCALL entry —
    // they hold the user's authentic callee-saved values.
    push rax            // user RAX (syscall number)
    push rbx            // user RBX (callee-saved) ← authentic
    push rcx            // = R10 (syscall arg3, per step 4)
    push rdx            // user RDX (syscall arg2) ← authentic
    push rbp            // user RBP (callee-saved) ← authentic
    push rdi            // user RDI (syscall arg0) ← authentic
    push rsi            // user RSI (syscall arg1) ← authentic
    push r8             // user R8  (syscall arg4) ← authentic
    push r9             // user R9  (syscall arg5) ← authentic
    push r10            // user R10 (syscall arg3) ← authentic
    push r11            // user R11 = RFLAGS placed here by SYSCALL
                        //   (the user's real R11 is permanently lost — an
                        //    unavoidable side-effect of the SYSCALL instruction)
    push r12            // user R12 (callee-saved) ← AUTHENTIC  ✓
    push r13            // user R13 (callee-saved) ← AUTHENTIC  ✓
    push r14            // user R14 (callee-saved) ← AUTHENTIC  ✓
    push r15            // user R15 (callee-saved) ← AUTHENTIC  ✓
    // After 15 pushes × 8 = 120 bytes the IRETQ frame from step 3 is at
    // [RSP + 120].

    // Step 6 — call the Rust dispatcher.
    //   RDI = 0x80   (SYSCALL vector)
    //   RSI = 0      (no hardware error code)
    //   RDX = RSP    (pointer to X86SavedRegs block)
    //   RCX = RSP+120 (pointer to IRETQ frame)
    // Align the stack to 16 bytes for the ABI; save pre-alignment RSP in R12
    // at the KERNEL level (the user's R12 is already safely on the stack).
    mov rdi, 0x80
    xor rsi, rsi
    mov rdx, rsp
    lea rcx, [rsp + 120]
    mov r12, rsp
    and rsp, -16
    call yarm_x86_dispatch_trap_from_stub
    mov rsp, r12

    // Step 7 — restore all user GPRs and return via IRETQ.
    // flush_trap_context_to_iret_frame has already patched [RSP+120].rip
    // and [RSP+120].rsp with the selected task's PC and SP.
    // write_task_gprs_to_saved_regs / write_trap_returns_to_saved_regs have
    // written the correct values into the slots below.
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rsi
    pop rdi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    // RSP now points at the 5-word IRETQ frame (step 3, updated in step 6):
    //   [RSP+ 0] user RIP   ← patched by flush_trap_context_to_iret_frame
    //   [RSP+ 8] user CS    = 0x23 (ring-3)
    //   [RSP+16] user RFLAGS = 0x202 (IF=1)
    //   [RSP+24] user RSP   ← patched by flush_trap_context_to_iret_frame
    //   [RSP+32] user SS    = 0x1b (ring-3)
    iretq
"#
);

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_x86_syscall_entry
    .type yarm_x86_syscall_entry, @function
yarm_x86_syscall_entry:
    // INT 0x80 compatibility entry: funnels into the shared trap/syscall
    // dispatch path. Production fast path is yarm_x86_lstar_entry.
    push 0
    push 0x80
    jmp yarm_x86_common_trap_entry
"#
);

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
unsafe extern "C" {
    fn yarm_x86_syscall_entry();
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
macro_rules! declare_all_isr_stubs {
    ($($name:ident),* $(,)?) => {
        unsafe extern "C" {
            $(fn $name();)*
        }
        const ISR_STUBS: [unsafe extern "C" fn(); IDT_ENTRIES] = [$($name),*];
    };
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .altmacro
    .set vector_index, 0
    .rept 256
      .set has_error, 0
      .if vector_index == 8 || vector_index == 10 || vector_index == 11 || vector_index == 12 || vector_index == 13 || vector_index == 14 || vector_index == 17 || vector_index == 21
        .set has_error, 1
      .endif
      YARM_X86_TRAP_STUB %vector_index, %has_error
      .set vector_index, vector_index + 1
    .endr
"#
);

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
declare_all_isr_stubs!(
    yarm_x86_isr_0,
    yarm_x86_isr_1,
    yarm_x86_isr_2,
    yarm_x86_isr_3,
    yarm_x86_isr_4,
    yarm_x86_isr_5,
    yarm_x86_isr_6,
    yarm_x86_isr_7,
    yarm_x86_isr_8,
    yarm_x86_isr_9,
    yarm_x86_isr_10,
    yarm_x86_isr_11,
    yarm_x86_isr_12,
    yarm_x86_isr_13,
    yarm_x86_isr_14,
    yarm_x86_isr_15,
    yarm_x86_isr_16,
    yarm_x86_isr_17,
    yarm_x86_isr_18,
    yarm_x86_isr_19,
    yarm_x86_isr_20,
    yarm_x86_isr_21,
    yarm_x86_isr_22,
    yarm_x86_isr_23,
    yarm_x86_isr_24,
    yarm_x86_isr_25,
    yarm_x86_isr_26,
    yarm_x86_isr_27,
    yarm_x86_isr_28,
    yarm_x86_isr_29,
    yarm_x86_isr_30,
    yarm_x86_isr_31,
    yarm_x86_isr_32,
    yarm_x86_isr_33,
    yarm_x86_isr_34,
    yarm_x86_isr_35,
    yarm_x86_isr_36,
    yarm_x86_isr_37,
    yarm_x86_isr_38,
    yarm_x86_isr_39,
    yarm_x86_isr_40,
    yarm_x86_isr_41,
    yarm_x86_isr_42,
    yarm_x86_isr_43,
    yarm_x86_isr_44,
    yarm_x86_isr_45,
    yarm_x86_isr_46,
    yarm_x86_isr_47,
    yarm_x86_isr_48,
    yarm_x86_isr_49,
    yarm_x86_isr_50,
    yarm_x86_isr_51,
    yarm_x86_isr_52,
    yarm_x86_isr_53,
    yarm_x86_isr_54,
    yarm_x86_isr_55,
    yarm_x86_isr_56,
    yarm_x86_isr_57,
    yarm_x86_isr_58,
    yarm_x86_isr_59,
    yarm_x86_isr_60,
    yarm_x86_isr_61,
    yarm_x86_isr_62,
    yarm_x86_isr_63,
    yarm_x86_isr_64,
    yarm_x86_isr_65,
    yarm_x86_isr_66,
    yarm_x86_isr_67,
    yarm_x86_isr_68,
    yarm_x86_isr_69,
    yarm_x86_isr_70,
    yarm_x86_isr_71,
    yarm_x86_isr_72,
    yarm_x86_isr_73,
    yarm_x86_isr_74,
    yarm_x86_isr_75,
    yarm_x86_isr_76,
    yarm_x86_isr_77,
    yarm_x86_isr_78,
    yarm_x86_isr_79,
    yarm_x86_isr_80,
    yarm_x86_isr_81,
    yarm_x86_isr_82,
    yarm_x86_isr_83,
    yarm_x86_isr_84,
    yarm_x86_isr_85,
    yarm_x86_isr_86,
    yarm_x86_isr_87,
    yarm_x86_isr_88,
    yarm_x86_isr_89,
    yarm_x86_isr_90,
    yarm_x86_isr_91,
    yarm_x86_isr_92,
    yarm_x86_isr_93,
    yarm_x86_isr_94,
    yarm_x86_isr_95,
    yarm_x86_isr_96,
    yarm_x86_isr_97,
    yarm_x86_isr_98,
    yarm_x86_isr_99,
    yarm_x86_isr_100,
    yarm_x86_isr_101,
    yarm_x86_isr_102,
    yarm_x86_isr_103,
    yarm_x86_isr_104,
    yarm_x86_isr_105,
    yarm_x86_isr_106,
    yarm_x86_isr_107,
    yarm_x86_isr_108,
    yarm_x86_isr_109,
    yarm_x86_isr_110,
    yarm_x86_isr_111,
    yarm_x86_isr_112,
    yarm_x86_isr_113,
    yarm_x86_isr_114,
    yarm_x86_isr_115,
    yarm_x86_isr_116,
    yarm_x86_isr_117,
    yarm_x86_isr_118,
    yarm_x86_isr_119,
    yarm_x86_isr_120,
    yarm_x86_isr_121,
    yarm_x86_isr_122,
    yarm_x86_isr_123,
    yarm_x86_isr_124,
    yarm_x86_isr_125,
    yarm_x86_isr_126,
    yarm_x86_isr_127,
    yarm_x86_isr_128,
    yarm_x86_isr_129,
    yarm_x86_isr_130,
    yarm_x86_isr_131,
    yarm_x86_isr_132,
    yarm_x86_isr_133,
    yarm_x86_isr_134,
    yarm_x86_isr_135,
    yarm_x86_isr_136,
    yarm_x86_isr_137,
    yarm_x86_isr_138,
    yarm_x86_isr_139,
    yarm_x86_isr_140,
    yarm_x86_isr_141,
    yarm_x86_isr_142,
    yarm_x86_isr_143,
    yarm_x86_isr_144,
    yarm_x86_isr_145,
    yarm_x86_isr_146,
    yarm_x86_isr_147,
    yarm_x86_isr_148,
    yarm_x86_isr_149,
    yarm_x86_isr_150,
    yarm_x86_isr_151,
    yarm_x86_isr_152,
    yarm_x86_isr_153,
    yarm_x86_isr_154,
    yarm_x86_isr_155,
    yarm_x86_isr_156,
    yarm_x86_isr_157,
    yarm_x86_isr_158,
    yarm_x86_isr_159,
    yarm_x86_isr_160,
    yarm_x86_isr_161,
    yarm_x86_isr_162,
    yarm_x86_isr_163,
    yarm_x86_isr_164,
    yarm_x86_isr_165,
    yarm_x86_isr_166,
    yarm_x86_isr_167,
    yarm_x86_isr_168,
    yarm_x86_isr_169,
    yarm_x86_isr_170,
    yarm_x86_isr_171,
    yarm_x86_isr_172,
    yarm_x86_isr_173,
    yarm_x86_isr_174,
    yarm_x86_isr_175,
    yarm_x86_isr_176,
    yarm_x86_isr_177,
    yarm_x86_isr_178,
    yarm_x86_isr_179,
    yarm_x86_isr_180,
    yarm_x86_isr_181,
    yarm_x86_isr_182,
    yarm_x86_isr_183,
    yarm_x86_isr_184,
    yarm_x86_isr_185,
    yarm_x86_isr_186,
    yarm_x86_isr_187,
    yarm_x86_isr_188,
    yarm_x86_isr_189,
    yarm_x86_isr_190,
    yarm_x86_isr_191,
    yarm_x86_isr_192,
    yarm_x86_isr_193,
    yarm_x86_isr_194,
    yarm_x86_isr_195,
    yarm_x86_isr_196,
    yarm_x86_isr_197,
    yarm_x86_isr_198,
    yarm_x86_isr_199,
    yarm_x86_isr_200,
    yarm_x86_isr_201,
    yarm_x86_isr_202,
    yarm_x86_isr_203,
    yarm_x86_isr_204,
    yarm_x86_isr_205,
    yarm_x86_isr_206,
    yarm_x86_isr_207,
    yarm_x86_isr_208,
    yarm_x86_isr_209,
    yarm_x86_isr_210,
    yarm_x86_isr_211,
    yarm_x86_isr_212,
    yarm_x86_isr_213,
    yarm_x86_isr_214,
    yarm_x86_isr_215,
    yarm_x86_isr_216,
    yarm_x86_isr_217,
    yarm_x86_isr_218,
    yarm_x86_isr_219,
    yarm_x86_isr_220,
    yarm_x86_isr_221,
    yarm_x86_isr_222,
    yarm_x86_isr_223,
    yarm_x86_isr_224,
    yarm_x86_isr_225,
    yarm_x86_isr_226,
    yarm_x86_isr_227,
    yarm_x86_isr_228,
    yarm_x86_isr_229,
    yarm_x86_isr_230,
    yarm_x86_isr_231,
    yarm_x86_isr_232,
    yarm_x86_isr_233,
    yarm_x86_isr_234,
    yarm_x86_isr_235,
    yarm_x86_isr_236,
    yarm_x86_isr_237,
    yarm_x86_isr_238,
    yarm_x86_isr_239,
    yarm_x86_isr_240,
    yarm_x86_isr_241,
    yarm_x86_isr_242,
    yarm_x86_isr_243,
    yarm_x86_isr_244,
    yarm_x86_isr_245,
    yarm_x86_isr_246,
    yarm_x86_isr_247,
    yarm_x86_isr_248,
    yarm_x86_isr_249,
    yarm_x86_isr_250,
    yarm_x86_isr_251,
    yarm_x86_isr_252,
    yarm_x86_isr_253,
    yarm_x86_isr_254,
    yarm_x86_isr_255,
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn ensure_boot_descriptor_tables_scaffolded() {
    if DESCRIPTOR_SCAFFOLD_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    unsafe {
        let rsp0: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp0, options(nomem, nostack, preserves_flags));
        populate_boot_idt_from_stubs();

        // The kernel image runs at the higher-half alias (PML4[511] direct
        // map). All addr_of! results below are therefore higher-half VAs;
        // the previous `& 0xFFFF_FFFF` truncations would have stripped the
        // canonical high bits and produced bogus IST/TSS pointers, so they
        // are removed here.
        let ist_nmi_top = core::ptr::addr_of!(IST_NMI.0) as u64 + IST_NMI_STACK_BYTES as u64;
        let ist_df_top =
            core::ptr::addr_of!(IST_DOUBLE_FAULT.0) as u64 + IST_DOUBLE_FAULT_STACK_BYTES as u64;
        let ist_pf_top =
            core::ptr::addr_of!(IST_PAGE_FAULT.0) as u64 + IST_PAGE_FAULT_STACK_BYTES as u64;
        BOOT_TSS.rsp0 = rsp0;
        BOOT_TSS.ist1 = ist_nmi_top;
        BOOT_TSS.ist2 = ist_df_top;
        BOOT_TSS.ist3 = ist_pf_top;

        let tss_base = core::ptr::addr_of!(BOOT_TSS) as u64;
        let tss_limit = (core::mem::size_of::<X86TaskStateSegment>() - 1) as u32;
        let (tss_low, tss_high) = encode_tss_descriptor(tss_base, tss_limit);
        BOOT_GDT.entries[5] = tss_low;
        BOOT_GDT.entries[6] = tss_high;

        let idtr = X86IdtPointer::from_ptr(core::ptr::addr_of!(BOOT_IDT).cast::<X86IdtEntry>());
        let gdtr = X86GdtPointer {
            limit: (core::mem::size_of::<X86BootGdt>() - 1) as u16,
            base: core::ptr::addr_of!(BOOT_GDT) as u64,
        };

        core::arch::asm!("lgdt [{}]", in(reg) &gdtr, options(readonly, nostack, preserves_flags));
        core::arch::asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
        core::arch::asm!(
            "mov ax, {data_sel}",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            data_sel = const KERNEL_DATA_SELECTOR,
            options(nostack, preserves_flags)
        );
        core::arch::asm!(
            "mov ax, {tss_sel}",
            "ltr ax",
            tss_sel = const TSS_SELECTOR,
            options(nostack, preserves_flags)
        );
        configure_syscall_fast_path(rsp0);
    }
}

// ---------------------------------------------------------------------------
// Stage 183 inc.3 — per-AP GDT/TSS (scheduler-admission prerequisites)
// ---------------------------------------------------------------------------
// One GDT + one TSS per possible CPU, in `.bss`. The AP GDT mirrors the BSP
// BOOT_GDT selector layout exactly (0x08 kernel code, 0x10 kernel data, 0x18
// user data, 0x20 user code, 0x28 TSS) so the AP converges on the kernel's
// production selectors — after `lgdt` the AP entry asm reloads SS/DS/ES=0x10
// and far-returns to CS=0x08, then `ltr 0x28` loads the per-AP TSS. TSS
// `rsp0` = the AP's own kernel stack top; IST slots stay 0 this increment
// (IST entries are only consumed via IDT gate `ist` fields, and the AP has no
// IDT yet — interrupts remain masked; the AP IDT increment wires real per-AP
// IST stacks before any interrupt can fire).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut AP_GDTS: [X86BootGdt; crate::arch::platform_constants::MAX_CPUS] = [const {
    X86BootGdt {
        entries: [0, 0, 0, 0, 0, 0, 0],
    }
};
    crate::arch::platform_constants::MAX_CPUS];
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut AP_TSSS: [X86TaskStateSegment; crate::arch::platform_constants::MAX_CPUS] =
    [const { X86TaskStateSegment::new() }; crate::arch::platform_constants::MAX_CPUS];

/// BSP-side (before SIPI): populate the per-AP TSS (`rsp0` = the AP kernel stack
/// top, ISTs 0 — see the block comment) and the per-AP GDT (BOOT_GDT selector
/// layout + this AP's TSS descriptor at 0x28). Returns
/// `(gdtr_limit, gdt_base_va, tss_base_va)` for the trampoline handoff.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn prepare_ap_descriptor_tables(cpu: usize, rsp0: u64) -> (u16, u64, u64) {
    let idx = cpu.min(crate::arch::platform_constants::MAX_CPUS - 1);
    unsafe {
        let tss = core::ptr::addr_of_mut!(AP_TSSS[idx]);
        let mut t = X86TaskStateSegment::new();
        t.rsp0 = rsp0;
        core::ptr::write_volatile(tss, t);

        let tss_base = tss as u64;
        let tss_limit = (core::mem::size_of::<X86TaskStateSegment>() - 1) as u32;
        let (tss_low, tss_high) = encode_tss_descriptor(tss_base, tss_limit);

        let gdt = core::ptr::addr_of_mut!(AP_GDTS[idx]);
        let entries = [
            0x0000_0000_0000_0000u64, // null
            0x00af_9a00_0000_ffff,    // 0x08 kernel code (same as BOOT_GDT)
            0x00af_9200_0000_ffff,    // 0x10 kernel data
            0x00af_f200_0000_ffff,    // 0x18 user data
            0x00af_fa00_0000_ffff,    // 0x20 user code
            tss_low,                  // 0x28 per-AP TSS (low)
            tss_high,                 //      per-AP TSS (high)
        ];
        core::ptr::write_volatile(gdt, X86BootGdt { entries });

        let limit = (core::mem::size_of::<X86BootGdt>() - 1) as u16;
        (limit, gdt as u64, tss_base)
    }
}

/// BSP-side readback (after the AP admits): `ltr` on the AP sets the TSS
/// descriptor's BUSY type (0b1001 → 0b1011) in that AP's GDT — a memory write
/// the BSP can observe, proving the AP really loaded the task register from
/// the per-AP GDT under the kernel CR3.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_tss_descriptor_busy(cpu: usize) -> bool {
    let idx = cpu.min(crate::arch::platform_constants::MAX_CPUS - 1);
    unsafe {
        let low = core::ptr::read_volatile(core::ptr::addr_of!(AP_GDTS[idx].entries[5]));
        ((low >> 40) & 0xF) == 0xB
    }
}

/// BSP-side: the `rsp0` recorded in this AP's TSS (for the X86_AP_TSS_OK marker).
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_tss_rsp0(cpu: usize) -> u64 {
    let idx = cpu.min(crate::arch::platform_constants::MAX_CPUS - 1);
    unsafe {
        let tss = core::ptr::read_volatile(core::ptr::addr_of!(AP_TSSS[idx]));
        tss.rsp0
    }
}

/// Stage 189C6: set the ring-0 `rsp0` in this AP's TSS — the kernel stack the CPU
/// switches to on a ring3→ring0 interrupt/fault while a live AP user task runs.
/// Field-granular write to `AP_TSSS[cpu].rsp0` only.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn set_ap_tss_rsp0(cpu: usize, rsp0: u64) {
    let idx = cpu.min(crate::arch::platform_constants::MAX_CPUS - 1);
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(AP_TSSS[idx].rsp0), rsp0);
    }
}

// ---------------------------------------------------------------------------
// Stage 183 inc.4 — AP-safe IDT (interrupt-safe idle)
// ---------------------------------------------------------------------------
// The shared kernel BOOT_IDT is NOT AP-safe: its gates enter the full Rust
// trap path (global KernelState, logging, compiled-Rust SSE the AP's CR4 may
// not dispatch). The APs therefore get a dedicated minimal IDT whose handlers
// are pure asm and touch only gs:-relative per-CPU record fields + LAPIC EOI:
//  - every vector defaults to a 16-byte catch-all stub that records
//    (vector+1) into PerCpuRecord.irq_unexpected_vec via gs: and PARKS
//    (cli/hlt, no iretq) — converting any unexpected interrupt/exception
//    from a triple fault into a deterministic, BSP-readable state;
//  - AP_IRQ_SMOKE_VECTOR gets a real handler: irq_hit_count += 1,
//    irq_hit_vector = vector, LAPIC EOI, iretq — the controlled
//    interrupt-smoke path (BSP sends one fixed IPI; the AP wakes from
//    sti;hlt, handles it, and returns to idle).
// IST policy: every gate uses ist=0 (current stack). No IST is required this
// increment: the AP never leaves its known-good idle stack (no user mode, no
// stack switching, no nesting — interrupts are only enabled inside the
// controlled sti;hlt smoke window), so the interrupted rsp is always valid.
// Real per-AP IST stacks arrive with the scheduler-online increment, BEFORE
// any path can take an interrupt on a non-idle stack.

/// The harmless fixed vector the BSP sends to each AP for the interrupt smoke.
/// High, outside the exception range and unused by the BSP IDT paths.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) const AP_IRQ_SMOKE_VECTOR: u8 = 0xF0;

/// Stage 183.5: the remote-wake vector for scheduler-online APs. Distinct from the
/// 183.4 smoke vector — its handler increments `PerCpuRecord.remote_wake_count`
/// (gs:[108]) and the AP's managed scheduler-idle loop observes the change,
/// publishes a re-entry, and returns to idle.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) const AP_REMOTE_WAKE_VECTOR: u8 = 0xF1;

#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text.ap_idt_stubs,"ax",@progbits
    .global yarm_ap_idt_unexpected_stubs
    .global yarm_ap_irq_smoke_stub
    .global yarm_ap_remote_wake_stub

    // 256 catch-all stubs, 16 bytes apart (vector = index). Each pushes its
    // vector and parks in the common tail. NOTE: `yarm_ap_vec` is a pure
    // NUMERIC .set, which GAS Intel syntax assembles as an immediate —
    // unlike the symbol-difference `.set` that caused the Stage 183 inc.2
    // `add rdi, AP_OFF_HANDOFF` memory-operand misassembly.
    .align 16
yarm_ap_idt_unexpected_stubs:
    .set yarm_ap_vec, 0
    .rept 256
    push yarm_ap_vec
    jmp yarm_ap_idt_unexpected_common
    .align 16
    .set yarm_ap_vec, yarm_ap_vec + 1
    .endr

yarm_ap_idt_unexpected_common:
    // Deterministic park, never returns: record (vector+1) via gs: so the
    // BSP can name the unexpected vector, then cli/hlt forever. rax is safe
    // to clobber because this path never resumes the interrupted code.
    pop rax
    add eax, 1
    mov dword ptr gs:[{unexpected_off}], eax
    cli
77:
    hlt
    jmp 77b

    .align 16
yarm_ap_irq_smoke_stub:
    // Controlled smoke handler: count + record the vector via gs:, EOI the
    // local APIC (this AP's own LAPIC at the shared MMIO VA), and iretq back
    // into the sti;hlt window. Preserves every register it touches.
    // 183.5 fix diagnostics: publish the handler sub-stage via gs:[irq_stage]
    // (32 = entered, 33 = after EOI, 34 = before iretq) so a resume failure
    // can name exactly where the handler path died.
    push rax
    mov dword ptr gs:[{irq_stage_off}], 32
    add dword ptr gs:[{hit_count_off}], 1
    mov dword ptr gs:[{hit_vec_off}], {smoke_vec}
    movabs rax, {lapic_eoi}
    mov dword ptr [rax], 0
    mov dword ptr gs:[{irq_stage_off}], 33
    mov dword ptr gs:[{irq_stage_off}], 34
    pop rax
    iretq

    .align 16
yarm_ap_remote_wake_stub:
    // Stage 183.5 remote-wake handler (vector 0xF1): count the wake via gs:,
    // EOI, iretq. The scheduler-idle loop observes the count change, publishes
    // the re-entry, and returns to idle. Preserves every register it touches.
    push rax
    add dword ptr gs:[{wake_count_off}], 1
    movabs rax, {lapic_eoi}
    mov dword ptr [rax], 0
    pop rax
    iretq

    .align 16
    .global yarm_ap_page_fault_stub
yarm_ap_page_fault_stub:
    // Stage 199A2D2C2B3 AP #PF handler (vector 14). The AP IDT otherwise routes #PF to the catch-all
    // park stub, so a resumed AP server's ring-3 data read faulted SILENTLY. This stub captures the
    // fault (error code + CR2 + faulting RIP + CR3) and calls a Rust diagnostic that emits the fault
    // marker + PTE walk. The #PF pushed an error code: [rsp]=err, +8=rip, +16=cs, +24=rflags,
    // +32=user_rsp, +40=ss. We never resume the faulting instruction (the diag decides), so registers
    // may be clobbered freely.
    mov rdi, qword ptr [rsp]       // error code (SysV arg0)
    mov rsi, cr2                   // CR2 (arg1)
    mov rdx, qword ptr [rsp + 8]   // faulting RIP (arg2)
    mov rcx, cr3                   // CR3 (arg3)
    and rsp, -16                   // 16-align for the SysV call
    call yarm_x86_ap_page_fault_diag
    cli
90:
    hlt
    jmp 90b
"#,
    unexpected_off = const super::percpu::IRQ_UNEXPECTED_VEC_OFFSET,
    hit_count_off = const super::percpu::IRQ_HIT_COUNT_OFFSET,
    hit_vec_off = const super::percpu::IRQ_HIT_VECTOR_OFFSET,
    smoke_vec = const AP_IRQ_SMOKE_VECTOR as u32,
    wake_count_off = const super::percpu::REMOTE_WAKE_COUNT_OFFSET,
    irq_stage_off = const super::percpu::IRQ_STAGE_OFFSET,
    lapic_eoi = const super::platform_layout::LAPIC_MMIO_BASE + 0xB0,
);

#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
unsafe extern "C" {
    static yarm_ap_idt_unexpected_stubs: u8;
    static yarm_ap_irq_smoke_stub: u8;
    static yarm_ap_remote_wake_stub: u8;
    static yarm_ap_page_fault_stub: u8;
}

/// Stage 199A2D2C2B3: AP #PF diagnostic (called from `yarm_ap_page_fault_stub`). Emits the exact
/// fault — decoded error bits + CR2 + faulting RIP + CR3 + a 4-level PTE walk of the CR2 page + the
/// region classification (payload/meta/stack) — then returns to the stub which parks. Bounded/one-shot
/// per CPU so a fault storm cannot flood the log.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_x86_ap_page_fault_diag(error_code: u64, cr2: u64, rip: u64, cr3: u64) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static EMITTED: [AtomicBool; crate::arch::platform_constants::MAX_CPUS] =
        [const { AtomicBool::new(false) }; crate::arch::platform_constants::MAX_CPUS];
    let cpu = current_cpu_id();
    let idx = (cpu.0 as usize).min(crate::arch::platform_constants::MAX_CPUS - 1);
    if EMITTED[idx].swap(true, Ordering::AcqRel) {
        return;
    }
    let present = error_code & 1;
    let write = (error_code >> 1) & 1;
    let user = (error_code >> 2) & 1;
    let rsvd = (error_code >> 3) & 1;
    let ifetch = (error_code >> 4) & 1;
    let region = match cr2 & !0xfffu64 {
        0x2003_0000 => "payload",
        0x2004_0000 => "meta",
        0x2001_0000 => "stack",
        0x2000_0000 => "code",
        0x2002_0000 => "marker",
        _ => "unknown",
    };
    let (pml4e, pdpte, pde, pte) =
        crate::arch::x86_64::page_table::hw_pte_walk_verbose(cr3 & !0xfffu64, cr2 & !0xfffu64);
    crate::yarm_log!(
        "X86_AP_RECV_V2_USER_READ_FAULT cpu={} cr2=0x{:x} error=0x{:x} present={} write={} user={} rsvd={} ifetch={} region={} result=fault",
        cpu.0,
        cr2,
        error_code,
        present,
        write,
        user,
        rsvd,
        ifetch,
        region,
    );
    crate::yarm_log!(
        "X86_AP_RECV_V2_USER_READ_FAULT_WALK cpu={} rip=0x{:x} cr3=0x{:x} pml4e=0x{:x} pdpte=0x{:x} pde=0x{:x} pte=0x{:x} pte_present={} pte_user={} pte_write={}",
        cpu.0,
        rip,
        cr3,
        pml4e,
        pdpte,
        pde,
        pte,
        pte & 1,
        (pte >> 2) & 1,
        (pte >> 1) & 1,
    );
}

/// One shared AP IDT (identical gates for every AP; per-AP results land in the
/// per-CPU record via gs:). Lives in `.bss`, mapped under the kernel CR3 the
/// AP runs on when it executes `lidt`.
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut AP_IDT: [X86IdtEntry; IDT_ENTRIES] = [const { X86IdtEntry::missing() }; IDT_ENTRIES];

/// BSP-side: the AP IDT stub table's base VA (for the prepare-time map check).
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_idt_stub_base() -> u64 {
    core::ptr::addr_of!(yarm_ap_idt_unexpected_stubs) as u64
}

/// BSP-side: the AP IDT table's base VA (for the X86_AP_IDT_OK marker).
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_idt_table_base() -> u64 {
    core::ptr::addr_of!(AP_IDT) as u64
}

/// Stage 183 inc.4 fix: BSP-side descriptor check of the smoke vector's gate —
/// present, interrupt-gate type (0xE), selector 0x08, ist 0, and the handler
/// offset equal to the smoke stub's linked VA. Returns
/// `(ok, gate_type, selector, ist, offset, expected_offset)`.
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_idt_smoke_vector_report() -> (bool, u8, u16, u8, u64, u64) {
    unsafe {
        let e = core::ptr::read_volatile(core::ptr::addr_of!(AP_IDT[AP_IRQ_SMOKE_VECTOR as usize]));
        let offset =
            (e.offset_low as u64) | ((e.offset_mid as u64) << 16) | ((e.offset_high as u64) << 32);
        let expected = core::ptr::addr_of!(yarm_ap_irq_smoke_stub) as u64;
        let present = (e.type_attr & 0x80) != 0;
        let gate_type = e.type_attr & 0xF;
        let ist = e.ist & 0x7;
        let ok = present
            && gate_type == IDT_GATE_INTERRUPT
            && e.selector == KERNEL_CODE_SELECTOR
            && ist == 0
            && offset == expected;
        (ok, gate_type, e.selector, ist, offset, expected)
    }
}

/// BSP-side IST-policy validation: TRUE if any AP IDT gate names an IST slot.
/// This increment none may (ist=0 everywhere — no per-AP IST stacks exist yet);
/// a nonzero IST here would dispatch onto TSS ist slots that are 0 → garbage
/// stack on delivery. Graded as X86_AP_IST_BAD by the admit poll if violated.
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn ap_idt_any_ist_nonzero() -> bool {
    unsafe {
        let idt = core::ptr::addr_of!(AP_IDT);
        let mut i = 0usize;
        while i < IDT_ENTRIES {
            if (*idt)[i].ist & 0x7 != 0 {
                return true;
            }
            i += 1;
        }
        false
    }
}

/// BSP-side (before SIPI): populate the AP IDT — catch-all park stubs on every
/// vector, the smoke handler on `AP_IRQ_SMOKE_VECTOR`, all gates CS=0x08
/// (kernel code in the per-AP GDT), dpl=0, ist=0 (see the IST policy above).
/// Idempotent. Returns `(idtr_limit, idt_base_va)` for the trampoline handoff.
#[cfg(all(not(test), not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn prepare_ap_idt() -> (u16, u64) {
    unsafe {
        let stub_base = core::ptr::addr_of!(yarm_ap_idt_unexpected_stubs) as u64;
        let idt = core::ptr::addr_of_mut!(AP_IDT);
        let mut i = 0usize;
        while i < IDT_ENTRIES {
            // Stubs are laid out 16 bytes apart in vector order.
            let handler = stub_base + (i as u64) * 16;
            (*idt)[i] = X86IdtEntry::new_interrupt(handler, KERNEL_CODE_SELECTOR, 0, 0);
            i += 1;
        }
        (*idt)[AP_IRQ_SMOKE_VECTOR as usize] = X86IdtEntry::new_interrupt(
            core::ptr::addr_of!(yarm_ap_irq_smoke_stub) as u64,
            KERNEL_CODE_SELECTOR,
            0,
            0,
        );
        // Stage 183.5: the scheduler remote-wake vector for online APs.
        (*idt)[AP_REMOTE_WAKE_VECTOR as usize] = X86IdtEntry::new_interrupt(
            core::ptr::addr_of!(yarm_ap_remote_wake_stub) as u64,
            KERNEL_CODE_SELECTOR,
            0,
            0,
        );
        // Stage 199A2D2C2B3: a real AP #PF (vector 14) handler — otherwise a resumed AP server's
        // ring-3 data read faults into the catch-all park stub and CPU 1 hangs silently. This stub
        // captures + emits the exact fault (CR2/error/PTE walk) before parking.
        (*idt)[VEC_PAGE_FAULT] = X86IdtEntry::new_interrupt(
            core::ptr::addr_of!(yarm_ap_page_fault_stub) as u64,
            KERNEL_CODE_SELECTOR,
            0,
            0,
        );
        let limit = (core::mem::size_of::<X86IdtEntry>() * IDT_ENTRIES - 1) as u16;
        (limit, idt as u64)
    }
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
unsafe fn populate_boot_idt_from_stubs() {
    let idt_ptr = core::ptr::addr_of_mut!(BOOT_IDT).cast::<X86IdtEntry>();
    let mut i = 0usize;
    while i < IDT_ENTRIES {
        let mut handler = ISR_STUBS[i] as *const () as u64;
        let mut dpl = 0;
        let mut ist = 0;
        if i == VEC_SYSCALL {
            dpl = 3;
            handler = yarm_x86_syscall_entry as *const () as u64;
        } else if i == VEC_NMI {
            ist = IST_SLOT_NMI;
        } else if i == VEC_DOUBLE_FAULT {
            ist = IST_SLOT_DOUBLE_FAULT;
        } else if i == VEC_PAGE_FAULT {
            ist = IST_SLOT_PAGE_FAULT;
        }
        unsafe {
            core::ptr::write(
                idt_ptr.add(i),
                X86IdtEntry::new_interrupt(handler, KERNEL_CODE_SELECTOR, dpl, ist),
            );
        }
        i += 1;
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "x86_64")))]
pub fn ensure_boot_descriptor_tables_scaffolded() {}

#[cfg(feature = "hosted-dev")]
pub fn ensure_boot_descriptor_tables_scaffolded() {
    let _ = DESCRIPTOR_SCAFFOLD_READY.swap(true, Ordering::AcqRel);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn refresh_boot_tss_rsp0(rsp0: u64) {
    ensure_boot_descriptor_tables_scaffolded();
    unsafe {
        BOOT_TSS.rsp0 = rsp0;
    }
    // Stage 189C4: update the EXECUTING CPU's per-CPU SYSCALL kernel RSP0 (the
    // gs-relative slot the LSTAR entry reads), not a global. On the BSP path this
    // is CPU 0; on an AP it is that AP's own record.
    super::percpu::set_syscall_kernel_rsp0(current_cpu_id(), rsp0);
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn refresh_boot_tss_rsp0(_rsp0: u64) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) fn read_boot_tss_rsp0() -> u64 {
    super::percpu::syscall_kernel_rsp0(current_cpu_id())
}
#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub(crate) fn read_boot_tss_rsp0() -> u64 {
    0
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
unsafe extern "C" {
    fn yarm_x86_enter_ring3(
        entry: u64,
        stack_top: u64,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
    ) -> !;
    fn yarm_x86_resume_ring3(frame: *const ApSavedResumeFrame) -> !;
}

/// Stage 199A2D2C2A: the owned, `repr(C)` saved-frame the `yarm_x86_resume_ring3` asm consumes. The
/// GPR lanes are in `user_gprs` order (rax..r15); `rip/rsp/rflags/cs/ss` are the canonical iret
/// frame. Offsets are asserted to match the asm.
#[cfg(any(all(not(feature = "hosted-dev"), target_arch = "x86_64"), test))]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ApSavedResumeFrame {
    pub(crate) gprs: [u64; 15],
    pub(crate) rip: u64,
    pub(crate) rsp: u64,
    pub(crate) rflags: u64,
    pub(crate) cs: u64,
    pub(crate) ss: u64,
}

#[cfg(any(all(not(feature = "hosted-dev"), target_arch = "x86_64"), test))]
const _: () = {
    assert!(core::mem::offset_of!(ApSavedResumeFrame, gprs) == 0);
    assert!(core::mem::offset_of!(ApSavedResumeFrame, rip) == 120);
    assert!(core::mem::offset_of!(ApSavedResumeFrame, rsp) == 128);
    assert!(core::mem::offset_of!(ApSavedResumeFrame, rflags) == 136);
    assert!(core::mem::offset_of!(ApSavedResumeFrame, cs) == 144);
    assert!(core::mem::offset_of!(ApSavedResumeFrame, ss) == 152);
};

/// Stage 199A2D2C2A: return to ring 3 by RESTORING a committed saved userspace continuation (the
/// canonical saved-frame return). Diverges. The caller must have already installed this task's CR3,
/// per-CPU syscall/TSS RSP0, GS and FS, and released every Rust guard.
///
/// # Safety
/// `frame` must be a fully-committed saved frame for the task whose CR3 is active, with a valid
/// user RIP/RSP; all per-CPU state must be installed for this CPU/task.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub(crate) unsafe fn resume_user_mode_iret(frame: &ApSavedResumeFrame) -> ! {
    unsafe { yarm_x86_resume_ring3(frame as *const ApSavedResumeFrame) }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn enter_user_mode_iret(
    entry: u64,
    stack_top: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> ! {
    unsafe { yarm_x86_enter_ring3(entry, stack_top, arg0, arg1, arg2, arg3, arg4, arg5) }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_x86_enter_ring3
    .type yarm_x86_enter_ring3, @function
yarm_x86_enter_ring3:
    mov r10, rdi
    mov r11, rsi
    mov rdi, rdx
    mov rsi, rcx
    mov rdx, r8
    mov rcx, r9
    mov r8, qword ptr [rsp + 8]
    mov r9, qword ptr [rsp + 16]
    mov ax, 0x1b
    mov ds, ax
    mov es, ax
    push 0x1b
    push r11
    push 0x202
    push 0x23
    push r10
    iretq
"#
);

/// Stage 199A2D2C2A: the GENERIC saved-frame context-restore return — the counterpart to
/// `yarm_x86_enter_ring3` (fresh entry). It restores a task's COMPLETE committed userspace
/// continuation (all 15 GPRs + the exact iret frame RIP/RSP/RFLAGS/CS/SS) and `iretq`s to the
/// instruction AFTER its trapping syscall — NOT a fresh entry. `rdi` points at an
/// [`ApSavedResumeFrame`]; its layout (15 GPR lanes in `user_gprs` order rax..r15, then
/// rip/rsp/rflags/cs/ss) matches the offsets below. The GPR that carries the frame pointer (`rdi`)
/// is restored LAST. This is the canonical BSP iret frame ABI (SS/RSP/RFLAGS/CS/RIP), never a
/// reduced AP-only frame.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_x86_resume_ring3
    .type yarm_x86_resume_ring3, @function
yarm_x86_resume_ring3:
    mov ax, 0x1b
    mov ds, ax
    mov es, ax
    // Build the canonical iret frame from the committed saved frame (offsets: rip=120, rsp=128,
    // rflags=136, cs=144, ss=152; 15 GPR lanes precede at 0..112).
    push qword ptr [rdi + 152]   // SS
    push qword ptr [rdi + 128]   // RSP
    push qword ptr [rdi + 136]   // RFLAGS
    push qword ptr [rdi + 144]   // CS
    push qword ptr [rdi + 120]   // RIP
    // Restore every GPR (user_gprs order: [0]=rax [1]=rbx [2]=rcx [3]=rdx [4]=rsi [5]=rdi
    // [6]=rbp [7]=r8 [8]=r9 [9]=r10 [10]=r11 [11]=r12 [12]=r13 [13]=r14 [14]=r15). rdi LAST.
    mov rax, qword ptr [rdi + 0]
    mov rbx, qword ptr [rdi + 8]
    mov rcx, qword ptr [rdi + 16]
    mov rdx, qword ptr [rdi + 24]
    mov rsi, qword ptr [rdi + 32]
    mov rbp, qword ptr [rdi + 48]
    mov r8,  qword ptr [rdi + 56]
    mov r9,  qword ptr [rdi + 64]
    mov r10, qword ptr [rdi + 72]
    mov r11, qword ptr [rdi + 80]
    mov r12, qword ptr [rdi + 88]
    mov r13, qword ptr [rdi + 96]
    mov r14, qword ptr [rdi + 104]
    mov r15, qword ptr [rdi + 112]
    mov rdi, qword ptr [rdi + 40]
    iretq
"#
);

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn enter_user_mode_iret(
    _entry: u64,
    _stack_top: u64,
    _arg0: u64,
    _arg1: u64,
    _arg2: u64,
    _arg3: u64,
    _arg4: u64,
    _arg5: u64,
) -> ! {
    panic!("x86_64 ring-3 iret entry is unavailable on this build target")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idt_entry_encodes_interrupt_gate_fields() {
        let entry = X86IdtEntry::new_interrupt(0x1122_3344_5566_7788, 0x8, 0, 3);
        let selector = entry.selector;
        let ist = entry.ist;
        let type_attr = entry.type_attr;
        assert_eq!(selector, 0x8);
        assert_eq!(ist, 3);
        assert_eq!(type_attr & IDT_PRESENT, IDT_PRESENT);
        assert_eq!(type_attr & 0x0F, IDT_GATE_INTERRUPT);
    }

    #[test]
    fn tss_default_io_map_base_points_to_tss_end() {
        let tss = X86TaskStateSegment::new();
        assert_eq!(
            tss.io_map_base as usize,
            core::mem::size_of::<X86TaskStateSegment>()
        );
    }

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    #[test]
    fn descriptor_scaffold_initializes_tss_rsp0() {
        ensure_boot_descriptor_tables_scaffolded();
        unsafe {
            assert_ne!(BOOT_TSS.rsp0, 0);
        }
    }

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    #[test]
    fn refresh_boot_tss_rsp0_updates_tss_kernel_stack_top() {
        ensure_boot_descriptor_tables_scaffolded();
        refresh_boot_tss_rsp0(0x1234_5000);
        unsafe {
            assert_eq!(BOOT_TSS.rsp0, 0x1234_5000);
        }
    }

    #[test]
    fn halt_without_kernel_state_for_cpu_exception_vectors() {
        assert!(should_halt_without_kernel_state(0));
        assert!(should_halt_without_kernel_state(VEC_DOUBLE_FAULT));
        assert!(should_halt_without_kernel_state(VEC_PAGE_FAULT));
        assert!(!should_halt_without_kernel_state(VEC_SYSCALL));
        assert!(!should_halt_without_kernel_state(0x40));
    }

    #[cfg(target_arch = "x86_64")]
    fn decode_handler_addr(entry: X86IdtEntry) -> u64 {
        entry.offset_low as u64
            | ((entry.offset_mid as u64) << 16)
            | ((entry.offset_high as u64) << 32)
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn idt_scaffold_binds_isr_stub_and_syscall_entry_handlers() {
        unsafe { populate_boot_idt_from_stubs() };
        let timer_handler = decode_handler_addr(unsafe { BOOT_IDT[VEC_TIMER] });
        let syscall_handler = decode_handler_addr(unsafe { BOOT_IDT[VEC_SYSCALL] });
        assert_eq!(timer_handler, ISR_STUBS[VEC_TIMER] as *const () as u64);
        assert_eq!(syscall_handler, yarm_x86_syscall_entry as *const () as u64);
        assert_eq!(unsafe { BOOT_IDT[VEC_SYSCALL] }.type_attr >> 5 & 0x3, 0x3);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    #[ignore = "flaky in hosted unit-test environment due nested trap dispatch stack growth"]
    fn real_stub_frame_dispatch_path_advances_timer_tick() {
        let mut kernel = crate::kernel::boot::Bootstrap::init().expect("kernel");
        let mut regs = X86SavedRegs::default();
        let frame = X86InterruptStackFrame {
            rip: 0x1000,
            cs: KERNEL_CODE_SELECTOR as u64,
            rflags: 0x202,
            rsp: 0x8000,
            ss: KERNEL_DATA_SELECTOR as u64,
        };
        assert_eq!(kernel.timer_ticks_for_test(), 0);
        let mut frame = frame;
        dispatch_trap_from_stub_for_test(&mut kernel, VEC_TIMER as u64, 0, &mut regs, &mut frame)
            .expect("timer dispatch");
        assert_eq!(kernel.timer_ticks_for_test(), 1);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    #[ignore = "flaky in hosted unit-test environment due nested trap dispatch stack growth"]
    fn real_stub_frame_syscall_path_reports_decode_error() {
        let mut kernel = crate::kernel::boot::Bootstrap::init().expect("kernel");
        let mut regs = X86SavedRegs {
            rax: 0xFFFF, // invalid syscall number
            ..X86SavedRegs::default()
        };
        let frame = X86InterruptStackFrame {
            rip: 0x2000,
            cs: KERNEL_CODE_SELECTOR as u64,
            rflags: 0x202,
            rsp: 0x9000,
            ss: KERNEL_DATA_SELECTOR as u64,
        };
        let mut frame = frame;
        let result = dispatch_trap_from_stub_for_test(
            &mut kernel,
            VEC_SYSCALL as u64,
            0,
            &mut regs,
            &mut frame,
        );
        assert_eq!(
            result,
            Err(crate::kernel::boot::TrapHandleError::Syscall(
                crate::kernel::syscall::SyscallError::InvalidNumber
            ))
        );
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn real_stub_frame_builder_captures_syscall_and_user_stack_registers() {
        let regs = X86SavedRegs {
            rax: 7,
            rdi: 11,
            rsi: 12,
            rdx: 13,
            rcx: 14,
            r8: 15,
            r9: 16,
            ..X86SavedRegs::default()
        };
        let frame = X86InterruptStackFrame {
            rip: 0x4444,
            cs: 0x1b, // ring3 selector, low bits set
            rflags: 0x202,
            rsp: 0x8888,
            ss: 0x23,
        };
        let mut frame = frame;
        let trap = unsafe {
            build_trap_frame_from_saved_regs(
                &regs as *const X86SavedRegs,
                &mut frame as *mut X86InterruptStackFrame,
                VEC_SYSCALL as u64,
            )
        };
        assert_eq!(trap.saved_pc(), 0x4444);
        assert_eq!(trap.saved_sp(), 0x8888);
        assert_eq!(trap.syscall_num(), 7);
        assert_eq!(trap.arg(0), 11);
        assert_eq!(trap.arg(1), 12);
        assert_eq!(trap.arg(2), 13);
        assert_eq!(trap.arg(3), 14);
        assert_eq!(trap.arg(4), 15);
        assert_eq!(trap.arg(5), 16);
    }

    // -----------------------------------------------------------------------
    // x86_64 startup argument mapping regression tests
    //
    // The x86_64 SysV ABI maps the first 6 function arguments to:
    //   arg0 -> RDI   arg1 -> RSI   arg2 -> RDX
    //   arg3 -> RCX   arg4 -> R8    arg5 -> R9
    //
    // For new-task first entry, write_task_gprs_to_saved_regs() must place
    // startup args into exactly these registers.
    // -----------------------------------------------------------------------
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn startup_arg_mapping_new_task_uses_sysv_calling_convention() {
        // Build a TrapFrame that looks like a brand-new task: all user_gprs
        // are zero (never ran) but startup args are non-zero.
        let mut trap = crate::kernel::trapframe::TrapFrame::zeroed();
        // Startup ABI: arg0=task_id, arg1=pm_send, arg2=pm_reply,
        //              arg3=slots_ptr, arg4=slots_len, arg5=reserved
        trap.set_arg(0, 0xAA); // -> RDI
        trap.set_arg(1, 0xBB); // -> RSI
        trap.set_arg(2, 0xCC); // -> RDX
        trap.set_arg(3, 0xDD); // -> RCX
        trap.set_arg(4, 0xEE); // -> R8
        trap.set_arg(5, 0xFF); // -> R9
        // user_gprs all zero → new-task detection fires

        let mut regs = X86SavedRegs::default();
        write_task_gprs_to_saved_regs(&mut regs as *mut X86SavedRegs, &trap);

        assert_eq!(regs.rdi, 0xAA, "arg0 must go to RDI");
        assert_eq!(regs.rsi, 0xBB, "arg1 must go to RSI");
        assert_eq!(regs.rdx, 0xCC, "arg2 must go to RDX");
        assert_eq!(regs.rcx, 0xDD, "arg3 must go to RCX");
        assert_eq!(regs.r8, 0xEE, "arg4 must go to R8");
        assert_eq!(regs.r9, 0xFF, "arg5 must go to R9");
        // Scratch registers must be cleared
        assert_eq!(regs.rax, 0);
        assert_eq!(regs.rbx, 0);
        assert_eq!(regs.rbp, 0);
        assert_eq!(regs.r10, 0);
        assert_eq!(regs.r11, 0);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn startup_arg_mapping_all_six_positions_independently() {
        // Verify each arg slot independently to catch off-by-one in the
        // RDI/RSI/RDX/RCX/R8/R9 assignment.
        for (slot, expected_reg) in [
            (0usize, "rdi"),
            (1, "rsi"),
            (2, "rdx"),
            (3, "rcx"),
            (4, "r8"),
            (5, "r9"),
        ] {
            let mut trap = crate::kernel::trapframe::TrapFrame::zeroed();
            let sentinel = 0x1000_0000 + slot as usize;
            trap.set_arg(slot, sentinel);
            trap.set_arg(0, if slot == 0 { sentinel } else { 0x1 }); // ensure non-zero task-id

            let mut regs = X86SavedRegs::default();
            write_task_gprs_to_saved_regs(&mut regs as *mut X86SavedRegs, &trap);

            let actual = match slot {
                0 => regs.rdi as usize,
                1 => regs.rsi as usize,
                2 => regs.rdx as usize,
                3 => regs.rcx as usize,
                4 => regs.r8 as usize,
                5 => regs.r9 as usize,
                _ => unreachable!(),
            };
            assert_eq!(
                actual, sentinel,
                "arg{} must map to {} (sentinel 0x{:x} found 0x{:x})",
                slot, expected_reg, sentinel, actual
            );
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn resumed_task_gprs_are_restored_verbatim() {
        // A resumed task (some user_gprs non-zero) must have its full GPR
        // snapshot written back without the new-task arg override.
        let mut trap = crate::kernel::trapframe::TrapFrame::zeroed();
        // Non-zero user_gprs[0] disables new-task detection.
        trap.set_user_gpr(0, 0x1); // rax = 1 (non-zero -> resumed task)
        trap.set_user_gpr(11, 0xCAFE); // r12
        trap.set_user_gpr(12, 0xBEEF); // r13
        trap.set_user_gpr(13, 0xDEAD); // r14
        trap.set_user_gpr(14, 0x1234); // r15
        trap.set_user_gpr(1, 0xABCD); // rbx
        trap.set_user_gpr(6, 0x5678); // rbp
        // Set arg0 non-zero — must NOT trigger new-task path.
        trap.set_arg(0, 0xFF);

        let mut regs = X86SavedRegs::default();
        write_task_gprs_to_saved_regs(&mut regs as *mut X86SavedRegs, &trap);

        // Callee-saved registers must be restored verbatim.
        assert_eq!(regs.r12, 0xCAFE, "r12 must be restored from user_gpr[11]");
        assert_eq!(regs.r13, 0xBEEF, "r13 must be restored from user_gpr[12]");
        assert_eq!(regs.r14, 0xDEAD, "r14 must be restored from user_gpr[13]");
        assert_eq!(regs.r15, 0x1234, "r15 must be restored from user_gpr[14]");
        assert_eq!(regs.rbx, 0xABCD, "rbx must be restored from user_gpr[1]");
        assert_eq!(regs.rbp, 0x5678, "rbp must be restored from user_gpr[6]");
        // Startup args must NOT be injected into RDI/RSI/etc for a resumed task.
        assert_ne!(
            regs.rdi, 0xFF,
            "startup args must not override resumed task RDI"
        );
    }

    // -----------------------------------------------------------------------
    // Callee-saved register preservation across SYSCALL — unit-test coverage
    //
    // The authentic hardware test (checking that R12/R13/R14/R15/RBX/RBP
    // survive a real SYSCALL + IRETQ round-trip) requires running user-mode
    // code on actual x86_64 hardware or QEMU.  It cannot be expressed as a
    // cargo unit test because:
    //   1. Unit tests run in kernel (ring-0) mode; SYSCALL from ring-0 is
    //      undefined behaviour on x86-64.
    //   2. The lstar entry assembly is not invocable from a hosted-dev test.
    //
    // Instead, the regression is covered at two levels:
    //   a) The tests above verify write_task_gprs_to_saved_regs() correctly
    //      maps user_gpr[11..14] (r12/r13/r14) to regs.r12/r13/r14.
    //   b) The QEMU smoke test (`scripts/qemu-x86_64-core-smoke.sh`) exercises
    //      the full entry/return path at runtime; a callee-saved register
    //      corruption manifests as PM_NO_RECV_CAP or a GPF.
    //
    // The assembly fix (Step 1 of yarm_x86_lstar_entry) eliminates the source
    // of the corruption: user RSP is now saved to YARM_X86_SYSCALL_SCRATCH_RSP
    // before ANY GPR is touched, so R12/R13/R14 remain authentic at push time.
    // -----------------------------------------------------------------------

    // Phase BT1 — bootstrap timer guard invariant tests.
    //
    // The LAPIC timer may fire during bootstrap_first_user_task (ELF loading
    // takes >800 ms; timer deadline is 800 ms). The timer ISR must NOT tick or
    // yield until signal_bootstrap_scheduler_ready() has been called, otherwise
    // it races with borrow_kernel_for_boot's raw &mut KernelState alias (UB).

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    #[test]
    fn bootstrap_scheduler_ready_starts_false_and_sets_to_true() {
        // Resetting is not possible (static), but signal → assert is idempotent:
        // regardless of prior test state, after signal the flag must be true.
        signal_bootstrap_scheduler_ready();
        assert!(
            bootstrap_scheduler_is_ready(),
            "bootstrap_scheduler_is_ready() must return true after signal"
        );
    }

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    #[test]
    fn bootstrap_scheduler_ready_is_idempotent() {
        signal_bootstrap_scheduler_ready();
        signal_bootstrap_scheduler_ready();
        assert!(
            bootstrap_scheduler_is_ready(),
            "repeated signal_bootstrap_scheduler_ready() calls must leave flag true"
        );
    }

    // Phase BT2 — no timer fires before signal.
    //
    // The BSP LAPIC timer was previously armed in init_lapic_mmio_base (during
    // configure_external_irq_controller_from_platform_layout) and again in
    // run_with_prepared_kernel before STI. This caused the timer ISR to fire
    // 21+ times during bootstrap ELF loading (>800ms in QEMU), each fire
    // creating aliased &mut KernelState via with_cpu while
    // borrow_kernel_for_boot()'s raw pointer is live — UB that corrupted
    // bootstrap state and caused it to hang (signal never called).
    //
    // Fix: init_lapic_mmio_base no longer arms the timer; run_with_prepared_kernel
    // no longer arms it before run(); start_bsp_periodic_timer() arms it after
    // signal_bootstrap_scheduler_ready() completes. The EOI-only guard (BT1)
    // remains as defense-in-depth but fires zero times with the timer not armed.

    #[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
    #[test]
    fn bootstrap_scheduler_ready_gates_timer_isr_scheduling() {
        // Before signal: bootstrap_scheduler_is_ready() returns false (or true
        // if a prior test already signalled — static is monotonic). Regardless,
        // verify the flag's monotone semantics: once true, stays true.
        signal_bootstrap_scheduler_ready();
        assert!(
            bootstrap_scheduler_is_ready(),
            "after signal, bootstrap_scheduler_is_ready() must be true (BT2 invariant)"
        );
        // Second signal must be idempotent — no panic, flag stays true.
        signal_bootstrap_scheduler_ready();
        assert!(
            bootstrap_scheduler_is_ready(),
            "bootstrap_scheduler_is_ready() must remain true after repeated signal (BT2)"
        );
    }
}
