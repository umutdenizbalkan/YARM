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
}
