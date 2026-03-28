pub fn default_present_cpu_bitmap() -> u64 {
    0b1111
}

pub fn discover_present_cpu_bitmap(madt_or_apic: &[u8]) -> u64 {
    let text = core::str::from_utf8(madt_or_apic).unwrap_or("");
    let mut bitmap = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if !(line.contains("LAPIC") || line.contains("APIC")) {
            continue;
        }
        if line.contains("enabled=0") {
            continue;
        }
        if let Some(id_field) = line
            .split_whitespace()
            .find(|part| part.starts_with("apic_id=") || part.starts_with("cpu="))
        {
            let raw = id_field.split('=').nth(1).unwrap_or("");
            match raw.parse::<u8>() {
                Ok(cpu) if cpu < 64 => bitmap |= 1u64 << cpu,
                Ok(cpu) => crate::pr_warn!("x86 topology: cpu id {} exceeds bitmap width", cpu),
                Err(_) => crate::pr_warn!("x86 topology: malformed apic_id/cpu field '{}'", raw),
            }
        } else {
            crate::pr_warn!(
                "x86 topology: LAPIC/APIC entry missing apic_id/cpu field: {}",
                line
            );
        }
    }
    if bitmap == 0 {
        default_present_cpu_bitmap()
    } else {
        bitmap
    }
}

pub fn discover_irq_controller_description(madt_or_apic: &[u8], out: &mut [u8]) -> Option<usize> {
    let base = crate::arch::irq_description::parse_usize_token(madt_or_apic, "lapic_mmio_base")
        .or_else(|| crate::arch::irq_description::parse_usize_token(madt_or_apic, "lapic_base"))
        .or_else(|| crate::arch::irq_description::parse_usize_token(madt_or_apic, "apic_base"))
        .or_else(|| crate::arch::irq_description::parse_usize_token(madt_or_apic, "LAPIC_BASE"))?;
    let mut len = 0usize;
    for byte in b"lapic_mmio_base=0x" {
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
        let nibble = ((base >> shift) & 0xF) as u8;
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
    Some(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_madt_lapics() {
        let madt = b"LAPIC apic_id=0 enabled=1\nLAPIC apic_id=1 enabled=1\nLAPIC apic_id=2 enabled=0\nAPIC cpu=3 enabled=1";
        assert_eq!(discover_present_cpu_bitmap(madt), 0b1011);
    }

    #[test]
    fn discovers_lapic_description_from_alias_key() {
        let mut out = [0u8; 64];
        let len = discover_irq_controller_description(b"LAPIC_BASE=0xfee00000", &mut out)
            .expect("x86 description should parse");
        let text = core::str::from_utf8(&out[..len]).expect("valid utf8");
        assert_eq!(text, "lapic_mmio_base=0xfee00000");
    }
}
