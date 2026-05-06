// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub fn default_present_cpu_bitmap() -> u64 {
    bitmap_from_logical_count(detect_logical_cpu_count_cpuid())
}

fn bitmap_from_logical_count(raw_count: u32) -> u64 {
    let count = raw_count.clamp(1, 64);
    if count >= 64 {
        u64::MAX
    } else {
        (1u64 << count) - 1
    }
}

fn detect_logical_cpu_count_cpuid() -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        #[allow(unused_unsafe)]
        let max_leaf = unsafe { core::arch::x86_64::__cpuid(0).eax };

        if max_leaf >= 0xB {
            #[allow(unused_unsafe)]
            let level1 = unsafe { core::arch::x86_64::__cpuid_count(0xB, 1) };
            let logical = level1.ebx & 0xFFFF;
            if logical != 0 {
                return logical;
            }
        }

        if max_leaf >= 1 {
            #[allow(unused_unsafe)]
            let leaf1 = unsafe { core::arch::x86_64::__cpuid(1) };
            let logical = (leaf1.ebx >> 16) & 0xFF;
            if logical != 0 {
                return logical;
            }
        }
    }

    1
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


pub fn discover_cpu_apic_ids(madt_or_apic: &[u8]) -> [Option<u8>; crate::kernel::scheduler::MAX_CPUS] {
    let text = core::str::from_utf8(madt_or_apic).unwrap_or("");
    let mut out = [None; crate::kernel::scheduler::MAX_CPUS];
    let mut next_logical = 1u8;
    out[crate::arch::platform_constants::BOOTSTRAP_CPU_ID as usize] =
        Some(crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
    for line in text.lines() {
        let line = line.trim();
        if !(line.contains("LAPIC") || line.contains("APIC")) || line.contains("enabled=0") { continue; }
        let apic = line.split_whitespace().find(|p| p.starts_with("apic_id=") || p.starts_with("cpu=")).and_then(|p| p.split('=').nth(1)).and_then(|v| v.parse::<u8>().ok());
        let is_bsp = line.contains("bsp=1") || apic == Some(crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
        if let Some(apic_id) = apic {
            let logical = if is_bsp { crate::arch::platform_constants::BOOTSTRAP_CPU_ID } else { let l=next_logical; next_logical = next_logical.saturating_add(1); l };
            if (logical as usize) < out.len() { out[logical as usize] = Some(apic_id); }
            crate::yarm_log!("YARM_CPU_ENUM logical={} apic_id={} bsp={} enabled=1", logical, apic_id, if is_bsp {1} else {0});
        }
    }
    out
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

    #[test]
    fn bitmap_from_logical_count_clamps_and_sets_bits() {
        assert_eq!(bitmap_from_logical_count(0), 0b1);
        assert_eq!(bitmap_from_logical_count(1), 0b1);
        assert_eq!(bitmap_from_logical_count(2), 0b11);
        assert_eq!(bitmap_from_logical_count(4), 0b1111);
        assert_eq!(bitmap_from_logical_count(64), u64::MAX);
        assert_eq!(bitmap_from_logical_count(128), u64::MAX);
    }
}
