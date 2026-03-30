#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicBool, Ordering};

pub const IDT_ENTRIES: usize = 256;
const IDT_GATE_INTERRUPT: u8 = 0x0E;
const IDT_PRESENT: u8 = 1 << 7;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_NMI: usize = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_DOUBLE_FAULT: usize = 8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_PAGE_FAULT: usize = 14;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
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
    entries: [u64; 5],
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut BOOT_IDT: [X86IdtEntry; IDT_ENTRIES] = [const { X86IdtEntry::missing() }; IDT_ENTRIES];
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut BOOT_TSS: X86TaskStateSegment = X86TaskStateSegment::new();
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut BOOT_GDT: X86BootGdt = X86BootGdt {
    entries: [
        0x0000_0000_0000_0000, // null
        0x00af_9a00_0000_ffff, // kernel code
        0x00af_9200_0000_ffff, // kernel data
        0,
        0,
    ],
};
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(align(16))]
struct IstStack([u8; 4096]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut IST_NMI: IstStack = IstStack([0; 4096]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut IST_DOUBLE_FAULT: IstStack = IstStack([0; 4096]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static mut IST_PAGE_FAULT: IstStack = IstStack([0; 4096]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const KERNEL_CODE_SELECTOR: u16 = 0x08;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const KERNEL_DATA_SELECTOR: u16 = 0x10;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const TSS_SELECTOR: u16 = 0x18;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IST_SLOT_NMI: u8 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IST_SLOT_DOUBLE_FAULT: u8 = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const IST_SLOT_PAGE_FAULT: u8 = 3;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
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

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[repr(C)]
struct X86InterruptStackFrame {
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static TRAP_KERNEL_STATE_PTR: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const UNMAPPED_CPU: usize = usize::MAX;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
static APIC_TO_CPU_ID: [AtomicUsize; 256] = [const { AtomicUsize::new(UNMAPPED_CPU) }; 256];

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
pub fn register_trap_kernel_state(kernel: &mut crate::kernel::boot::KernelState) {
    TRAP_KERNEL_STATE_PTR.store(kernel as *mut _ as usize, Ordering::Release);
    register_apic_cpu_mapping(
        raw_current_apic_id() as u8,
        crate::kernel::scheduler::CpuId(crate::arch::platform_layout::BOOTSTRAP_CPU_ID),
    );
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn raw_current_apic_id() -> u32 {
    unsafe { core::arch::x86_64::__cpuid(1).ebx >> 24 }
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
unsafe fn build_trap_frame_from_saved_regs(
    regs: *const X86SavedRegs,
    frame: *const X86InterruptStackFrame,
) -> crate::kernel::trapframe::TrapFrame {
    let regs = &*regs;
    let frame = &*frame;
    let mut trap = crate::kernel::trapframe::TrapFrame::new(
        regs.rax as usize,
        [
            regs.rdi as usize,
            regs.rsi as usize,
            regs.rdx as usize,
            regs.r10 as usize,
            regs.r8 as usize,
            regs.r9 as usize,
        ],
    );
    trap.set_saved_pc(frame.rip as usize);
    if (frame.cs & 0x3) == 0x3 {
        trap.set_saved_sp(frame.rsp as usize);
    }
    trap
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_x86_dispatch_trap_from_stub(
    vector: u64,
    error_code: u64,
    regs: *mut X86SavedRegs,
    interrupt_frame: *const X86InterruptStackFrame,
) {
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

    let state_ptr = TRAP_KERNEL_STATE_PTR.load(Ordering::Acquire);
    if state_ptr == 0 {
        if vector as usize == VEC_DOUBLE_FAULT {
            loop {
                unsafe {
                    core::arch::asm!("cli", "hlt", options(noreturn));
                }
            }
        }
        return;
    }
    let kernel = unsafe { &mut *(state_ptr as *mut crate::kernel::boot::KernelState) };
    let mut trap_frame = unsafe { build_trap_frame_from_saved_regs(regs, interrupt_frame) };
    let _ = crate::arch::x86_64::trap::handle_trap_entry(
        kernel,
        current_cpu_id(),
        context,
        Some(&mut trap_frame),
    );
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .intel_syntax noprefix
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
    call yarm_x86_dispatch_trap_from_stub

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
macro_rules! declare_all_isr_stubs {
    ($($name:ident),* $(,)?) => {
        unsafe extern "C" {
            $(fn $name();)*
        }
        const ISR_STUBS: [unsafe extern "C" fn(); IDT_ENTRIES] = [$($name),*];
    };
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
core::arch::global_asm!(
    r#"
    .intel_syntax noprefix
    .set vector_index, 0
    .rept 256
      .set has_error, 0
      .if vector_index == 8 || vector_index == 10 || vector_index == 11 || vector_index == 12 || vector_index == 13 || vector_index == 14 || vector_index == 17 || vector_index == 21
        .set has_error, 1
      .endif
      YARM_X86_TRAP_STUB vector_index, has_error
      .set vector_index, vector_index + 1
    .endr
"#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
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
        let idt_ptr = core::ptr::addr_of_mut!(BOOT_IDT).cast::<X86IdtEntry>();
        let mut i = 0usize;
        while i < IDT_ENTRIES {
            let handler = ISR_STUBS[i] as *const () as u64;
            let mut dpl = 0;
            let mut ist = 0;
            if i == VEC_SYSCALL {
                dpl = 3;
            } else if i == VEC_NMI {
                ist = IST_SLOT_NMI;
            } else if i == VEC_DOUBLE_FAULT {
                ist = IST_SLOT_DOUBLE_FAULT;
            } else if i == VEC_PAGE_FAULT {
                ist = IST_SLOT_PAGE_FAULT;
            }
            core::ptr::write(
                idt_ptr.add(i),
                X86IdtEntry::new_interrupt(handler, KERNEL_CODE_SELECTOR, dpl, ist),
            );
            i += 1;
        }

        let ist_nmi_top =
            core::ptr::addr_of!(IST_NMI.0) as u64 + core::mem::size_of::<IstStack>() as u64;
        let ist_df_top = core::ptr::addr_of!(IST_DOUBLE_FAULT.0) as u64
            + core::mem::size_of::<IstStack>() as u64;
        let ist_pf_top =
            core::ptr::addr_of!(IST_PAGE_FAULT.0) as u64 + core::mem::size_of::<IstStack>() as u64;
        BOOT_TSS.ist1 = ist_nmi_top;
        BOOT_TSS.ist2 = ist_df_top;
        BOOT_TSS.ist3 = ist_pf_top;

        let tss_base = core::ptr::addr_of!(BOOT_TSS) as u64;
        let tss_limit = (core::mem::size_of::<X86TaskStateSegment>() - 1) as u32;
        let (tss_low, tss_high) = encode_tss_descriptor(tss_base, tss_limit);
        BOOT_GDT.entries[3] = tss_low;
        BOOT_GDT.entries[4] = tss_high;

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
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "x86_64")))]
pub fn ensure_boot_descriptor_tables_scaffolded() {}

#[cfg(feature = "hosted-dev")]
pub fn ensure_boot_descriptor_tables_scaffolded() {
    let _ = DESCRIPTOR_SCAFFOLD_READY.swap(true, Ordering::AcqRel);
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
}
