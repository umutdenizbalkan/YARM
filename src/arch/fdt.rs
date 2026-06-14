// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 0x1;
const FDT_END_NODE: u32 = 0x2;
const FDT_PROP: u32 = 0x3;
const FDT_NOP: u32 = 0x4;
const FDT_END: u32 = 0x9;

/// Returns the raw `/chosen/bootargs` property bytes, including any firmware
/// supplied trailing NUL. Structural errors reject the FDT rather than yielding
/// a partial value.
pub fn chosen_bootargs(bytes: &[u8]) -> Option<&[u8]> {
    if read_be_u32(bytes, 0)? != FDT_MAGIC {
        return None;
    }
    let total_size = read_be_u32(bytes, 4)? as usize;
    let off_dt_struct = read_be_u32(bytes, 8)? as usize;
    let off_dt_strings = read_be_u32(bytes, 12)? as usize;
    let size_dt_strings = read_be_u32(bytes, 32)? as usize;
    let size_dt_struct = read_be_u32(bytes, 36)? as usize;
    if total_size > bytes.len()
        || off_dt_struct.checked_add(size_dt_struct)? > total_size
        || off_dt_strings.checked_add(size_dt_strings)? > total_size
    {
        return None;
    }
    let struct_block = &bytes[off_dt_struct..off_dt_struct + size_dt_struct];
    let strings = &bytes[off_dt_strings..off_dt_strings + size_dt_strings];
    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut chosen_depth = None;

    while cursor + 4 <= struct_block.len() {
        let token = read_be_u32(struct_block, cursor)?;
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                let (name, next) = read_cstr(struct_block, cursor)?;
                cursor = align_up_4(next)?;
                depth = depth.checked_add(1)?;
                if depth == 2 && name == b"chosen" {
                    chosen_depth = Some(depth);
                }
            }
            FDT_END_NODE => {
                if chosen_depth == Some(depth) {
                    chosen_depth = None;
                }
                depth = depth.checked_sub(1)?;
            }
            FDT_PROP => {
                let prop_len = read_be_u32(struct_block, cursor)? as usize;
                let name_off = read_be_u32(struct_block, cursor + 4)? as usize;
                cursor = cursor.checked_add(8)?;
                let prop_end = cursor.checked_add(prop_len)?;
                if prop_end > struct_block.len() {
                    return None;
                }
                let prop_data = &struct_block[cursor..prop_end];
                cursor = align_up_4(prop_end)?;
                let prop_name = read_cstr(strings, name_off)?.0;
                if chosen_depth == Some(depth) && prop_name == b"bootargs" {
                    return Some(prop_data);
                }
            }
            FDT_NOP => {}
            FDT_END => return None,
            _ => return None,
        }
    }
    None
}

/// Returns the first `/memory` node's first `reg` pair as `(base, size)`,
/// honoring the root `#address-cells` / `#size-cells` (defaulting to 2/2, the
/// QEMU-virt layout). Structural errors reject the FDT rather than yielding a
/// partial value. RISC-V uses this to stage the real RAM window for the frame
/// allocator instead of a hard-coded fallback base.
pub fn memory_reg(bytes: &[u8]) -> Option<(u64, u64)> {
    if read_be_u32(bytes, 0)? != FDT_MAGIC {
        return None;
    }
    let total_size = read_be_u32(bytes, 4)? as usize;
    let off_dt_struct = read_be_u32(bytes, 8)? as usize;
    let off_dt_strings = read_be_u32(bytes, 12)? as usize;
    let size_dt_strings = read_be_u32(bytes, 32)? as usize;
    let size_dt_struct = read_be_u32(bytes, 36)? as usize;
    if total_size > bytes.len()
        || off_dt_struct.checked_add(size_dt_struct)? > total_size
        || off_dt_strings.checked_add(size_dt_strings)? > total_size
    {
        return None;
    }
    let struct_block = &bytes[off_dt_struct..off_dt_struct + size_dt_struct];
    let strings = &bytes[off_dt_strings..off_dt_strings + size_dt_strings];
    let mut cursor = 0usize;
    let mut depth = 0usize;
    // Root-level cell sizes; FDT default is 2 each, which matches QEMU virt.
    let mut address_cells: u32 = 2;
    let mut size_cells: u32 = 2;
    let mut in_memory_depth: Option<usize> = None;

    while cursor + 4 <= struct_block.len() {
        let token = read_be_u32(struct_block, cursor)?;
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                let (name, next) = read_cstr(struct_block, cursor)?;
                cursor = align_up_4(next)?;
                depth = depth.checked_add(1)?;
                // A node named "memory" or "memory@<addr>" at depth 2.
                if depth == 2 && (name == b"memory" || name.starts_with(b"memory@")) {
                    in_memory_depth = Some(depth);
                }
            }
            FDT_END_NODE => {
                if in_memory_depth == Some(depth) {
                    in_memory_depth = None;
                }
                depth = depth.checked_sub(1)?;
            }
            FDT_PROP => {
                let prop_len = read_be_u32(struct_block, cursor)? as usize;
                let name_off = read_be_u32(struct_block, cursor + 4)? as usize;
                cursor = cursor.checked_add(8)?;
                let prop_end = cursor.checked_add(prop_len)?;
                if prop_end > struct_block.len() {
                    return None;
                }
                let prop_data = &struct_block[cursor..prop_end];
                cursor = align_up_4(prop_end)?;
                let prop_name = read_cstr(strings, name_off)?.0;
                // Root cell sizes govern how we decode `reg`.
                if depth == 1 && prop_name == b"#address-cells" {
                    address_cells = read_be_u32(prop_data, 0)?;
                }
                if depth == 1 && prop_name == b"#size-cells" {
                    size_cells = read_be_u32(prop_data, 0)?;
                }
                if in_memory_depth == Some(depth) && prop_name == b"reg" {
                    return decode_reg_pair(prop_data, address_cells, size_cells);
                }
            }
            FDT_NOP => {}
            FDT_END => return None,
            _ => return None,
        }
    }
    None
}

/// Returns the `/chosen` `linux,initrd-start` / `linux,initrd-end` pair as
/// `(start, end)` physical addresses, if present. Each value may be encoded as
/// a 4-byte or 8-byte big-endian integer (QEMU uses either depending on
/// version). RISC-V uses this to locate and reserve the initramfs.
pub fn chosen_initrd(bytes: &[u8]) -> Option<(u64, u64)> {
    if read_be_u32(bytes, 0)? != FDT_MAGIC {
        return None;
    }
    let total_size = read_be_u32(bytes, 4)? as usize;
    let off_dt_struct = read_be_u32(bytes, 8)? as usize;
    let off_dt_strings = read_be_u32(bytes, 12)? as usize;
    let size_dt_strings = read_be_u32(bytes, 32)? as usize;
    let size_dt_struct = read_be_u32(bytes, 36)? as usize;
    if total_size > bytes.len()
        || off_dt_struct.checked_add(size_dt_struct)? > total_size
        || off_dt_strings.checked_add(size_dt_strings)? > total_size
    {
        return None;
    }
    let struct_block = &bytes[off_dt_struct..off_dt_struct + size_dt_struct];
    let strings = &bytes[off_dt_strings..off_dt_strings + size_dt_strings];
    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut chosen_depth: Option<usize> = None;
    let mut initrd_start: Option<u64> = None;
    let mut initrd_end: Option<u64> = None;

    while cursor + 4 <= struct_block.len() {
        let token = read_be_u32(struct_block, cursor)?;
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                let (name, next) = read_cstr(struct_block, cursor)?;
                cursor = align_up_4(next)?;
                depth = depth.checked_add(1)?;
                if depth == 2 && name == b"chosen" {
                    chosen_depth = Some(depth);
                }
            }
            FDT_END_NODE => {
                if chosen_depth == Some(depth) {
                    chosen_depth = None;
                }
                depth = depth.checked_sub(1)?;
            }
            FDT_PROP => {
                let prop_len = read_be_u32(struct_block, cursor)? as usize;
                let name_off = read_be_u32(struct_block, cursor + 4)? as usize;
                cursor = cursor.checked_add(8)?;
                let prop_end = cursor.checked_add(prop_len)?;
                if prop_end > struct_block.len() {
                    return None;
                }
                let prop_data = &struct_block[cursor..prop_end];
                cursor = align_up_4(prop_end)?;
                let prop_name = read_cstr(strings, name_off)?.0;
                if chosen_depth == Some(depth) {
                    if prop_name == b"linux,initrd-start" {
                        initrd_start = decode_int(prop_data);
                    } else if prop_name == b"linux,initrd-end" {
                        initrd_end = decode_int(prop_data);
                    }
                }
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => return None,
        }
    }
    match (initrd_start, initrd_end) {
        (Some(start), Some(end)) if end > start => Some((start, end)),
        _ => None,
    }
}

/// Decodes the first `(address, size)` pair from a `reg` property given the
/// governing cell counts. Only 1- or 2-cell encodings are supported (the only
/// sizes QEMU/real boards use for top-level RAM).
fn decode_reg_pair(data: &[u8], address_cells: u32, size_cells: u32) -> Option<(u64, u64)> {
    let addr = decode_cells(data, 0, address_cells)?;
    let addr_bytes = (address_cells as usize).checked_mul(4)?;
    let size = decode_cells(data, addr_bytes, size_cells)?;
    Some((addr, size))
}

fn decode_cells(data: &[u8], offset: usize, cells: u32) -> Option<u64> {
    match cells {
        1 => Some(read_be_u32(data, offset)? as u64),
        2 => {
            let hi = read_be_u32(data, offset)? as u64;
            let lo = read_be_u32(data, offset + 4)? as u64;
            Some((hi << 32) | lo)
        }
        _ => None,
    }
}

/// Decodes a 4- or 8-byte big-endian integer property value.
fn decode_int(data: &[u8]) -> Option<u64> {
    match data.len() {
        4 => Some(u32::from_be_bytes(data.try_into().ok()?) as u64),
        8 => Some(u64::from_be_bytes(data.try_into().ok()?)),
        _ => None,
    }
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(raw))
}

fn read_cstr(bytes: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    let tail = bytes.get(offset..)?;
    let len = tail.iter().position(|byte| *byte == 0)?;
    Some((&tail[..len], offset.checked_add(len + 1)?))
}

fn align_up_4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|aligned| aligned & !3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn align(out: &mut Vec<u8>) {
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn make_dtb(bootargs: Option<&[u8]>) -> Vec<u8> {
        let mut structure = Vec::new();
        let strings = b"bootargs\0";
        push_u32(&mut structure, FDT_BEGIN_NODE);
        structure.push(0);
        align(&mut structure);
        push_u32(&mut structure, FDT_BEGIN_NODE);
        structure.extend_from_slice(b"chosen\0");
        align(&mut structure);
        if let Some(value) = bootargs {
            push_u32(&mut structure, FDT_PROP);
            push_u32(&mut structure, value.len() as u32);
            push_u32(&mut structure, 0);
            structure.extend_from_slice(value);
            align(&mut structure);
        }
        push_u32(&mut structure, FDT_END_NODE);
        push_u32(&mut structure, FDT_END_NODE);
        push_u32(&mut structure, FDT_END);

        let header = 40usize;
        let strings_offset = header + structure.len();
        let total = strings_offset + strings.len();
        let mut dtb = Vec::new();
        for value in [
            FDT_MAGIC,
            total as u32,
            header as u32,
            strings_offset as u32,
            header as u32,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            push_u32(&mut dtb, value);
        }
        dtb.extend_from_slice(&structure);
        dtb.extend_from_slice(strings);
        dtb
    }

    #[test]
    fn extracts_chosen_bootargs_with_trailing_nul() {
        let dtb = make_dtb(Some(b"console=ttyS0 yarm.manifest=/boot/services.txt\0"));
        assert_eq!(
            chosen_bootargs(&dtb),
            Some(b"console=ttyS0 yarm.manifest=/boot/services.txt\0".as_slice())
        );
    }

    #[test]
    fn reports_absent_bootargs() {
        assert_eq!(chosen_bootargs(&make_dtb(None)), None);
    }

    /// Builds a minimal DTB with root `#address-cells`/`#size-cells`, a
    /// `memory@<base>` node carrying `reg = <base size>`, and a `chosen` node
    /// optionally carrying `linux,initrd-start`/`-end`.
    fn make_dtb_with_memory(
        address_cells: u32,
        size_cells: u32,
        mem_base: u64,
        mem_size: u64,
        initrd: Option<(u64, u64)>,
    ) -> Vec<u8> {
        let mut structure = Vec::new();
        // strings block: collect property names with their offsets.
        let mut strings = Vec::new();
        let mut str_off = |strings: &mut Vec<u8>, s: &[u8]| -> u32 {
            let off = strings.len() as u32;
            strings.extend_from_slice(s);
            strings.push(0);
            off
        };
        let ac_off = str_off(&mut strings, b"#address-cells");
        let sc_off = str_off(&mut strings, b"#size-cells");
        let reg_off = str_off(&mut strings, b"reg");
        let istart_off = str_off(&mut strings, b"linux,initrd-start");
        let iend_off = str_off(&mut strings, b"linux,initrd-end");

        let push_cells = |out: &mut Vec<u8>, value: u64, cells: u32| match cells {
            1 => push_u32(out, value as u32),
            2 => {
                push_u32(out, (value >> 32) as u32);
                push_u32(out, value as u32);
            }
            _ => {}
        };

        // root node
        push_u32(&mut structure, FDT_BEGIN_NODE);
        structure.push(0);
        align(&mut structure);
        // #address-cells
        push_u32(&mut structure, FDT_PROP);
        push_u32(&mut structure, 4);
        push_u32(&mut structure, ac_off);
        push_u32(&mut structure, address_cells);
        // #size-cells
        push_u32(&mut structure, FDT_PROP);
        push_u32(&mut structure, 4);
        push_u32(&mut structure, sc_off);
        push_u32(&mut structure, size_cells);

        // memory@<base> node
        push_u32(&mut structure, FDT_BEGIN_NODE);
        structure.extend_from_slice(b"memory@80000000\0");
        align(&mut structure);
        push_u32(&mut structure, FDT_PROP);
        let reg_len = (address_cells + size_cells) * 4;
        push_u32(&mut structure, reg_len);
        push_u32(&mut structure, reg_off);
        push_cells(&mut structure, mem_base, address_cells);
        push_cells(&mut structure, mem_size, size_cells);
        align(&mut structure);
        push_u32(&mut structure, FDT_END_NODE);

        // chosen node
        push_u32(&mut structure, FDT_BEGIN_NODE);
        structure.extend_from_slice(b"chosen\0");
        align(&mut structure);
        if let Some((start, end)) = initrd {
            push_u32(&mut structure, FDT_PROP);
            push_u32(&mut structure, 8);
            push_u32(&mut structure, istart_off);
            push_u32(&mut structure, (start >> 32) as u32);
            push_u32(&mut structure, start as u32);
            push_u32(&mut structure, FDT_PROP);
            push_u32(&mut structure, 8);
            push_u32(&mut structure, iend_off);
            push_u32(&mut structure, (end >> 32) as u32);
            push_u32(&mut structure, end as u32);
        }
        push_u32(&mut structure, FDT_END_NODE);

        push_u32(&mut structure, FDT_END_NODE); // close root
        push_u32(&mut structure, FDT_END);

        let header = 40usize;
        let strings_offset = header + structure.len();
        let total = strings_offset + strings.len();
        let mut dtb = Vec::new();
        for value in [
            FDT_MAGIC,
            total as u32,
            header as u32,
            strings_offset as u32,
            header as u32,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            push_u32(&mut dtb, value);
        }
        dtb.extend_from_slice(&structure);
        dtb.extend_from_slice(&strings);
        dtb
    }

    #[test]
    fn parses_memory_reg_2_2_cells_qemu_virt_layout() {
        let dtb = make_dtb_with_memory(2, 2, 0x8000_0000, 0x2000_0000, None);
        assert_eq!(memory_reg(&dtb), Some((0x8000_0000, 0x2000_0000)));
    }

    #[test]
    fn parses_memory_reg_1_1_cells() {
        let dtb = make_dtb_with_memory(1, 1, 0x8000_0000, 0x1000_0000, None);
        assert_eq!(memory_reg(&dtb), Some((0x8000_0000, 0x1000_0000)));
    }

    #[test]
    fn parses_chosen_initrd_when_present() {
        let dtb = make_dtb_with_memory(
            2,
            2,
            0x8000_0000,
            0x2000_0000,
            Some((0x8820_0000, 0x882d_05d0)),
        );
        assert_eq!(chosen_initrd(&dtb), Some((0x8820_0000, 0x882d_05d0)));
    }

    #[test]
    fn reports_absent_initrd() {
        let dtb = make_dtb_with_memory(2, 2, 0x8000_0000, 0x2000_0000, None);
        assert_eq!(chosen_initrd(&dtb), None);
    }
}
