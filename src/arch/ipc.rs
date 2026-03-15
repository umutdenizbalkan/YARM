/// Portable register-lane mapping for small IPC payloads.
///
/// The kernel uses this abstraction to avoid embedding ISA-specific register
/// details in core IPC logic. ISAs can override/extend this module later.
pub const IPC_REGISTER_WORDS: usize = 2;
pub const IPC_REGISTER_BYTES: usize = IPC_REGISTER_WORDS * core::mem::size_of::<usize>();

pub fn unpack_register_payload(
    words: [usize; IPC_REGISTER_WORDS],
    len: usize,
) -> Option<[u8; IPC_REGISTER_BYTES]> {
    if len > IPC_REGISTER_BYTES {
        return None;
    }

    let mut out = [0u8; IPC_REGISTER_BYTES];
    let mut i = 0;
    while i < IPC_REGISTER_WORDS {
        let bytes = words[i].to_le_bytes();
        let start = i * core::mem::size_of::<usize>();
        let end = start + core::mem::size_of::<usize>();
        out[start..end].copy_from_slice(&bytes);
        i += 1;
    }
    Some(out)
}

pub fn pack_register_payload(payload: &[u8]) -> [usize; IPC_REGISTER_WORDS] {
    let mut words = [0usize; IPC_REGISTER_WORDS];
    let mut i = 0;
    while i < IPC_REGISTER_WORDS {
        let start = i * core::mem::size_of::<usize>();
        let end = start + core::mem::size_of::<usize>();
        let mut lane = [0u8; core::mem::size_of::<usize>()];
        if start < payload.len() {
            let copy_end = core::cmp::min(end, payload.len());
            lane[..copy_end - start].copy_from_slice(&payload[start..copy_end]);
        }
        words[i] = usize::from_le_bytes(lane);
        i += 1;
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_payload_roundtrip() {
        let source = [0xAAu8; IPC_REGISTER_BYTES];
        let words = pack_register_payload(&source);
        let decoded = unpack_register_payload(words, IPC_REGISTER_BYTES).expect("decode");
        assert_eq!(decoded, source);
    }
}
