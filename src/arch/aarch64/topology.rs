pub fn default_present_cpu_bitmap() -> u64 {
    0b11
}

pub fn discover_present_cpu_bitmap(dtb: &[u8]) -> u64 {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_dtb_cpu_nodes() {
        let dtb = b"/cpus {\n cpu@0 { };\n cpu@2 { };\n};";
        assert_eq!(discover_present_cpu_bitmap(dtb), 0b101);
    }
}
