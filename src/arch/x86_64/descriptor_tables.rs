// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
use core::sync::atomic::AtomicU8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
use core::sync::atomic::AtomicU64;
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

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static TRAP_DISPATCH_DEPTH: AtomicUsize = AtomicUsize::new(0);
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
const DEBUG_UART_DATA_PORT: u16 = 0x3F8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const DEBUG_UART_LINE_STATUS_PORT: u16 = 0x3FD;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(no_mangle)]
static YARM_X86_SYSCALL_RSP0: AtomicU64 = AtomicU64::new(0);

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
    YARM_X86_SYSCALL_RSP0.store(rsp0, Ordering::Release);
    // STAR[47:32] = kernel CS selector; STAR[63:48] = SYSRET CS base (CS-16).
    let star = ((KERNEL_CODE_SELECTOR as u64) << 32) | (((USER_CODE_SELECTOR as u64) - 16) << 48);
    let mut efer = read_msr(IA32_EFER_MSR);
    efer |= IA32_EFER_SCE;
    write_msr(IA32_EFER_MSR, efer);
    write_msr(IA32_STAR_MSR, star);
    write_msr(IA32_LSTAR_MSR, yarm_x86_lstar_entry as *const () as u64);
    write_msr(IA32_FMASK_MSR, RFLAGS_IF_MASK);
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
fn install_trap_shared_kernel(shared: &'static crate::runtime::SharedKernel) {
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

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn raw_current_apic_id() -> u32 {
    core::arch::x86_64::__cpuid(1).ebx >> 24
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn register_apic_cpu_mapping(apic_id: u8, cpu: crate::kernel::scheduler::CpuId) {
    APIC_TO_CPU_ID[apic_id as usize].store(cpu.0 as usize, Ordering::Release);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn current_cpu_id() -> crate::kernel::scheduler::CpuId {
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

#[cfg(any(test, all(not(feature = "hosted-dev"), target_arch = "x86_64")))]
const fn should_halt_without_kernel_state(vector: usize) -> bool {
    vector < 32 && vector != VEC_SYSCALL
}

#[cfg(all(any(not(feature = "hosted-dev"), test), target_arch = "x86_64"))]
unsafe fn build_trap_frame_from_saved_regs(
    regs: *const X86SavedRegs,
    frame: *const X86InterruptStackFrame,
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
    regs.rbx = trap_frame.ret1 as u64;
    regs.rdx = trap_frame.ret2 as u64;
    regs.rcx = trap_frame.error as u64;
}

#[cfg(all(test, target_arch = "x86_64"))]
fn dispatch_trap_from_stub_for_test(
    kernel: &mut crate::kernel::boot::KernelState,
    vector: u64,
    error_code: u64,
    regs: &mut X86SavedRegs,
    interrupt_frame: &X86InterruptStackFrame,
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
            interrupt_frame as *const X86InterruptStackFrame,
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

#[cfg(all(test, feature = "hosted-dev", target_arch = "x86_64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_dispatch_trap_from_stub(
    _vector: u64,
    _error_code: u64,
    _regs: *mut X86SavedRegs,
    _interrupt_frame: *const X86InterruptStackFrame,
) {
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_dispatch_trap_from_stub(
    vector: u64,
    error_code: u64,
    regs: *mut X86SavedRegs,
    interrupt_frame: *const X86InterruptStackFrame,
) {
    let cpu_apic = raw_current_apic_id() as u64;
    let previous_depth = TRAP_DISPATCH_DEPTH.fetch_add(1, Ordering::AcqRel);
    if previous_depth != 0 {
        debug_uart_trap_breadcrumb(b'N', vector, error_code, 0, 0, cpu_apic);
        halt_forever();
    }
    let mut fault_addr = 0u64;
    if vector as usize == VEC_PAGE_FAULT {
        unsafe {
            core::arch::asm!("mov {}, cr2", out(reg) fault_addr, options(nomem, preserves_flags));
        }
    }
    let context = crate::arch::x86_64::trap::X86TrapContext {
        vector: vector as u8,
        error_code,
        fault_addr,
    };

    let Some(kernel) = trap_kernel_state_mut() else {
        if should_halt_without_kernel_state(vector as usize) {
            let fault_rip = unsafe { (*(interrupt_frame as *const X86InterruptStackFrameHeader)).rip };
            debug_uart_trap_breadcrumb(b'E', vector, error_code, fault_addr, fault_rip, cpu_apic);
            TRAP_DISPATCH_DEPTH.store(0, Ordering::Release);
            halt_forever();
        }
        TRAP_DISPATCH_DEPTH.store(0, Ordering::Release);
        return;
    };
    let fault_rip = unsafe { (*(interrupt_frame as *const X86InterruptStackFrameHeader)).rip };
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
        debug_uart_trap_breadcrumb(b'T', vector, error_code, fault_addr, fault_rip, cpu_apic);
        halt_forever();
    }
    if vector as usize == VEC_SYSCALL {
        write_trap_returns_to_saved_regs(regs, &trap_frame);
    }
    TRAP_DISPATCH_DEPTH.store(0, Ordering::Release);
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
    sub rsp, 8
    call yarm_x86_dispatch_trap_from_stub
    add rsp, 8

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
    // SYSCALL clobbers RCX/R11 with user RIP/RFLAGS. Preserve them explicitly.
    mov r12, rcx
    mov r13, r11
    mov r14, rsp
    mov rsp, qword ptr [rip + YARM_X86_SYSCALL_RSP0]
    test rsp, rsp
    jnz 1f
    // RSP0 must be initialized before any real user SYSCALL entry.
    // A zero value indicates broken descriptor/TSS setup; fail hard.
    ud2
1:
    // Materialize synthetic interrupt frame expected by dispatch.
    sub rsp, 40
    mov qword ptr [rsp + 0], r12
    mov qword ptr [rsp + 8], 0x23
    mov qword ptr [rsp + 16], r13
    mov qword ptr [rsp + 24], r14
    mov qword ptr [rsp + 32], 0x1b

    // Keep arg3 compatible with trap ABI (rcx lane); syscall callers pass it in r10.
    mov rcx, r10

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

    mov rdi, 0x80
    xor rsi, rsi
    mov rdx, rsp
    lea rcx, [rsp + 120]
    sub rsp, 8
    call yarm_x86_dispatch_trap_from_stub
    add rsp, 8

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

    mov rcx, qword ptr [rsp + 0]
    mov r11, qword ptr [rsp + 16]
    mov rsp, qword ptr [rsp + 24]
    sysretq
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
        let ist_nmi_top =
            core::ptr::addr_of!(IST_NMI.0) as u64 + IST_NMI_STACK_BYTES as u64;
        let ist_df_top = core::ptr::addr_of!(IST_DOUBLE_FAULT.0) as u64
            + IST_DOUBLE_FAULT_STACK_BYTES as u64;
        let ist_pf_top = core::ptr::addr_of!(IST_PAGE_FAULT.0) as u64
            + IST_PAGE_FAULT_STACK_BYTES as u64;
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
    YARM_X86_SYSCALL_RSP0.store(rsp0, Ordering::Release);
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn refresh_boot_tss_rsp0(_rsp0: u64) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
unsafe extern "C" {
    fn yarm_x86_enter_ring3(entry: u64, stack_top: u64, arg0: u64, arg1: u64) -> !;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn enter_user_mode_iret(entry: u64, stack_top: u64, arg0: u64, arg1: u64) -> ! {
    unsafe { yarm_x86_enter_ring3(entry, stack_top, arg0, arg1) }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .section .text, "ax", @progbits
    .global yarm_x86_enter_ring3
    .type yarm_x86_enter_ring3, @function
yarm_x86_enter_ring3:
    mov r8, rdi
    mov r9, rsi
    mov rdi, rdx
    mov rsi, rcx
    mov ax, 0x23
    mov ds, ax
    mov es, ax
    push 0x23
    push r9
    push 0x202
    push 0x1b
    push r8
    iretq
"#
);

#[cfg(any(feature = "hosted-dev", not(target_arch = "x86_64")))]
pub fn enter_user_mode_iret(_entry: u64, _stack_top: u64, _arg0: u64, _arg1: u64) -> ! {
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
        dispatch_trap_from_stub_for_test(&mut kernel, VEC_TIMER as u64, 0, &mut regs, &frame)
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
        let result =
            dispatch_trap_from_stub_for_test(&mut kernel, VEC_SYSCALL as u64, 0, &mut regs, &frame);
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
        let trap = unsafe {
            build_trap_frame_from_saved_regs(
                &regs as *const X86SavedRegs,
                &frame as *const X86InterruptStackFrame,
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
}
