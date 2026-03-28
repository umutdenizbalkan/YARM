use core::sync::atomic::{AtomicBool, Ordering};

pub const IDT_ENTRIES: usize = 256;
const IDT_GATE_INTERRUPT: u8 = 0x0E;
const IDT_PRESENT: u8 = 1 << 7;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_TIMER: usize = 0x20;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_NMI: usize = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_DOUBLE_FAULT: usize = 8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_PAGE_FAULT: usize = 14;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_SYSCALL: usize = 0x80;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_EXTERNAL_BASE: usize = 0x20;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const VEC_EXTERNAL_LIMIT: usize = 0x30;

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
extern "C" fn default_interrupt_stub() -> ! {
    loop {
        unsafe {
            core::arch::asm!("cli", "hlt", options(noreturn));
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
extern "C" fn timer_interrupt_stub() -> ! {
    default_interrupt_stub()
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
extern "C" fn page_fault_stub() -> ! {
    default_interrupt_stub()
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
extern "C" fn nmi_interrupt_stub() -> ! {
    default_interrupt_stub()
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
extern "C" fn double_fault_stub() -> ! {
    default_interrupt_stub()
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
extern "C" fn syscall_interrupt_stub() -> ! {
    default_interrupt_stub()
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
extern "C" fn external_interrupt_stub() -> ! {
    default_interrupt_stub()
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn handler_addr(handler: extern "C" fn() -> !) -> u64 {
    handler as *const () as usize as u64
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn ensure_boot_descriptor_tables_scaffolded() {
    if DESCRIPTOR_SCAFFOLD_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    unsafe {
        let handler = handler_addr(default_interrupt_stub);
        let idt_ptr = core::ptr::addr_of_mut!(BOOT_IDT).cast::<X86IdtEntry>();
        let mut i = 0usize;
        while i < IDT_ENTRIES {
            core::ptr::write(
                idt_ptr.add(i),
                X86IdtEntry::new_interrupt(handler, KERNEL_CODE_SELECTOR, 0, 0),
            );
            i += 1;
        }
        BOOT_IDT[VEC_TIMER] = X86IdtEntry::new_interrupt(
            handler_addr(timer_interrupt_stub),
            KERNEL_CODE_SELECTOR,
            0,
            0,
        );
        BOOT_IDT[VEC_NMI] = X86IdtEntry::new_interrupt(
            handler_addr(nmi_interrupt_stub),
            KERNEL_CODE_SELECTOR,
            0,
            IST_SLOT_NMI,
        );
        BOOT_IDT[VEC_DOUBLE_FAULT] = X86IdtEntry::new_interrupt(
            handler_addr(double_fault_stub),
            KERNEL_CODE_SELECTOR,
            0,
            IST_SLOT_DOUBLE_FAULT,
        );
        BOOT_IDT[VEC_PAGE_FAULT] = X86IdtEntry::new_interrupt(
            handler_addr(page_fault_stub),
            KERNEL_CODE_SELECTOR,
            0,
            IST_SLOT_PAGE_FAULT,
        );
        BOOT_IDT[VEC_SYSCALL] = X86IdtEntry::new_interrupt(
            handler_addr(syscall_interrupt_stub),
            KERNEL_CODE_SELECTOR,
            3,
            0,
        );
        for vector in VEC_EXTERNAL_BASE..VEC_EXTERNAL_LIMIT {
            if vector == VEC_TIMER {
                continue;
            }
            BOOT_IDT[vector] = X86IdtEntry::new_interrupt(
                handler_addr(external_interrupt_stub),
                KERNEL_CODE_SELECTOR,
                0,
                0,
            );
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
