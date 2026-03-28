pub fn parse_usize_token(description: &[u8], key: &str) -> Option<usize> {
    let text = core::str::from_utf8(description).ok()?;
    for token in text.split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ',' | ';')) {
        if token.is_empty() {
            continue;
        }
        let (lhs, rhs) = token.split_once('=')?;
        if lhs != key {
            continue;
        }
        if let Some(hex) = rhs.strip_prefix("0x").or_else(|| rhs.strip_prefix("0X")) {
            return usize::from_str_radix(hex, 16).ok();
        }
        if let Ok(value) = rhs.parse::<usize>() {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usize_token_supports_hex_decimal_and_separators() {
        let description = b"lapic_mmio_base=0xfee00000, plic_smode_context=1; gic_cpu_if_base=42";
        assert_eq!(
            parse_usize_token(description, "lapic_mmio_base"),
            Some(0xFEE0_0000)
        );
        assert_eq!(
            parse_usize_token(description, "plic_smode_context"),
            Some(1)
        );
        assert_eq!(parse_usize_token(description, "gic_cpu_if_base"), Some(42));
    }
}
