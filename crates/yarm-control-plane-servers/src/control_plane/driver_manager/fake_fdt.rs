// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Hosted/test-only fake FDT parser for driver-manager inventory scaffolding.
//!
//! This parser is deliberately tiny and inert: it parses bounded synthetic FDT
//! blobs used by hosted tests and returns [`PlatformInventory`] records only. It
//! is not wired into the live boot path, never reads the real boot DTB, never
//! grants resources, and never spawns drivers.

use super::service::{DeviceClass, DeviceRecord, DeviceStatus, MmioRange, PlatformInventory};
use yarm_user_rt::runtime::KernelIpcError;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const FDT_HEADER_LEN: usize = 40;
const MAX_NODE_NAME: usize = 64;
const MAX_PROP_NAME: usize = 64;
const MAX_COMPATIBLE: usize = 64;
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeFdtError {
    TooSmall,
    BadMagic,
    BadTotalsize,
    BadBlock,
    BadStructBlock,
    BadStringsBlock,
    BadToken,
    BadString,
    BadProperty,
    MalformedReg,
    MalformedRanges,
    TranslationOverflow,
    BadInterrupt,
    Unterminated,
    Inventory(KernelIpcError),
}

impl From<KernelIpcError> for FakeFdtError {
    fn from(value: KernelIpcError) -> Self {
        Self::Inventory(value)
    }
}

#[derive(Debug, Clone, Copy)]
struct FdtHeader {
    off_dt_struct: usize,
    off_dt_strings: usize,
    size_dt_strings: usize,
    size_dt_struct: usize,
}

#[derive(Debug, Clone, Copy)]
struct CurrentNode {
    name: [u8; MAX_NODE_NAME],
    name_len: usize,
    compatible: [u8; MAX_COMPATIBLE],
    compatible_len: usize,
    status_disabled: bool,
    reg: Option<MmioRange>,
    irq: Option<u32>,
    parent_bus: BusContext,
    child_bus: BusContext,
}

impl CurrentNode {
    const fn empty() -> Self {
        Self {
            name: [0; MAX_NODE_NAME],
            name_len: 0,
            compatible: [0; MAX_COMPATIBLE],
            compatible_len: 0,
            status_disabled: false,
            reg: None,
            irq: None,
            parent_bus: BusContext::root(),
            child_bus: BusContext::root(),
        }
    }

    fn reset(&mut self, name: &[u8], parent_bus: BusContext) -> Result<(), FakeFdtError> {
        if name.len() > self.name.len() {
            return Err(FakeFdtError::BadString);
        }
        *self = Self::empty();
        self.name[..name.len()].copy_from_slice(name);
        self.name_len = name.len();
        self.parent_bus = parent_bus;
        self.child_bus = parent_bus;
        Ok(())
    }

    fn is_root(&self) -> bool {
        self.name_len == 0
    }

    fn compatible_str(&self) -> Option<&str> {
        core::str::from_utf8(self.compatible.get(..self.compatible_len)?).ok()
    }
}

#[derive(Debug, Clone, Copy)]
struct RangeTranslation {
    child_base: u64,
    parent_base: u64,
    size: u64,
}

#[derive(Debug, Clone, Copy)]
struct BusContext {
    address_cells: usize,
    size_cells: usize,
    range: Option<RangeTranslation>,
}

impl BusContext {
    const fn root() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
            range: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ParserState {
    bus_stack: [BusContext; MAX_DEPTH],
    current: CurrentNode,
    depth: usize,
}

impl ParserState {
    const fn new() -> Self {
        Self {
            bus_stack: [BusContext::root(); MAX_DEPTH],
            current: CurrentNode::empty(),
            depth: 0,
        }
    }

    fn current_parent_bus(&self) -> Result<BusContext, FakeFdtError> {
        if self.depth == 0 {
            Ok(BusContext::root())
        } else {
            self.bus_stack
                .get(self.depth - 1)
                .copied()
                .ok_or(FakeFdtError::BadBlock)
        }
    }
}

pub fn parse_fake_rpi5_fdt_to_inventory(blob: &[u8]) -> Result<PlatformInventory, FakeFdtError> {
    let header = parse_header(blob)?;
    let struct_block = checked_range(blob, header.off_dt_struct, header.size_dt_struct)?;
    let strings_block = checked_range(blob, header.off_dt_strings, header.size_dt_strings)?;
    let mut cursor = 0usize;
    let mut state = ParserState::new();
    let mut inventory = PlatformInventory::new();
    let mut saw_end = false;

    while cursor < struct_block.len() {
        let token = read_be_u32_at(struct_block, cursor)?;
        cursor = cursor.checked_add(4).ok_or(FakeFdtError::BadBlock)?;
        match token {
            FDT_BEGIN_NODE => {
                let (name, next) = read_cstr(struct_block, cursor, MAX_NODE_NAME)?;
                cursor = align4(next)?;
                let parent_bus = state.current_parent_bus()?;
                state.depth = state.depth.checked_add(1).ok_or(FakeFdtError::BadBlock)?;
                if state.depth > MAX_DEPTH {
                    return Err(FakeFdtError::BadBlock);
                }
                state.bus_stack[state.depth - 1] = parent_bus;
                state.current.reset(name, parent_bus)?;
            }
            FDT_END_NODE => {
                if state.depth == 0 {
                    return Err(FakeFdtError::BadToken);
                }
                if !state.current.is_root() {
                    maybe_add_record(&state.current, &mut inventory)?;
                }
                state.bus_stack[state.depth - 1] = state.current.child_bus;
                state.current = CurrentNode::empty();
                state.depth -= 1;
            }
            FDT_PROP => {
                let len = usize::try_from(read_be_u32_at(struct_block, cursor)?)
                    .map_err(|_| FakeFdtError::BadProperty)?;
                let nameoff = usize::try_from(read_be_u32_at(struct_block, cursor + 4)?)
                    .map_err(|_| FakeFdtError::BadProperty)?;
                cursor = cursor.checked_add(8).ok_or(FakeFdtError::BadBlock)?;
                let value = checked_range(struct_block, cursor, len)?;
                cursor = align4(cursor.checked_add(len).ok_or(FakeFdtError::BadBlock)?)?;
                let (prop_name, _) = read_cstr(strings_block, nameoff, MAX_PROP_NAME)?;
                apply_property(&mut state, prop_name, value)?;
            }
            FDT_NOP => {}
            FDT_END => {
                saw_end = true;
                break;
            }
            _ => return Err(FakeFdtError::BadToken),
        }
    }

    if !saw_end || state.depth != 0 {
        return Err(FakeFdtError::Unterminated);
    }
    Ok(inventory)
}

fn parse_header(blob: &[u8]) -> Result<FdtHeader, FakeFdtError> {
    if blob.len() < FDT_HEADER_LEN {
        return Err(FakeFdtError::TooSmall);
    }
    let magic = read_be_u32_at(blob, 0)?;
    if magic != FDT_MAGIC {
        return Err(FakeFdtError::BadMagic);
    }
    let totalsize =
        usize::try_from(read_be_u32_at(blob, 4)?).map_err(|_| FakeFdtError::BadTotalsize)?;
    if totalsize < FDT_HEADER_LEN || totalsize > blob.len() {
        return Err(FakeFdtError::BadTotalsize);
    }
    let off_dt_struct =
        usize::try_from(read_be_u32_at(blob, 8)?).map_err(|_| FakeFdtError::BadBlock)?;
    let off_dt_strings =
        usize::try_from(read_be_u32_at(blob, 12)?).map_err(|_| FakeFdtError::BadBlock)?;
    let size_dt_strings =
        usize::try_from(read_be_u32_at(blob, 32)?).map_err(|_| FakeFdtError::BadStringsBlock)?;
    let size_dt_struct =
        usize::try_from(read_be_u32_at(blob, 36)?).map_err(|_| FakeFdtError::BadStructBlock)?;
    checked_range(blob, off_dt_struct, size_dt_struct).map_err(|_| FakeFdtError::BadStructBlock)?;
    checked_range(blob, off_dt_strings, size_dt_strings)
        .map_err(|_| FakeFdtError::BadStringsBlock)?;
    let struct_end = off_dt_struct
        .checked_add(size_dt_struct)
        .ok_or(FakeFdtError::BadStructBlock)?;
    let strings_end = off_dt_strings
        .checked_add(size_dt_strings)
        .ok_or(FakeFdtError::BadStringsBlock)?;
    if off_dt_struct >= totalsize || struct_end > totalsize {
        return Err(FakeFdtError::BadStructBlock);
    }
    if off_dt_strings >= totalsize || strings_end > totalsize {
        return Err(FakeFdtError::BadStringsBlock);
    }
    Ok(FdtHeader {
        off_dt_struct,
        off_dt_strings,
        size_dt_strings,
        size_dt_struct,
    })
}

fn apply_property(state: &mut ParserState, name: &[u8], value: &[u8]) -> Result<(), FakeFdtError> {
    match name {
        b"#address-cells" => {
            state.current.child_bus.address_cells = usize::try_from(read_be_u32_at(value, 0)?)
                .map_err(|_| FakeFdtError::BadProperty)?;
            if state.current.child_bus.address_cells == 0
                || state.current.child_bus.address_cells > 2
            {
                return Err(FakeFdtError::BadProperty);
            }
        }
        b"#size-cells" => {
            state.current.child_bus.size_cells = usize::try_from(read_be_u32_at(value, 0)?)
                .map_err(|_| FakeFdtError::BadProperty)?;
            if state.current.child_bus.size_cells == 0 || state.current.child_bus.size_cells > 2 {
                return Err(FakeFdtError::BadProperty);
            }
        }
        b"compatible" => set_compatible(&mut state.current, value)?,
        b"reg" => state.current.reg = Some(parse_reg(value, state.current.parent_bus)?),
        b"ranges" => {
            state.current.child_bus.range =
                parse_ranges(value, state.current.child_bus, state.current.parent_bus)?;
        }
        b"interrupts" | b"yarm,irq" => {
            if value.len() != 4 {
                return Err(FakeFdtError::BadInterrupt);
            }
            state.current.irq = Some(read_be_u32_at(value, 0)?)
        }
        b"status" => state.current.status_disabled = first_cstr_eq(value, b"disabled")?,
        b"interrupt-parent" => return Err(FakeFdtError::BadInterrupt),
        _ => {}
    }
    if state.depth > 0 {
        state.bus_stack[state.depth - 1] = state.current.child_bus;
    }
    Ok(())
}

fn maybe_add_record(
    node: &CurrentNode,
    inventory: &mut PlatformInventory,
) -> Result<(), FakeFdtError> {
    if node.status_disabled || node.compatible_len == 0 {
        return Ok(());
    }
    let compatible = node.compatible_str().ok_or(FakeFdtError::BadString)?;
    let (class, candidate, status) = map_compatible(compatible);
    let mut record = DeviceRecord::new(compatible, class, candidate, status)?;
    if let Some(reg) = node.reg {
        record = record.with_mmio(0, reg.base, reg.len)?;
    }
    if let Some(irq) = node.irq {
        record = record.with_irq(0, irq)?;
    }
    inventory.add(record)?;
    Ok(())
}

fn map_compatible(compatible: &str) -> (DeviceClass, &'static str, DeviceStatus) {
    if compatible == "arm,pl011" || compatible == "brcm,bcm2712-pl011" {
        (DeviceClass::Uart, "uart_srv", DeviceStatus::Discovered)
    } else if compatible == "raspberrypi,firmware" || compatible == "brcm,bcm2712-mailbox" {
        (
            DeviceClass::Mailbox,
            "rpi_firmware",
            DeviceStatus::DeferredNoMmioGrant,
        )
    } else if compatible == "raspberrypi,rp1-gpio" || compatible == "test,rp1-gpio" {
        (
            DeviceClass::Gpio,
            "rp1_gpio_srv",
            DeviceStatus::DeferredNoMmioGrant,
        )
    } else if compatible == "yarm,irqmux" || compatible == "test,irqmux" {
        (DeviceClass::IrqMux, "irqmux_srv", DeviceStatus::Discovered)
    } else {
        (DeviceClass::Unknown, "unknown", DeviceStatus::Unsupported)
    }
}

fn set_compatible(node: &mut CurrentNode, value: &[u8]) -> Result<(), FakeFdtError> {
    let first = first_cstr(value)?;
    if first.is_empty() || first.len() > node.compatible.len() {
        return Err(FakeFdtError::BadString);
    }
    node.compatible[..first.len()].copy_from_slice(first);
    node.compatible_len = first.len();
    Ok(())
}

fn parse_reg(value: &[u8], parent_bus: BusContext) -> Result<MmioRange, FakeFdtError> {
    let cells = parent_bus
        .address_cells
        .checked_add(parent_bus.size_cells)
        .ok_or(FakeFdtError::MalformedReg)?;
    let needed = cells.checked_mul(4).ok_or(FakeFdtError::MalformedReg)?;
    if value.len() != needed {
        return Err(FakeFdtError::MalformedReg);
    }
    let mut cursor = 0usize;
    let child_base = read_cells(value, &mut cursor, parent_bus.address_cells)?;
    let len = read_cells(value, &mut cursor, parent_bus.size_cells)?;
    let base = translate_address(parent_bus, child_base)?;
    MmioRange::new(base, len).map_err(FakeFdtError::Inventory)
}

fn read_cells(value: &[u8], cursor: &mut usize, cells: usize) -> Result<u64, FakeFdtError> {
    let mut out = 0u64;
    for _ in 0..cells {
        out = out.checked_shl(32).ok_or(FakeFdtError::BadProperty)?;
        out |= u64::from(read_be_u32_at(value, *cursor)?);
        *cursor = cursor.checked_add(4).ok_or(FakeFdtError::BadProperty)?;
    }
    Ok(out)
}

fn parse_ranges(
    value: &[u8],
    child_bus: BusContext,
    parent_bus: BusContext,
) -> Result<Option<RangeTranslation>, FakeFdtError> {
    if value.is_empty() {
        return Ok(None);
    }
    let cells = child_bus
        .address_cells
        .checked_add(parent_bus.address_cells)
        .and_then(|cells| cells.checked_add(child_bus.size_cells))
        .ok_or(FakeFdtError::MalformedRanges)?;
    let needed = cells.checked_mul(4).ok_or(FakeFdtError::MalformedRanges)?;
    if value.len() != needed {
        return Err(FakeFdtError::MalformedRanges);
    }
    let mut cursor = 0usize;
    let child_base = read_cells(value, &mut cursor, child_bus.address_cells)?;
    let parent_base = read_cells(value, &mut cursor, parent_bus.address_cells)?;
    let size = read_cells(value, &mut cursor, child_bus.size_cells)?;
    if size == 0 {
        return Err(FakeFdtError::MalformedRanges);
    }
    Ok(Some(RangeTranslation {
        child_base,
        parent_base,
        size,
    }))
}

fn translate_address(parent_bus: BusContext, child_base: u64) -> Result<u64, FakeFdtError> {
    let Some(range) = parent_bus.range else {
        // Test policy: absent ranges means identity/no translation. This keeps
        // BAR-relative fake buses descriptive and not live-grantable unless a
        // later inventory policy marks the device grantable.
        return Ok(child_base);
    };
    let range_end = range
        .child_base
        .checked_add(range.size)
        .ok_or(FakeFdtError::TranslationOverflow)?;
    if child_base < range.child_base || child_base >= range_end {
        return Err(FakeFdtError::MalformedRanges);
    }
    let offset = child_base - range.child_base;
    range
        .parent_base
        .checked_add(offset)
        .ok_or(FakeFdtError::TranslationOverflow)
}

fn first_cstr_eq(value: &[u8], expected: &[u8]) -> Result<bool, FakeFdtError> {
    Ok(first_cstr(value)? == expected)
}

fn first_cstr(value: &[u8]) -> Result<&[u8], FakeFdtError> {
    let end = value
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(FakeFdtError::BadString)?;
    Ok(&value[..end])
}

fn checked_range(bytes: &[u8], offset: usize, len: usize) -> Result<&[u8], FakeFdtError> {
    let end = offset.checked_add(len).ok_or(FakeFdtError::BadBlock)?;
    bytes.get(offset..end).ok_or(FakeFdtError::BadBlock)
}

fn read_be_u32_at(bytes: &[u8], offset: usize) -> Result<u32, FakeFdtError> {
    let raw = checked_range(bytes, offset, 4)?;
    Ok(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_cstr(bytes: &[u8], offset: usize, max_len: usize) -> Result<(&[u8], usize), FakeFdtError> {
    let tail = bytes.get(offset..).ok_or(FakeFdtError::BadString)?;
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(FakeFdtError::BadString)?;
    if end > max_len {
        return Err(FakeFdtError::BadString);
    }
    let next = offset.checked_add(end + 1).ok_or(FakeFdtError::BadString)?;
    Ok((&tail[..end], next))
}

fn align4(value: usize) -> Result<usize, FakeFdtError> {
    value
        .checked_add(3)
        .map(|value| value & !3)
        .ok_or(FakeFdtError::BadBlock)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::control_plane::driver_manager::service::{
        DRIVER_OP_QUERY_MY_IRQS, DRIVER_OP_QUERY_MY_MMIO, DriverRegistry, SpawnAction,
        SpawnBlocker, SpawnPolicy, handle_request_with_sender,
    };
    use alloc::vec;
    use core::cell::Cell;
    use std::collections::BTreeMap;
    use std::vec::Vec;
    use yarm_ipc_abi::driver_abi::DRIVER_OP_GRANT_IRQ;
    use yarm_user_rt::capability::CapId;
    use yarm_user_rt::ipc::Message;
    use yarm_user_rt::runtime::DriverControlOps;

    #[derive(Default)]
    struct FakeFdtBuilder {
        strings: Vec<u8>,
        names: BTreeMap<&'static str, u32>,
        structure: Vec<u8>,
    }

    impl FakeFdtBuilder {
        fn new() -> Self {
            Self::default()
        }

        fn nameoff(&mut self, name: &'static str) -> u32 {
            if let Some(offset) = self.names.get(name) {
                return *offset;
            }
            let offset = u32::try_from(self.strings.len()).unwrap();
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);
            self.names.insert(name, offset);
            offset
        }

        fn token(&mut self, token: u32) {
            self.structure.extend_from_slice(&token.to_be_bytes());
        }

        fn begin(&mut self, name: &str) {
            self.token(FDT_BEGIN_NODE);
            self.structure.extend_from_slice(name.as_bytes());
            self.structure.push(0);
            self.pad_structure();
        }

        fn end_node(&mut self) {
            self.token(FDT_END_NODE);
        }

        fn prop(&mut self, name: &'static str, value: &[u8]) {
            self.token(FDT_PROP);
            self.structure
                .extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
            let nameoff = self.nameoff(name);
            self.structure.extend_from_slice(&nameoff.to_be_bytes());
            self.structure.extend_from_slice(value);
            self.pad_structure();
        }

        fn prop_u32(&mut self, name: &'static str, value: u32) {
            self.prop(name, &value.to_be_bytes());
        }

        fn prop_str(&mut self, name: &'static str, value: &str) {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
            self.prop(name, &bytes);
        }

        fn prop_reg(&mut self, base: u64, len: u64) {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&u32::try_from(base >> 32).unwrap().to_be_bytes());
            bytes.extend_from_slice(&u32::try_from(base & 0xffff_ffff).unwrap().to_be_bytes());
            bytes.extend_from_slice(&u32::try_from(len).unwrap().to_be_bytes());
            self.prop("reg", &bytes);
        }

        fn prop_cells(&mut self, name: &'static str, cells: &[u32]) {
            let mut bytes = Vec::new();
            for cell in cells {
                bytes.extend_from_slice(&cell.to_be_bytes());
            }
            self.prop(name, &bytes);
        }

        fn pad_structure(&mut self) {
            while self.structure.len() % 4 != 0 {
                self.structure.push(0);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            self.token(FDT_END);
            let off_dt_struct = FDT_HEADER_LEN;
            let size_dt_struct = self.structure.len();
            let off_dt_strings = off_dt_struct + size_dt_struct;
            let size_dt_strings = self.strings.len();
            let totalsize = off_dt_strings + size_dt_strings;
            let mut out = vec![0u8; FDT_HEADER_LEN];
            write_header_word(&mut out, 0, FDT_MAGIC);
            write_header_word(&mut out, 4, u32::try_from(totalsize).unwrap());
            write_header_word(&mut out, 8, u32::try_from(off_dt_struct).unwrap());
            write_header_word(&mut out, 12, u32::try_from(off_dt_strings).unwrap());
            write_header_word(&mut out, 16, 0);
            write_header_word(&mut out, 20, 17);
            write_header_word(&mut out, 24, 16);
            write_header_word(&mut out, 28, 0);
            write_header_word(&mut out, 32, u32::try_from(size_dt_strings).unwrap());
            write_header_word(&mut out, 36, u32::try_from(size_dt_struct).unwrap());
            out.extend_from_slice(&self.structure);
            out.extend_from_slice(&self.strings);
            out
        }
    }

    fn write_header_word(out: &mut [u8], offset: usize, value: u32) {
        out[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn base_tree() -> FakeFdtBuilder {
        let mut b = FakeFdtBuilder::new();
        b.begin("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 1);
        b
    }

    fn valid_rpi5_like_blob() -> Vec<u8> {
        let mut b = base_tree();
        b.begin("serial@107d00100000");
        b.prop_str("compatible", "arm,pl011");
        b.prop_reg(0x107d_0010_0000, 0x1000);
        b.prop_u32("interrupts", 121);
        b.end_node();
        b.begin("firmware");
        b.prop_str("compatible", "raspberrypi,firmware");
        b.prop_u32("interrupts", 1);
        b.end_node();
        b.begin("rp1-gpio");
        b.prop_str("compatible", "raspberrypi,rp1-gpio");
        b.prop_reg(0x1_0000, 0x1000);
        b.prop_u32("interrupts", 33);
        b.end_node();
        b.begin("irqmux");
        b.prop_str("compatible", "yarm,irqmux");
        b.prop_u32("yarm,irq", 5);
        b.end_node();
        b.begin("disabled-uart");
        b.prop_str("compatible", "arm,pl011");
        b.prop_str("status", "disabled");
        b.end_node();
        b.end_node();
        b.finish()
    }

    #[derive(Default)]
    struct CountingControl {
        calls: Cell<u32>,
    }

    impl DriverControlOps for CountingControl {
        fn register_driver(&mut self, _tid: u64) -> Result<(), KernelIpcError> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
        fn mint_irq_cap(&mut self, _line: u16) -> Result<CapId, KernelIpcError> {
            self.calls.set(self.calls.get() + 1);
            Ok(CapId(99))
        }
        fn grant_driver_irq(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
        fn mint_dma_region_cap(
            &mut self,
            _mem_cap: CapId,
            _offset: usize,
            _len: usize,
        ) -> Result<CapId, KernelIpcError> {
            self.calls.set(self.calls.get() + 1);
            Ok(CapId(100))
        }
        fn grant_driver_dma(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
        fn restart_task(&mut self, _tid: u64, _token: u64) -> Result<(), KernelIpcError> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    fn msg(opcode: u16, claimed_tid: u64) -> Message {
        Message::with_header(0, opcode, 0, None, &claimed_tid.to_le_bytes()).unwrap()
    }

    fn reply_u32(reply: &Message, offset: usize) -> u32 {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(&reply.as_slice()[offset..offset + 4]);
        u32::from_le_bytes(bytes)
    }

    #[test]
    fn valid_fdt_header_and_rpi5_nodes_build_inventory() {
        let inventory = parse_fake_rpi5_fdt_to_inventory(&valid_rpi5_like_blob()).unwrap();
        assert_eq!(inventory.len(), 4, "disabled node is skipped");
        let uart = inventory.candidates_for(DeviceClass::Uart).next().unwrap();
        assert_eq!(uart.compatible(), Some("arm,pl011"));
        assert_eq!(uart.driver_candidate(), Some("uart_srv"));
        assert_eq!(uart.status, DeviceStatus::Discovered);
        assert_eq!(
            uart.mmio_ranges[0],
            Some(MmioRange {
                base: 0x107d_0010_0000,
                len: 0x1000
            })
        );
        assert_eq!(uart.irq_lines[0], Some(121));
        let mailbox = inventory
            .candidates_for(DeviceClass::Mailbox)
            .next()
            .unwrap();
        assert_eq!(mailbox.status, DeviceStatus::DeferredNoMmioGrant);
        let gpio = inventory.candidates_for(DeviceClass::Gpio).next().unwrap();
        assert_eq!(gpio.status, DeviceStatus::DeferredNoMmioGrant);
        assert_eq!(
            gpio.mmio_ranges[0].unwrap().base,
            0x1_0000,
            "RP1 range is BAR-relative fake data, not BCM2712 direct MMIO"
        );
        let irqmux = inventory
            .candidates_for(DeviceClass::IrqMux)
            .next()
            .unwrap();
        assert_eq!(irqmux.driver_candidate(), Some("irqmux_srv"));
    }

    #[test]
    fn root_two_cell_address_and_two_cell_size_reg_parses() {
        let mut b = FakeFdtBuilder::new();
        b.begin("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin("serial@107d00100000");
        b.prop_str("compatible", "arm,pl011");
        b.prop_cells("reg", &[0x107d, 0x0010_0000, 0, 0x2000]);
        b.prop_u32("interrupts", 121);
        b.end_node();
        b.end_node();
        let inventory = parse_fake_rpi5_fdt_to_inventory(&b.finish()).unwrap();
        let uart = inventory.candidates_for(DeviceClass::Uart).next().unwrap();
        assert_eq!(
            uart.mmio_ranges[0],
            Some(MmioRange {
                base: 0x107d_0010_0000,
                len: 0x2000
            })
        );
    }

    #[test]
    fn child_bus_cell_inheritance_and_ranges_translate_mmio() {
        let mut b = base_tree();
        b.begin("soc");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop_cells("ranges", &[0x1000_0000, 0x107d, 0x0000_0000, 0x0010_0000]);
        b.begin("serial@10001000");
        b.prop_str("compatible", "arm,pl011");
        b.prop_cells("reg", &[0x1000_1000, 0x1000]);
        b.prop_u32("interrupts", 44);
        b.end_node();
        b.end_node();
        b.end_node();
        let inventory = parse_fake_rpi5_fdt_to_inventory(&b.finish()).unwrap();
        let uart = inventory.candidates_for(DeviceClass::Uart).next().unwrap();
        assert_eq!(
            uart.mmio_ranges[0],
            Some(MmioRange {
                base: 0x107d_0000_1000,
                len: 0x1000
            })
        );
    }

    #[test]
    fn nested_bus_inherits_parent_cells_without_ranges_as_identity() {
        let mut b = base_tree();
        b.begin("soc");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin("simple-bus");
        b.begin("serial@2000");
        b.prop_str("compatible", "arm,pl011");
        b.prop_cells("reg", &[0x2000, 0x100]);
        b.end_node();
        b.end_node();
        b.end_node();
        b.end_node();
        let inventory = parse_fake_rpi5_fdt_to_inventory(&b.finish()).unwrap();
        let uart = inventory.candidates_for(DeviceClass::Uart).next().unwrap();
        assert_eq!(
            uart.mmio_ranges[0],
            Some(MmioRange {
                base: 0x2000,
                len: 0x100
            })
        );
    }

    #[test]
    fn malformed_fdt_inputs_are_rejected_cleanly() {
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&[]),
            Err(FakeFdtError::TooSmall)
        ));
        let mut bad_magic = valid_rpi5_like_blob();
        bad_magic[0] = 0;
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&bad_magic),
            Err(FakeFdtError::BadMagic)
        ));
        let mut bad_total = valid_rpi5_like_blob();
        write_header_word(&mut bad_total, 4, u32::MAX);
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&bad_total),
            Err(FakeFdtError::BadTotalsize)
        ));
        let mut bad_struct = valid_rpi5_like_blob();
        write_header_word(&mut bad_struct, 36, u32::MAX);
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&bad_struct),
            Err(FakeFdtError::BadStructBlock)
        ));
    }

    #[test]
    fn malformed_cells_reg_ranges_and_irq_reject_cleanly() {
        let mut bad_cells = base_tree();
        bad_cells.prop_u32("#address-cells", 3);
        bad_cells.end_node();
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&bad_cells.finish()),
            Err(FakeFdtError::BadProperty)
        ));

        let mut truncated_reg = base_tree();
        truncated_reg.begin("serial");
        truncated_reg.prop_str("compatible", "arm,pl011");
        truncated_reg.prop_cells("reg", &[0x107d, 0x1000]);
        truncated_reg.end_node();
        truncated_reg.end_node();
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&truncated_reg.finish()),
            Err(FakeFdtError::MalformedReg)
        ));

        let mut bad_ranges = base_tree();
        bad_ranges.begin("soc");
        bad_ranges.prop_u32("#address-cells", 1);
        bad_ranges.prop_u32("#size-cells", 1);
        bad_ranges.prop_cells("ranges", &[0, 0x107d]);
        bad_ranges.end_node();
        bad_ranges.end_node();
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&bad_ranges.finish()),
            Err(FakeFdtError::MalformedRanges)
        ));

        let mut bad_irq = base_tree();
        bad_irq.begin("serial");
        bad_irq.prop_str("compatible", "arm,pl011");
        bad_irq.prop_cells("interrupts", &[1, 2]);
        bad_irq.end_node();
        bad_irq.end_node();
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&bad_irq.finish()),
            Err(FakeFdtError::BadInterrupt)
        ));
    }

    #[test]
    fn ranges_translation_overflow_rejects() {
        let mut b = base_tree();
        b.begin("soc");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.prop_cells("ranges", &[0, u32::MAX, u32::MAX, 0x1000]);
        b.begin("serial");
        b.prop_str("compatible", "arm,pl011");
        b.prop_cells("reg", &[0x100, 0x100]);
        b.end_node();
        b.end_node();
        b.end_node();
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&b.finish()),
            Err(FakeFdtError::TranslationOverflow)
        ));
    }

    #[test]
    fn rp1_child_under_pcie_parent_remains_bar_relative_and_deferred() {
        let mut b = base_tree();
        b.begin("pcie");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin("rp1");
        b.begin("gpio@10000");
        b.prop_str("compatible", "raspberrypi,rp1-gpio");
        b.prop_cells("reg", &[0x1_0000, 0x1000]);
        b.prop_u32("interrupts", 33);
        b.end_node();
        b.end_node();
        b.end_node();
        b.end_node();
        let parsed = parse_fake_rpi5_fdt_to_inventory(&b.finish()).unwrap();
        let gpio = *parsed.candidates_for(DeviceClass::Gpio).next().unwrap();
        assert_eq!(gpio.status, DeviceStatus::DeferredNoMmioGrant);
        assert_eq!(
            gpio.mmio_ranges[0],
            Some(MmioRange {
                base: 0x1_0000,
                len: 0x1000
            })
        );
        let mut inventory = PlatformInventory::new();
        inventory.add(gpio.assigned_to(11).unwrap()).unwrap();
        assert_eq!(
            inventory.authorize_mmio(11, 0x1_0000, 0x1000),
            Err(KernelIpcError::MissingRight)
        );
    }

    #[test]
    fn unterminated_structure_and_bad_string_offsets_are_rejected() {
        let mut unterminated = valid_rpi5_like_blob();
        let len = unterminated.len();
        unterminated.truncate(len - 4);
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&unterminated),
            Err(FakeFdtError::BadBlock | FakeFdtError::Unterminated | FakeFdtError::BadTotalsize)
        ));

        let mut b = base_tree();
        b.begin("bad");
        b.token(FDT_PROP);
        b.structure.extend_from_slice(&4u32.to_be_bytes());
        b.structure.extend_from_slice(&0xffff_u32.to_be_bytes());
        b.structure.extend_from_slice(&1u32.to_be_bytes());
        b.end_node();
        b.end_node();
        let bad_nameoff = b.finish();
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&bad_nameoff),
            Err(FakeFdtError::BadString)
        ));
    }

    #[test]
    fn unknown_compatible_maps_to_unknown_and_cannot_authorize() {
        let mut b = base_tree();
        b.begin("mystery");
        b.prop_str("compatible", "vendor,mystery");
        b.prop_u32("interrupts", 77);
        b.end_node();
        b.end_node();
        let inventory = parse_fake_rpi5_fdt_to_inventory(&b.finish()).unwrap();
        let unknown = inventory
            .candidates_for(DeviceClass::Unknown)
            .next()
            .unwrap();
        assert_eq!(unknown.status, DeviceStatus::Unsupported);
        let assigned = DeviceRecord::new(
            "vendor,mystery",
            DeviceClass::Unknown,
            "unknown",
            DeviceStatus::Unsupported,
        )
        .unwrap()
        .with_irq(0, 77)
        .unwrap()
        .assigned_to(9)
        .unwrap();
        let mut inv = PlatformInventory::new();
        inv.add(assigned).unwrap();
        assert_eq!(inv.authorize_irq(9, 77), Err(KernelIpcError::MissingRight));
    }

    #[test]
    fn inventory_capacity_is_enforced_while_parsing() {
        let mut b = base_tree();
        for index in 0..40u32 {
            b.begin("uart");
            b.prop_str("compatible", "arm,pl011");
            b.prop_u32("interrupts", index);
            b.end_node();
        }
        b.end_node();
        assert!(matches!(
            parse_fake_rpi5_fdt_to_inventory(&b.finish()),
            Err(FakeFdtError::Inventory(KernelIpcError::CapabilityFull))
        ));
    }

    #[test]
    fn parsed_inventory_queries_are_inert_and_sender_scoped() {
        let parsed = parse_fake_rpi5_fdt_to_inventory(&valid_rpi5_like_blob()).unwrap();
        let uart = *parsed.candidates_for(DeviceClass::Uart).next().unwrap();
        let rp1 = *parsed.candidates_for(DeviceClass::Gpio).next().unwrap();
        let mut inventory = PlatformInventory::new();
        inventory.add(uart.assigned_to(7).unwrap()).unwrap();
        inventory.add(rp1.assigned_to(10).unwrap()).unwrap();
        let mut registry = DriverRegistry::new();
        let mut control = CountingControl::default();
        let mmio = handle_request_with_sender(
            &mut registry,
            &inventory,
            &mut control,
            msg(DRIVER_OP_QUERY_MY_MMIO, 7),
            Some(7),
        )
        .unwrap();
        assert_eq!(mmio.transferred_cap(), None);
        assert_eq!(reply_u32(&mmio, 0), 1);
        assert_eq!(control.calls.get(), 0);
        assert_eq!(
            registry.len(),
            0,
            "queries do not register or spawn drivers"
        );
        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut control,
                msg(DRIVER_OP_QUERY_MY_IRQS, 7),
                Some(10)
            ),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(
            handle_request_with_sender(
                &mut registry,
                &inventory,
                &mut control,
                Message::with_header(
                    0,
                    DRIVER_OP_GRANT_IRQ,
                    0,
                    None,
                    &[10u64.to_le_bytes(), 33u64.to_le_bytes()].concat()
                )
                .unwrap(),
                Some(10)
            ),
            Err(KernelIpcError::MissingRight)
        );
        assert_eq!(
            control.calls.get(),
            0,
            "deferred RP1 grant fails before control ops"
        );
    }

    #[test]
    fn parsed_inventory_can_feed_policy_only_spawn_plan_without_side_effects() {
        let parsed = parse_fake_rpi5_fdt_to_inventory(&valid_rpi5_like_blob()).unwrap();
        let registry = DriverRegistry::new();
        let plan = parsed
            .build_spawn_plan(&registry, SpawnPolicy::hosted_fake_rpi5())
            .unwrap();
        assert_eq!(plan.len(), 4, "disabled fake FDT node stays skipped");
        let uart = plan
            .iter()
            .find(|entry| entry.compatible() == Some("arm,pl011"))
            .unwrap();
        assert_eq!(uart.action, SpawnAction::WouldSpawn);
        let rp1 = plan
            .iter()
            .find(|entry| entry.compatible() == Some("raspberrypi,rp1-gpio"))
            .unwrap();
        assert_eq!(rp1.action, SpawnAction::Deferred);
        assert!(rp1.has_blocker(SpawnBlocker::RequiresPcieBarDiscovery));
        assert!(rp1.has_blocker(SpawnBlocker::MissingMmioGrant));
    }
}
