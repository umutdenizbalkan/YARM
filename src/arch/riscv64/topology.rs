// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub fn default_present_cpu_bitmap() -> u64 {
    // Without a valid DTB CPU map, use a conservative single-CPU fallback.
    0b1
}

pub fn discover_present_cpu_bitmap(dtb: &[u8]) -> u64 {
    // Prefer the binary FDT walker (the real DTB encoding); fall back to the
    // legacy textual "/cpus { cpu@N { } }" scan only if the FDT walker yields
    // an empty bitmap. Returning the conservative single-CPU default keeps
    // the boot path working when no usable topology is present.
    if let Some(bitmap) = crate::arch::fdt::cpus_hart_id_bitmap(dtb) {
        if bitmap != 0 {
            return bitmap;
        }
    }
    let text = core::str::from_utf8(dtb).unwrap_or("");
    let mut inside = false;
    let mut bitmap = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("/cpus") {
            inside = true;
            continue;
        }
        if inside && line.starts_with('}') {
            break;
        }
        if inside && line.starts_with("cpu@") {
            let id = line[4..]
                .split(|c: char| !c.is_ascii_hexdigit())
                .next()
                .unwrap_or("");
            if let Ok(cpu) = u8::from_str_radix(id, 16) {
                if cpu < 64 {
                    bitmap |= 1u64 << cpu;
                }
            }
        }
    }
    if bitmap == 0 {
        default_present_cpu_bitmap()
    } else {
        bitmap
    }
}

pub fn discover_irq_controller_description(dtb: &[u8], out: &mut [u8]) -> Option<usize> {
    let base = crate::arch::irq_description::parse_usize_token(dtb, "plic_mmio_base")
        .or_else(|| crate::arch::irq_description::parse_usize_token(dtb, "plic_base"))
        .or_else(|| crate::arch::irq_description::parse_usize_token(dtb, "PLIC_BASE"))?;
    let context = crate::arch::irq_description::parse_usize_token(dtb, "plic_smode_context")
        .or_else(|| crate::arch::irq_description::parse_usize_token(dtb, "plic_context"))
        .or_else(|| crate::arch::irq_description::parse_usize_token(dtb, "PLIC_CONTEXT"))?;
    let canonical = [
        ("plic_mmio_base=0x", base),
        (" plic_smode_context=0x", context),
    ];
    let mut len = 0usize;
    for (prefix, value) in canonical {
        for byte in prefix.as_bytes() {
            if len >= out.len() {
                return None;
            }
            out[len] = *byte;
            len += 1;
        }
        let mut started = false;
        for shift in (0..=(core::mem::size_of::<usize>() * 8 - 4))
            .rev()
            .step_by(4)
        {
            let nibble = ((value >> shift) & 0xF) as u8;
            if nibble == 0 && !started && shift != 0 {
                continue;
            }
            started = true;
            if len >= out.len() {
                return None;
            }
            out[len] = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            };
            len += 1;
        }
    }
    Some(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_dtb_cpu_nodes() {
        let dtb = b"/cpus {\n cpu@0 { };\n cpu@1 { };\n cpu@3 { };\n};";
        assert_eq!(discover_present_cpu_bitmap(dtb), 0b1011);
    }

    #[test]
    fn default_present_cpu_bitmap_is_single_hart() {
        assert_eq!(default_present_cpu_bitmap(), 0b1);
    }

    #[test]
    fn discover_falls_back_to_default_for_empty_blob() {
        assert_eq!(
            discover_present_cpu_bitmap(b""),
            default_present_cpu_bitmap()
        );
    }

    #[test]
    fn discovers_plic_description_from_alias_keys() {
        let mut out = [0u8; 80];
        let len =
            discover_irq_controller_description(b"PLIC_BASE=0x0c000000 PLIC_CONTEXT=0x1", &mut out)
                .expect("riscv description should parse");
        let text = core::str::from_utf8(&out[..len]).expect("valid utf8");
        assert_eq!(text, "plic_mmio_base=0xc000000 plic_smode_context=0x1");
    }
}
