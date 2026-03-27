use core::sync::atomic::{AtomicBool, Ordering};

pub const IDT_ENTRIES: usize = 256;
const IDT_GATE_INTERRUPT: u8 = 0x0E;
const IDT_PRESENT: u8 = 1 << 7;

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
        Self {
            limit: (core::mem::size_of::<X86IdtEntry>() * IDT_ENTRIES - 1) as u16,
            base: table.as_ptr() as u64,
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

#[cfg(not(feature = "hosted-dev"))]
pub fn ensure_boot_descriptor_tables_scaffolded() {
    if DESCRIPTOR_SCAFFOLD_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    // Scaffolding phase:
    // - Keep typed IDT/TSS data structures in-tree.
    // - Prepare caller-visible one-time init point for future `lidt`/`ltr` wiring.
}

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
