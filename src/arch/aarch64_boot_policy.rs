// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use core::fmt;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const MAX_PATH: usize = 192;
const MAX_DEPTH: usize = 32;
const RPI5_PREFERRED_UART: &[u8] = b"/soc@107c000000/serial@7d001000";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetectedPlatform {
    QemuVirt,
    Rpi5Bcm2712,
    #[default]
    Unknown,
}

impl DetectedPlatform {
    pub const fn label(self) -> &'static str {
        match self {
            Self::QemuVirt => "qemu-virt",
            Self::Rpi5Bcm2712 => "rpi5-bcm2712",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DtbPath {
    bytes: [u8; MAX_PATH],
    len: usize,
}

impl DtbPath {
    pub const fn empty() -> Self {
        Self {
            bytes: [0; MAX_PATH],
            len: 0,
        }
    }

    fn set(&mut self, value: &[u8]) -> bool {
        if value.len() > self.bytes.len() {
            return false;
        }
        self.bytes.fill(0);
        self.bytes[..value.len()].copy_from_slice(value);
        self.len = value.len();
        true
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("<non-utf8-dtb-path>")
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for DtbPath {
    fn default() -> Self {
        Self::empty()
    }
}
impl fmt::Debug for DtbPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DtbPath").field(&self.as_str()).finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialSelection {
    pub path: DtbPath,
    pub base: u64,
    pub size: u64,
    pub clock_hz: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlatformDtbInfo {
    pub platform: DetectedPlatform,
    pub memory_start: Option<u64>,
    pub memory_len: Option<u64>,
    pub reserved_count: u32,
    pub first_reserved_start: Option<u64>,
    pub first_reserved_len: Option<u64>,
    pub interrupt_controller_path: DtbPath,
    pub interrupt_controller_base: Option<u64>,
    pub initrd_start: Option<u64>,
    pub initrd_end: Option<u64>,
    pub stdout_path: DtbPath,
    pub serial: Option<SerialSelection>,
}

impl PlatformDtbInfo {
    pub const fn has_initrd(&self) -> bool {
        matches!((self.initrd_start, self.initrd_end), (Some(start), Some(end)) if end > start)
    }
}

#[derive(Clone, Copy)]
struct Blocks<'a> {
    structure: &'a [u8],
    strings: &'a [u8],
}

#[derive(Clone, Copy)]
struct Walker<'a> {
    blocks: Blocks<'a>,
    cursor: usize,
    path: [u8; MAX_PATH],
    path_len: usize,
    parent_lens: [usize; MAX_DEPTH],
    depth: usize,
    address_cells: [u32; MAX_DEPTH],
    size_cells: [u32; MAX_DEPTH],
}

#[derive(Clone, Copy)]
enum Event<'a> {
    Begin {
        path: &'a [u8],
        name: &'a [u8],
    },
    Property {
        path: &'a [u8],
        name: &'a [u8],
        value: &'a [u8],
        parent_address_cells: u32,
        parent_size_cells: u32,
    },
    EndNode,
    End,
}

impl<'a> Walker<'a> {
    fn new(blocks: Blocks<'a>) -> Self {
        let mut address_cells = [2; MAX_DEPTH];
        let mut size_cells = [1; MAX_DEPTH];
        address_cells[0] = 2;
        size_cells[0] = 1;
        Self {
            blocks,
            cursor: 0,
            path: [0; MAX_PATH],
            path_len: 0,
            parent_lens: [0; MAX_DEPTH],
            depth: 0,
            address_cells,
            size_cells,
        }
    }

    fn next(&mut self) -> Option<Event<'_>> {
        loop {
            let token = be32(self.blocks.structure, self.cursor)?;
            self.cursor += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let (name, next) = cstr(self.blocks.structure, self.cursor)?;
                    self.cursor = align4(next)?;
                    if self.depth >= MAX_DEPTH {
                        return None;
                    }
                    self.parent_lens[self.depth] = self.path_len;
                    if self.depth == 0 {
                        self.path[0] = b'/';
                        self.path_len = 1;
                    } else {
                        if self.path_len > 1 {
                            self.push(b'/')?;
                        }
                        self.extend(name)?;
                    }
                    let parent_depth = self.depth.saturating_sub(1);
                    let parent_address_cells = self.address_cells[parent_depth];
                    let parent_size_cells = self.size_cells[parent_depth];
                    self.address_cells[self.depth] = parent_address_cells;
                    self.size_cells[self.depth] = parent_size_cells;
                    self.depth += 1;
                    return Some(Event::Begin {
                        path: &self.path[..self.path_len],
                        name,
                    });
                }
                FDT_END_NODE => {
                    if self.depth == 0 {
                        return None;
                    }
                    self.depth -= 1;
                    self.path_len = self.parent_lens[self.depth];
                    return Some(Event::EndNode);
                }
                FDT_PROP => {
                    let len = be32(self.blocks.structure, self.cursor)? as usize;
                    let name_off = be32(self.blocks.structure, self.cursor + 4)? as usize;
                    self.cursor += 8;
                    let end = self.cursor.checked_add(len)?;
                    let value = self.blocks.structure.get(self.cursor..end)?;
                    self.cursor = align4(end)?;
                    let name = cstr(self.blocks.strings, name_off)?.0;
                    let node_depth = self.depth.checked_sub(1)?;
                    if name == b"#address-cells" {
                        self.address_cells[node_depth] = be32(value, 0)?;
                    }
                    if name == b"#size-cells" {
                        self.size_cells[node_depth] = be32(value, 0)?;
                    }
                    let parent_depth = node_depth.saturating_sub(1);
                    return Some(Event::Property {
                        path: &self.path[..self.path_len],
                        name,
                        value,
                        parent_address_cells: self.address_cells[parent_depth],
                        parent_size_cells: self.size_cells[parent_depth],
                    });
                }
                FDT_NOP => continue,
                FDT_END => return Some(Event::End),
                _ => return None,
            }
        }
    }

    fn push(&mut self, byte: u8) -> Option<()> {
        if self.path_len == MAX_PATH {
            return None;
        }
        self.path[self.path_len] = byte;
        self.path_len += 1;
        Some(())
    }
    fn extend(&mut self, bytes: &[u8]) -> Option<()> {
        if self.path_len.checked_add(bytes.len())? > MAX_PATH {
            return None;
        }
        self.path[self.path_len..self.path_len + bytes.len()].copy_from_slice(bytes);
        self.path_len += bytes.len();
        Some(())
    }
}

pub fn parse_platform_dtb(bytes: &[u8]) -> Option<PlatformDtbInfo> {
    let blocks = blocks(bytes)?;
    let mut info = PlatformDtbInfo::default();
    let mut stdout_ref = DtbPath::empty();
    let mut stdout_alias = DtbPath::empty();
    let mut root_address_cells = 2;
    let mut root_size_cells = 1;
    let mut walker = Walker::new(blocks);
    while let Some(event) = walker.next() {
        match event {
            Event::Property {
                path,
                name,
                value,
                parent_address_cells,
                parent_size_cells,
            } => {
                if path == b"/" && name == b"compatible" {
                    info.platform = classify_compatible(value);
                }
                if path == b"/" && name == b"#address-cells" {
                    root_address_cells = be32(value, 0)?;
                }
                if path == b"/" && name == b"#size-cells" {
                    root_size_cells = be32(value, 0)?;
                }
                if path == b"/chosen"
                    && (name == b"stdout-path"
                        || (name == b"linux,stdout-path" && stdout_ref.is_empty()))
                {
                    let raw = first_string(value);
                    let reference = raw.split(|byte| *byte == b':').next().unwrap_or(&[]);
                    stdout_ref.set(reference);
                }
                if path == b"/chosen" && name == b"linux,initrd-start" {
                    info.initrd_start = scalar(value);
                }
                if path == b"/chosen" && name == b"linux,initrd-end" {
                    info.initrd_end = scalar(value);
                }
                if is_memory_path(path) && name == b"reg" && info.memory_start.is_none() {
                    (info.memory_start, info.memory_len) =
                        first_reg(value, parent_address_cells, parent_size_cells);
                }
                if path.starts_with(b"/reserved-memory/") && name == b"reg" {
                    if let (Some(start), Some(len)) =
                        first_reg(value, parent_address_cells, parent_size_cells)
                    {
                        info.reserved_count = info.reserved_count.saturating_add(1);
                        if info.first_reserved_start.is_none() {
                            info.first_reserved_start = Some(start);
                            info.first_reserved_len = Some(len);
                        }
                    }
                }
            }
            Event::End => break,
            _ => {}
        }
    }
    if stdout_ref.as_bytes().starts_with(b"/") {
        info.stdout_path = stdout_ref;
    } else if !stdout_ref.is_empty() {
        let mut aliases = Walker::new(blocks);
        while let Some(event) = aliases.next() {
            if let Event::Property {
                path, name, value, ..
            } = event
            {
                if path == b"/aliases" && name == stdout_ref.as_bytes() {
                    stdout_alias.set(first_string(value));
                    break;
                }
            }
        }
        info.stdout_path = if stdout_alias.is_empty() {
            stdout_ref
        } else {
            stdout_alias
        };
    }

    let preferred = find_serial(blocks, RPI5_PREFERRED_UART);
    let resolved = if info.stdout_path.is_empty() {
        None
    } else {
        find_serial(blocks, info.stdout_path.as_bytes())
    };
    let first = find_first_pl011(blocks);
    info.serial = if info.platform == DetectedPlatform::Rpi5Bcm2712 {
        preferred.or(resolved).or(first)
    } else {
        resolved.or(first)
    };
    if let Some(mut serial) = info.serial {
        serial.base =
            translate_to_root(blocks, serial.path.as_bytes(), serial.base).unwrap_or(serial.base);
        info.serial = Some(serial);
    }

    let mut gic = Walker::new(blocks);
    let mut candidate = DtbPath::empty();
    let mut candidate_base = None;
    let mut node_is_interrupt = false;
    let mut current_path = DtbPath::empty();
    while let Some(event) = gic.next() {
        match event {
            Event::Begin { path, name, .. } => {
                current_path.set(path);
                node_is_interrupt =
                    name.starts_with(b"intc") || name.starts_with(b"interrupt-controller");
            }
            Event::Property {
                path,
                name,
                value,
                parent_address_cells,
                parent_size_cells,
            } => {
                if name == b"interrupt-controller" {
                    node_is_interrupt = true;
                }
                if node_is_interrupt && name == b"reg" && candidate_base.is_none() {
                    let (base, _) = first_reg(value, parent_address_cells, parent_size_cells);
                    candidate.set(path);
                    candidate_base = base;
                }
            }
            Event::EndNode => {
                node_is_interrupt = false;
                current_path = DtbPath::empty();
            }
            Event::End => break,
        }
    }
    if let Some(base) = candidate_base {
        info.interrupt_controller_base =
            translate_to_root(blocks, candidate.as_bytes(), base).or(Some(base));
        info.interrupt_controller_path = candidate;
    }
    let _ = (root_address_cells, root_size_cells, current_path);
    Some(info)
}

fn find_serial(blocks: Blocks<'_>, wanted: &[u8]) -> Option<SerialSelection> {
    find_serial_matching(blocks, Some(wanted))
}
fn find_first_pl011(blocks: Blocks<'_>) -> Option<SerialSelection> {
    find_serial_matching(blocks, None)
}

fn find_serial_matching(blocks: Blocks<'_>, wanted: Option<&[u8]>) -> Option<SerialSelection> {
    let mut walker = Walker::new(blocks);
    let mut current = DtbPath::empty();
    let mut match_node = false;
    let mut usable = true;
    let mut pl011 = false;
    let mut base = None;
    let mut size = 0;
    let mut clock = None;
    while let Some(event) = walker.next() {
        match event {
            Event::Begin { path, name, .. } => {
                current.set(path);
                match_node = wanted.map_or(
                    name.starts_with(b"serial@") || name.starts_with(b"uart@"),
                    |value| value == path,
                );
                usable = true;
                pl011 = false;
                base = None;
                size = 0;
                clock = None;
            }
            Event::Property {
                path,
                name,
                value,
                parent_address_cells,
                parent_size_cells,
            } if match_node && path == current.as_bytes() => {
                if name == b"compatible" {
                    pl011 = string_list_contains(value, b"arm,pl011")
                        || string_list_contains(value, b"arm,primecell");
                }
                if name == b"status" {
                    let status = first_string(value);
                    usable = status.is_empty() || status == b"okay" || status == b"ok";
                }
                if name == b"reg" {
                    let (reg_base, reg_size) =
                        first_reg(value, parent_address_cells, parent_size_cells);
                    base = reg_base;
                    size = reg_size.unwrap_or(0);
                }
                if name == b"clock-frequency" {
                    clock = be32(value, 0);
                }
            }
            Event::EndNode => {
                if match_node
                    && usable
                    && pl011
                    && let Some(base) = base
                {
                    return Some(SerialSelection {
                        path: current,
                        base,
                        size,
                        clock_hz: clock,
                    });
                }
                match_node = false;
            }
            Event::End => break,
            _ => {}
        }
    }
    None
}

fn translate_to_root(blocks: Blocks<'_>, node_path: &[u8], mut address: u64) -> Option<u64> {
    let mut path = DtbPath::empty();
    path.set(node_path);
    for _ in 0..MAX_DEPTH {
        let parent_len = path
            .as_bytes()
            .iter()
            .rposition(|b| *b == b'/')
            .unwrap_or(0);
        if parent_len == 0 {
            return Some(address);
        }
        path.len = parent_len;
        if let Some((child, parent, size)) = node_ranges(blocks, path.as_bytes()) {
            if address >= child && address < child.checked_add(size)? {
                address = parent.checked_add(address - child)?;
            }
        }
    }
    None
}

fn node_ranges(blocks: Blocks<'_>, wanted: &[u8]) -> Option<(u64, u64, u64)> {
    let mut walker = Walker::new(blocks);
    while let Some(event) = walker.next() {
        if let Event::Property {
            path,
            name,
            value,
            parent_address_cells,
            parent_size_cells,
        } = event
        {
            if path == wanted && name == b"ranges" {
                if value.is_empty() {
                    return None;
                }
                let child_cells = node_cell_count(blocks, wanted, b"#address-cells")
                    .unwrap_or(parent_address_cells);
                let child = cells(value, child_cells, 0)?;
                let parent = cells(value, parent_address_cells, child_cells as usize)?;
                let size = cells(
                    value,
                    parent_size_cells,
                    (child_cells + parent_address_cells) as usize,
                )?;
                return Some((child, parent, size));
            }
        }
    }
    None
}

fn node_cell_count(blocks: Blocks<'_>, wanted: &[u8], property: &[u8]) -> Option<u32> {
    let mut walker = Walker::new(blocks);
    while let Some(event) = walker.next() {
        if let Event::Property {
            path, name, value, ..
        } = event
        {
            if path == wanted && name == property {
                return be32(value, 0);
            }
        }
    }
    None
}

fn classify_compatible(value: &[u8]) -> DetectedPlatform {
    if string_list_contains(value, b"raspberrypi,5-model-b")
        || string_list_contains(value, b"brcm,bcm2712")
    {
        DetectedPlatform::Rpi5Bcm2712
    } else if string_list_contains(value, b"linux,dummy-virt")
        || string_list_contains(value, b"qemu,virt")
    {
        DetectedPlatform::QemuVirt
    } else {
        DetectedPlatform::Unknown
    }
}

fn blocks(bytes: &[u8]) -> Option<Blocks<'_>> {
    if be32(bytes, 0)? != FDT_MAGIC {
        return None;
    }
    let total = be32(bytes, 4)? as usize;
    let struct_off = be32(bytes, 8)? as usize;
    let strings_off = be32(bytes, 12)? as usize;
    let strings_len = be32(bytes, 32)? as usize;
    let struct_len = be32(bytes, 36)? as usize;
    if total > bytes.len() {
        return None;
    }
    Some(Blocks {
        structure: bytes.get(struct_off..struct_off.checked_add(struct_len)?)?,
        strings: bytes.get(strings_off..strings_off.checked_add(strings_len)?)?,
    })
}
fn is_memory_path(path: &[u8]) -> bool {
    path.strip_prefix(b"/")
        .is_some_and(|rest| rest.starts_with(b"memory@") || rest == b"memory")
}
fn first_string(value: &[u8]) -> &[u8] {
    &value[..value.iter().position(|b| *b == 0).unwrap_or(value.len())]
}
fn string_list_contains(value: &[u8], wanted: &[u8]) -> bool {
    value.split(|b| *b == 0).any(|part| part == wanted)
}
fn scalar(value: &[u8]) -> Option<u64> {
    match value.len() {
        4 => Some(be32(value, 0)? as u64),
        8 => Some((be32(value, 0)? as u64) << 32 | be32(value, 4)? as u64),
        _ => None,
    }
}
fn first_reg(value: &[u8], address_cells: u32, size_cells: u32) -> (Option<u64>, Option<u64>) {
    (
        cells(value, address_cells, 0),
        cells(value, size_cells, address_cells as usize),
    )
}
fn cells(value: &[u8], count: u32, offset_cells: usize) -> Option<u64> {
    if count > 2 {
        return None;
    }
    let mut out = 0;
    for i in 0..count as usize {
        out = (out << 32) | be32(value, (offset_cells + i) * 4)? as u64;
    }
    Some(out)
}
fn be32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn cstr(bytes: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    let tail = bytes.get(offset..)?;
    let len = tail.iter().position(|b| *b == 0)?;
    Some((&tail[..len], offset + len + 1))
}
fn align4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|v| v & !3)
}

pub fn rpi5_phase_allows_boot(
    phase: crate::kernel::boot_command_line::BootPhase,
    has_initrd: bool,
) -> bool {
    use crate::kernel::boot_command_line::BootPhase;
    match phase {
        BootPhase::Entry | BootPhase::Uart | BootPhase::Dtb | BootPhase::Mmu => true,
        BootPhase::Kernel => has_initrd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot_command_line::BootPhase;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    fn be(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }
    fn begin(out: &mut Vec<u8>, name: &[u8]) {
        be(out, FDT_BEGIN_NODE);
        out.extend_from_slice(name);
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    fn end(out: &mut Vec<u8>) {
        be(out, FDT_END_NODE);
    }
    fn prop(
        out: &mut Vec<u8>,
        strings: &mut Vec<u8>,
        offsets: &mut BTreeMap<&'static str, u32>,
        name: &'static str,
        value: &[u8],
    ) {
        let offset = *offsets.entry(name).or_insert_with(|| {
            let offset = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            offset
        });
        be(out, FDT_PROP);
        be(out, value.len() as u32);
        be(out, offset);
        out.extend_from_slice(value);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    fn reg64(address: u64, size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(address >> 32).to_be_bytes()[4..]);
        out.extend_from_slice(&(address as u32).to_be_bytes());
        out.extend_from_slice(&(size >> 32).to_be_bytes()[4..]);
        out.extend_from_slice(&(size as u32).to_be_bytes());
        out
    }
    fn finish(structure: Vec<u8>, strings: Vec<u8>) -> Vec<u8> {
        let header = 40usize;
        let struct_off = header;
        let strings_off = struct_off + structure.len();
        let total = strings_off + strings.len();
        let mut out = Vec::new();
        for value in [
            FDT_MAGIC,
            total as u32,
            struct_off as u32,
            strings_off as u32,
            header as u32,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            be(&mut out, value);
        }
        out.extend_from_slice(&structure);
        out.extend_from_slice(&strings);
        out
    }
    fn test_dtb(compatible: &[u8], rpi: bool, with_initrd: bool) -> Vec<u8> {
        let mut st = Vec::new();
        let mut strings = Vec::new();
        let mut offsets = BTreeMap::new();
        begin(&mut st, b"");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "#address-cells",
            &2u32.to_be_bytes(),
        );
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "#size-cells",
            &2u32.to_be_bytes(),
        );
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "compatible",
            compatible,
        );
        begin(&mut st, b"aliases");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "serial10",
            if rpi {
                b"/soc@107c000000/serial@7d001000\0"
            } else {
                b"/pl011@9000000\0"
            },
        );
        end(&mut st);
        begin(&mut st, b"chosen");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "stdout-path",
            b"serial10:115200n8\0",
        );
        if with_initrd {
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "linux,initrd-start",
                &0x0800_0000u64.to_be_bytes(),
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "linux,initrd-end",
                &0x0810_0000u64.to_be_bytes(),
            );
        }
        end(&mut st);
        begin(&mut st, b"memory@0");
        prop(
            &mut st,
            &mut strings,
            &mut offsets,
            "reg",
            &reg64(if rpi { 0 } else { 0x4000_0000 }, 0x4000_0000),
        );
        end(&mut st);
        if rpi {
            begin(&mut st, b"reserved-memory");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#address-cells",
                &2u32.to_be_bytes(),
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#size-cells",
                &2u32.to_be_bytes(),
            );
            begin(&mut st, b"area@1000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg64(0x1000, 0x2000),
            );
            end(&mut st);
            end(&mut st);
            begin(&mut st, b"soc@107c000000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#address-cells",
                &2u32.to_be_bytes(),
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "#size-cells",
                &2u32.to_be_bytes(),
            );
            let mut ranges = Vec::new();
            ranges.extend_from_slice(&reg64(0x7c00_0000, 0x0400_0000)[..8]);
            ranges.extend_from_slice(&0x0000_0010u32.to_be_bytes());
            ranges.extend_from_slice(&0x7c00_0000u32.to_be_bytes());
            ranges.extend_from_slice(&0u32.to_be_bytes());
            ranges.extend_from_slice(&0x0400_0000u32.to_be_bytes());
            prop(&mut st, &mut strings, &mut offsets, "ranges", &ranges);
            begin(&mut st, b"serial@7d001000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "compatible",
                b"arm,pl011\0arm,primecell\0",
            );
            prop(&mut st, &mut strings, &mut offsets, "status", b"okay\0");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg64(0x7d00_1000, 0x1000),
            );
            end(&mut st);
            begin(&mut st, b"interrupt-controller@7fff9000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "interrupt-controller",
                &[],
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg64(0x7fff_9000, 0x1000),
            );
            end(&mut st);
            end(&mut st);
        } else {
            begin(&mut st, b"pl011@9000000");
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "compatible",
                b"arm,pl011\0",
            );
            prop(
                &mut st,
                &mut strings,
                &mut offsets,
                "reg",
                &reg64(0x0900_0000, 0x1000),
            );
            end(&mut st);
        }
        end(&mut st);
        be(&mut st, FDT_END);
        finish(st, strings)
    }

    #[test]
    fn detects_qemu_virt_and_resolves_absolute_uart_alias() {
        let info = parse_platform_dtb(&test_dtb(b"linux,dummy-virt\0", false, true)).unwrap();
        assert_eq!(info.platform, DetectedPlatform::QemuVirt);
        assert_eq!(info.serial.unwrap().base, 0x0900_0000);
        assert!(info.has_initrd());
    }

    #[test]
    fn detects_rpi5_prefers_soc_pl011_and_translates_ranges() {
        let info = parse_platform_dtb(&test_dtb(
            b"raspberrypi,5-model-b\0brcm,bcm2712\0",
            true,
            false,
        ))
        .unwrap();
        assert_eq!(info.platform, DetectedPlatform::Rpi5Bcm2712);
        assert_eq!(info.stdout_path.as_str(), "/soc@107c000000/serial@7d001000");
        assert_eq!(info.serial.unwrap().base, 0x10_7d00_1000);
        assert_eq!(info.memory_start, Some(0));
        assert_eq!(info.reserved_count, 1);
        assert_eq!(
            info.interrupt_controller_path.as_str(),
            "/soc@107c000000/interrupt-controller@7fff9000"
        );
    }

    #[test]
    fn missing_initrd_is_allowed_before_kernel_phase_only() {
        for phase in [
            BootPhase::Entry,
            BootPhase::Uart,
            BootPhase::Dtb,
            BootPhase::Mmu,
        ] {
            assert!(rpi5_phase_allows_boot(phase, false));
        }
        assert!(!rpi5_phase_allows_boot(BootPhase::Kernel, false));
        assert!(rpi5_phase_allows_boot(BootPhase::Kernel, true));
    }
}
