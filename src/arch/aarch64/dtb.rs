// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 0x1;
const FDT_END_NODE: u32 = 0x2;
const FDT_PROP: u32 = 0x3;
const FDT_NOP: u32 = 0x4;
const FDT_END: u32 = 0x9;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParsedDtb {
    pub memory_start: Option<u64>,
    pub memory_len: Option<u64>,
    pub initrd_start: Option<u64>,
    pub initrd_end: Option<u64>,
    pub gic_cpu_if_base: Option<usize>,
}

pub fn parse_boot_dtb(bytes: &[u8]) -> Option<ParsedDtb> {
    if read_be_u32(bytes, 0)? != FDT_MAGIC {
        return None;
    }
    let total_size = read_be_u32(bytes, 4)? as usize;
    let off_dt_struct = read_be_u32(bytes, 8)? as usize;
    let off_dt_strings = read_be_u32(bytes, 12)? as usize;
    let size_dt_strings = read_be_u32(bytes, 32)? as usize;
    let size_dt_struct = read_be_u32(bytes, 36)? as usize;
    if total_size > bytes.len() || off_dt_struct.checked_add(size_dt_struct)? > total_size {
        return None;
    }
    if off_dt_strings.checked_add(size_dt_strings)? > total_size {
        return None;
    }
    let struct_block = &bytes[off_dt_struct..off_dt_struct + size_dt_struct];
    let strings = &bytes[off_dt_strings..off_dt_strings + size_dt_strings];

    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut out = ParsedDtb::default();
    let mut address_cells: u32 = 2;
    let mut size_cells: u32 = 2;
    let mut inside_memory = false;
    let mut inside_chosen = false;
    let mut inside_interrupt_controller = false;
    let mut gic_prefers_second_reg = false;

    while cursor + 4 <= struct_block.len() {
        let token = read_be_u32(struct_block, cursor)?;
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                let (name, next) = read_cstr(struct_block, cursor)?;
                cursor = align_up_4(next);
                depth = depth.saturating_add(1);
                inside_memory = name.starts_with(b"memory");
                inside_chosen = name == b"chosen";
                inside_interrupt_controller =
                    name.starts_with(b"intc") || name.starts_with(b"interrupt-controller");
                gic_prefers_second_reg = false;
            }
            FDT_END_NODE => {
                depth = depth.saturating_sub(1);
                inside_memory = false;
                inside_chosen = false;
                inside_interrupt_controller = false;
                gic_prefers_second_reg = false;
            }
            FDT_PROP => {
                let prop_len = read_be_u32(struct_block, cursor)? as usize;
                let name_off = read_be_u32(struct_block, cursor + 4)? as usize;
                cursor += 8;
                let prop_data_end = cursor.checked_add(prop_len)?;
                if prop_data_end > struct_block.len() {
                    return None;
                }
                let prop_data = &struct_block[cursor..prop_data_end];
                cursor = align_up_4(prop_data_end);
                let prop_name = read_cstr(strings, name_off)?.0;

                if depth == 1 && prop_name == b"#address-cells" {
                    address_cells = read_cells_as_u64(prop_data, 1)? as u32;
                } else if depth == 1 && prop_name == b"#size-cells" {
                    size_cells = read_cells_as_u64(prop_data, 1)? as u32;
                } else if inside_memory && prop_name == b"reg" {
                    if out.memory_start.is_none() || out.memory_len.is_none() {
                        out.memory_start =
                            read_cells_tuple_64(prop_data, address_cells, 0).map(|v| v.0);
                        out.memory_len =
                            read_cells_tuple_64(prop_data, size_cells, address_cells as usize)
                                .map(|v| v.0);
                    }
                } else if inside_chosen && prop_name == b"linux,initrd-start" {
                    out.initrd_start = read_initrd_scalar(prop_data);
                } else if inside_chosen && prop_name == b"linux,initrd-end" {
                    out.initrd_end = read_initrd_scalar(prop_data);
                } else if inside_interrupt_controller && prop_name == b"compatible" {
                    gic_prefers_second_reg = prop_data
                        .split(|b| *b == 0)
                        .any(|part| part == b"arm,cortex-a15-gic" || part == b"arm,gic-400");
                } else if inside_interrupt_controller
                    && prop_name == b"reg"
                    && out.gic_cpu_if_base.is_none()
                {
                    let cell_span = (address_cells + size_cells) as usize;
                    if cell_span > 0 {
                        let first =
                            read_cells_tuple_64(prop_data, address_cells, 0).map(|v| v.0 as usize);
                        let second = read_cells_tuple_64(prop_data, address_cells, cell_span)
                            .map(|v| v.0 as usize);
                        out.gic_cpu_if_base = if gic_prefers_second_reg {
                            second.or(first)
                        } else {
                            first
                        };
                    }
                }
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => return None,
        }
    }
    Some(out)
}

fn read_initrd_scalar(bytes: &[u8]) -> Option<u64> {
    match bytes.len() {
        4 => Some(read_be_u32(bytes, 0)? as u64),
        8 => Some(read_be_u64(bytes, 0)?),
        _ => None,
    }
}

fn read_cells_tuple_64(bytes: &[u8], cells: u32, offset_cells: usize) -> Option<(u64, usize)> {
    let cells = cells as usize;
    if cells == 0 || cells > 2 {
        return None;
    }
    let offset = offset_cells.checked_mul(4)?;
    let end = offset.checked_add(cells * 4)?;
    if end > bytes.len() {
        return None;
    }
    let value = if cells == 1 {
        read_be_u32(bytes, offset)? as u64
    } else {
        ((read_be_u32(bytes, offset)? as u64) << 32) | read_be_u32(bytes, offset + 4)? as u64
    };
    Some((value, end))
}

fn read_cells_as_u64(bytes: &[u8], cells: usize) -> Option<u64> {
    read_cells_tuple_64(bytes, cells as u32, 0).map(|v| v.0)
}

fn read_cstr(bytes: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    if offset >= bytes.len() {
        return None;
    }
    let mut end = offset;
    while end < bytes.len() && bytes[end] != 0 {
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    Some((&bytes[offset..end], end + 1))
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes(raw.try_into().ok()?))
}

fn read_be_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let raw = bytes.get(offset..offset + 8)?;
    Some(u64::from_be_bytes(raw.try_into().ok()?))
}

const fn align_up_4(v: usize) -> usize {
    (v + 3) & !3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::std::vec::Vec;

    fn push_be_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn push_prop(out: &mut Vec<u8>, strings: &mut Vec<u8>, name: &str, data: &[u8]) {
        let name_off = strings.len() as u32;
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        push_be_u32(out, FDT_PROP);
        push_be_u32(out, data.len() as u32);
        push_be_u32(out, name_off);
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn push_begin_node(out: &mut Vec<u8>, name: &str) {
        push_be_u32(out, FDT_BEGIN_NODE);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn make_test_dtb() -> Vec<u8> {
        let mut struct_block = Vec::new();
        let mut strings = Vec::new();

        push_begin_node(&mut struct_block, "");
        push_prop(
            &mut struct_block,
            &mut strings,
            "#address-cells",
            &2u32.to_be_bytes(),
        );
        push_prop(
            &mut struct_block,
            &mut strings,
            "#size-cells",
            &2u32.to_be_bytes(),
        );

        push_begin_node(&mut struct_block, "memory@40000000");
        let mut mem_reg = Vec::new();
        mem_reg.extend_from_slice(&0u32.to_be_bytes());
        mem_reg.extend_from_slice(&0x4000_0000u32.to_be_bytes());
        mem_reg.extend_from_slice(&0u32.to_be_bytes());
        mem_reg.extend_from_slice(&0x4000_0000u32.to_be_bytes());
        push_prop(&mut struct_block, &mut strings, "reg", &mem_reg);
        push_be_u32(&mut struct_block, FDT_END_NODE);

        push_begin_node(&mut struct_block, "intc@8000000");
        push_prop(
            &mut struct_block,
            &mut strings,
            "compatible",
            b"arm,cortex-a15-gic\0arm,gic-400\0",
        );
        let mut gic_reg = Vec::new();
        gic_reg.extend_from_slice(&0u32.to_be_bytes());
        gic_reg.extend_from_slice(&0x0800_0000u32.to_be_bytes());
        gic_reg.extend_from_slice(&0u32.to_be_bytes());
        gic_reg.extend_from_slice(&0x1000u32.to_be_bytes());
        gic_reg.extend_from_slice(&0u32.to_be_bytes());
        gic_reg.extend_from_slice(&0x0801_0000u32.to_be_bytes());
        gic_reg.extend_from_slice(&0u32.to_be_bytes());
        gic_reg.extend_from_slice(&0x2000u32.to_be_bytes());
        push_prop(&mut struct_block, &mut strings, "reg", &gic_reg);
        push_be_u32(&mut struct_block, FDT_END_NODE);

        push_begin_node(&mut struct_block, "chosen");
        push_prop(
            &mut struct_block,
            &mut strings,
            "linux,initrd-start",
            &0x4800_0000u64.to_be_bytes(),
        );
        push_prop(
            &mut struct_block,
            &mut strings,
            "linux,initrd-end",
            &0x4810_0000u64.to_be_bytes(),
        );
        push_be_u32(&mut struct_block, FDT_END_NODE);
        push_be_u32(&mut struct_block, FDT_END_NODE);
        push_be_u32(&mut struct_block, FDT_END);

        let header_size = 40usize;
        let off_struct = header_size;
        let off_strings = off_struct + struct_block.len();
        let total = off_strings + strings.len();
        let mut dtb = Vec::new();
        push_be_u32(&mut dtb, FDT_MAGIC);
        push_be_u32(&mut dtb, total as u32);
        push_be_u32(&mut dtb, off_struct as u32);
        push_be_u32(&mut dtb, off_strings as u32);
        push_be_u32(&mut dtb, header_size as u32);
        push_be_u32(&mut dtb, 17);
        push_be_u32(&mut dtb, 16);
        push_be_u32(&mut dtb, 0);
        push_be_u32(&mut dtb, strings.len() as u32);
        push_be_u32(&mut dtb, struct_block.len() as u32);
        dtb.extend_from_slice(&struct_block);
        dtb.extend_from_slice(&strings);
        dtb
    }

    #[test]
    fn parse_boot_dtb_extracts_memory_initrd_and_gic() {
        let dtb = make_test_dtb();
        let parsed = parse_boot_dtb(&dtb).expect("parsed");
        assert_eq!(parsed.memory_start, Some(0x4000_0000));
        assert_eq!(parsed.memory_len, Some(0x4000_0000));
        assert_eq!(parsed.initrd_start, Some(0x4800_0000));
        assert_eq!(parsed.initrd_end, Some(0x4810_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }
}
