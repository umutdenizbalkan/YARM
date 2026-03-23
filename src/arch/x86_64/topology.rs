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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_madt_lapics() {
        let madt = b"LAPIC apic_id=0 enabled=1\nLAPIC apic_id=1 enabled=1\nLAPIC apic_id=2 enabled=0\nAPIC cpu=3 enabled=1";
        assert_eq!(discover_present_cpu_bitmap(madt), 0b1011);
    }
}
