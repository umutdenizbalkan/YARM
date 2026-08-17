// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const FDT_MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 0x1;
const FDT_END_NODE: u32 = 0x2;
const FDT_PROP: u32 = 0x3;
const FDT_NOP: u32 = 0x4;
const FDT_END: u32 = 0x9;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PsciConduit {
    #[default]
    Unknown,
    Smc,
    Hvc,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParsedDtb {
    pub memory_start: Option<u64>,
    pub memory_len: Option<u64>,
    pub initrd_start: Option<u64>,
    pub initrd_end: Option<u64>,
    pub gic_cpu_if_base: Option<usize>,
    /// GICv2 DISTRIBUTOR base — the FIRST `reg` tuple of the interrupt-controller node.
    ///
    /// Derived alongside `gic_cpu_if_base` and committed only together with it, so the two can
    /// never disagree about which tuple is which. Carried for later use; this checkpoint performs
    /// no distributor MMIO.
    pub gic_dist_base: Option<usize>,
    pub present_cpu_bitmap: Option<u64>,
    pub psci_conduit: PsciConduit,
}

/// Node-local scratch for ONE candidate interrupt-controller node.
///
/// The GIC bases cannot be decided when `reg` is seen, because the device tree does not order
/// properties: QEMU `virt` emits `reg` BEFORE `compatible`, so a decision taken at `reg` runs
/// with the compatible string still unknown. That is precisely how the distributor base
/// (`0x0800_0000`, the first tuple) came to be stored as the CPU interface. Both properties are
/// therefore recorded here and the mapping is resolved at the node's `FDT_END_NODE`, when both
/// have had their chance to appear.
///
/// `depth` scopes the scratch to the node that opened it, so a child node's `reg`/`compatible`
/// (QEMU nests an `arm,gic-v2m-frame` MSI node inside `intc`) cannot contaminate the parent's
/// decision. Fixed-size and `Copy`: no allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GicNodeScratch {
    /// Tree depth of the node that opened this scratch; only properties at this exact depth
    /// belong to it.
    depth: usize,
    /// A supported GICv2 `compatible` string was seen on this node.
    supported_gicv2: bool,
    /// First `reg` tuple — `(base, size)`, the distributor under the GICv2 layout.
    first: Option<(u64, u64)>,
    /// Second `reg` tuple — `(base, size)`, the CPU interface under the GICv2 layout.
    second: Option<(u64, u64)>,
}

/// `compatible` strings whose `reg` layout is known to be
/// `<dist_base dist_size cpu_if_base cpu_if_size>`.
///
/// GICv3 and unknown controllers are deliberately absent: their second tuple is a
/// redistributor, not a CPU interface, so applying this mapping to them would publish a wrong
/// base rather than none. An unrecognized controller yields no GIC bases at all.
fn compatible_is_supported_gicv2(prop_data: &[u8]) -> bool {
    prop_data
        .split(|b| *b == 0)
        .any(|part| part == b"arm,cortex-a15-gic" || part == b"arm,gic-400")
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
    let mut inside_psci = false;
    let mut gic_scratch: Option<GicNodeScratch> = None;
    let mut present_cpu_bitmap = 0u64;

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
                inside_psci = name.starts_with(b"psci");
                // Open scratch for a candidate controller only while nothing has been committed
                // yet (first-valid-controller selection) and only when no scratch is already
                // open — a nested controller-shaped child must not displace its parent.
                if gic_scratch.is_none()
                    && out.gic_cpu_if_base.is_none()
                    && (name.starts_with(b"intc") || name.starts_with(b"interrupt-controller"))
                {
                    gic_scratch = Some(GicNodeScratch {
                        depth,
                        ..GicNodeScratch::default()
                    });
                }
                if let Some(cpu_id) = parse_cpu_id_from_node_name(name) {
                    present_cpu_bitmap |= 1u64 << cpu_id;
                }
            }
            FDT_END_NODE => {
                let closing_depth = depth;
                depth = depth.saturating_sub(1);
                inside_memory = false;
                inside_chosen = false;
                inside_psci = false;
                // The decision point: both `compatible` and `reg` have now had their chance to
                // appear, in either order. Commit only a COMPLETE supported GICv2 layout — a
                // recognized compatible AND both tuples — and commit both bases together.
                // Anything else (GICv3, unknown compatible, a single-`reg` node, a truncated
                // second tuple) commits nothing and lets a later controller node try.
                if let Some(scratch) = gic_scratch
                    && scratch.depth == closing_depth
                {
                    if let (true, Some(dist), Some(cpu_if)) =
                        (scratch.supported_gicv2, scratch.first, scratch.second)
                    {
                        out.gic_dist_base = Some(dist.0 as usize);
                        out.gic_cpu_if_base = Some(cpu_if.0 as usize);
                    }
                    gic_scratch = None;
                }
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
                } else if gic_scratch.is_some_and(|s| s.depth == depth)
                    && (prop_name == b"compatible" || prop_name == b"reg")
                {
                    // Record only; the mapping is resolved at FDT_END_NODE. The address/size-cell
                    // handling and tuple offsets are unchanged — the root's `#address-cells` /
                    // `#size-cells` still govern, and the second tuple still begins one full
                    // `(address + size)` span in.
                    let scratch = gic_scratch.as_mut().expect("checked Some");
                    if prop_name == b"compatible" {
                        scratch.supported_gicv2 = compatible_is_supported_gicv2(prop_data);
                    } else {
                        let cell_span = (address_cells + size_cells) as usize;
                        if cell_span > 0 {
                            scratch.first = read_cells_tuple_64(prop_data, address_cells, 0)
                                .zip(read_cells_tuple_64(
                                    prop_data,
                                    size_cells,
                                    address_cells as usize,
                                ))
                                .map(|(base, size)| (base.0, size.0));
                            scratch.second =
                                read_cells_tuple_64(prop_data, address_cells, cell_span)
                                    .zip(read_cells_tuple_64(
                                        prop_data,
                                        size_cells,
                                        cell_span + address_cells as usize,
                                    ))
                                    .map(|(base, size)| (base.0, size.0));
                        }
                    }
                } else if inside_psci && prop_name == b"method" {
                    out.psci_conduit = if prop_data.starts_with(b"hvc") {
                        PsciConduit::Hvc
                    } else if prop_data.starts_with(b"smc") {
                        PsciConduit::Smc
                    } else {
                        PsciConduit::Unknown
                    };
                }
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => return None,
        }
    }
    if present_cpu_bitmap != 0 {
        out.present_cpu_bitmap = Some(present_cpu_bitmap);
    }
    Some(out)
}

fn parse_cpu_id_from_node_name(name: &[u8]) -> Option<u8> {
    let suffix = name.strip_prefix(b"cpu@")?;
    let mut value = 0u64;
    let mut consumed = 0usize;
    for byte in suffix.iter().copied() {
        let nibble = match byte {
            b'0'..=b'9' => (byte - b'0') as u64,
            b'a'..=b'f' => (byte - b'a' + 10) as u64,
            b'A'..=b'F' => (byte - b'A' + 10) as u64,
            _ => break,
        };
        value = (value << 4) | nibble;
        consumed += 1;
    }
    if consumed == 0 || value >= 64 {
        return None;
    }
    Some(value as u8)
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

        push_begin_node(&mut struct_block, "psci");
        push_prop(&mut struct_block, &mut strings, "method", b"hvc\0");
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
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.present_cpu_bitmap, None);
        assert_eq!(parsed.psci_conduit, PsciConduit::Hvc);
    }

    // ── Canonical 199E prerequisite: property-order-independent GICv2 base derivation ─────────
    //
    // The device tree does not order properties, and QEMU `virt` emits `reg` BEFORE `compatible`.
    // Deciding the tuple mapping at `reg` therefore ran with the compatible string still unknown
    // and stored the DISTRIBUTOR (`0x0800_0000`, the first tuple) as the CPU interface. These
    // fixtures pin the decision to end-of-node, where both orders agree.

    /// One `(base, size)` pair as `address_cells`/`size_cells` big-endian cells (1 or 2 cells).
    fn push_tuple(out: &mut Vec<u8>, cells: u32, base: u64, size: u64) {
        for value in [base, size] {
            if cells == 1 {
                out.extend_from_slice(&(value as u32).to_be_bytes());
            } else {
                out.extend_from_slice(&value.to_be_bytes());
            }
        }
    }

    struct GicNodeSpec {
        name: &'static str,
        compatible: &'static [u8],
        /// `(base, size)` tuples in `reg` order.
        tuples: Vec<(u64, u64)>,
        /// Emit `reg` before `compatible` — the real QEMU ordering.
        reg_first: bool,
        /// Emit a nested controller-shaped child carrying its own conflicting `reg`/`compatible`.
        nested_child: bool,
    }

    impl GicNodeSpec {
        fn qemu_like(name: &'static str) -> Self {
            Self {
                name,
                compatible: b"arm,cortex-a15-gic\0",
                tuples: Vec::from([(0x0800_0000, 0x1_0000), (0x0801_0000, 0x1_0000)]),
                reg_first: true,
                nested_child: false,
            }
        }
    }

    /// Build a DTB whose root declares `cells`-wide address/size and which contains `nodes`.
    fn make_gic_dtb(cells: u32, nodes: &[GicNodeSpec]) -> Vec<u8> {
        let mut struct_block = Vec::new();
        let mut strings = Vec::new();
        push_begin_node(&mut struct_block, "");
        push_prop(
            &mut struct_block,
            &mut strings,
            "#address-cells",
            &cells.to_be_bytes(),
        );
        push_prop(
            &mut struct_block,
            &mut strings,
            "#size-cells",
            &cells.to_be_bytes(),
        );
        for node in nodes {
            push_begin_node(&mut struct_block, node.name);
            let mut reg = Vec::new();
            for (base, size) in &node.tuples {
                push_tuple(&mut reg, cells, *base, *size);
            }
            let emit_reg = |sb: &mut Vec<u8>, st: &mut Vec<u8>| {
                push_prop(sb, st, "reg", &reg);
            };
            let emit_compatible = |sb: &mut Vec<u8>, st: &mut Vec<u8>| {
                push_prop(sb, st, "compatible", node.compatible);
            };
            if node.reg_first {
                emit_reg(&mut struct_block, &mut strings);
                emit_compatible(&mut struct_block, &mut strings);
            } else {
                emit_compatible(&mut struct_block, &mut strings);
                emit_reg(&mut struct_block, &mut strings);
            }
            if node.nested_child {
                // A controller-shaped CHILD with a conflicting layout, exactly as QEMU nests its
                // `arm,gic-v2m-frame` MSI node inside `intc`.
                push_begin_node(&mut struct_block, "v2m@8020000");
                let mut child_reg = Vec::new();
                push_tuple(&mut child_reg, cells, 0x0802_0000, 0x1000);
                push_prop(&mut struct_block, &mut strings, "reg", &child_reg);
                push_prop(
                    &mut struct_block,
                    &mut strings,
                    "compatible",
                    b"arm,gic-v2m-frame\0",
                );
                push_be_u32(&mut struct_block, FDT_END_NODE);
            }
            push_be_u32(&mut struct_block, FDT_END_NODE);
        }
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

    fn parse_gic(cells: u32, nodes: &[GicNodeSpec]) -> ParsedDtb {
        parse_boot_dtb(&make_gic_dtb(cells, nodes)).expect("parsed")
    }

    /// THE REGRESSION: `reg` before `compatible`, exactly as QEMU `virt` emits it. Before the
    /// deferred decision this produced `gic_cpu_if_base = 0x0800_0000` — the distributor.
    #[test]
    fn gic_reg_before_compatible_maps_both_tuples_correctly() {
        let parsed = parse_gic(2, &[GicNodeSpec::qemu_like("intc@8000000")]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }

    /// The other order must agree exactly.
    #[test]
    fn gic_compatible_before_reg_maps_both_tuples_correctly() {
        let mut spec = GicNodeSpec::qemu_like("intc@8000000");
        spec.reg_first = false;
        let parsed = parse_gic(2, &[spec]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }

    /// Multiple NUL-separated compatible strings: matching any supported entry is enough.
    #[test]
    fn gic_multiple_nul_separated_compatibles_match() {
        let mut spec = GicNodeSpec::qemu_like("intc@8000000");
        spec.compatible = b"qemu,unknown-gic\0arm,gic-400\0";
        let parsed = parse_gic(2, &[spec]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }

    /// GICv3 must NOT get the GICv2 mapping: its second tuple is a redistributor, not a CPU
    /// interface, so publishing it would be worse than publishing nothing.
    #[test]
    fn gicv3_compatible_yields_no_bases() {
        let mut spec = GicNodeSpec::qemu_like("intc@8000000");
        spec.compatible = b"arm,gic-v3\0";
        let parsed = parse_gic(2, &[spec]);
        assert_eq!(parsed.gic_dist_base, None);
        assert_eq!(parsed.gic_cpu_if_base, None);
    }

    #[test]
    fn unknown_compatible_yields_no_bases() {
        let mut spec = GicNodeSpec::qemu_like("intc@8000000");
        spec.compatible = b"vendor,mystery-intc\0";
        let parsed = parse_gic(2, &[spec]);
        assert_eq!(parsed.gic_dist_base, None);
        assert_eq!(parsed.gic_cpu_if_base, None);
    }

    /// A single-`reg` node has no CPU interface; committing the distributor as one is the exact
    /// defect this checkpoint removes, so nothing is committed.
    #[test]
    fn missing_second_tuple_yields_no_bases() {
        let mut spec = GicNodeSpec::qemu_like("intc@8000000");
        spec.tuples = Vec::from([(0x0800_0000, 0x1_0000)]);
        let parsed = parse_gic(2, &[spec]);
        assert_eq!(parsed.gic_dist_base, None);
        assert_eq!(parsed.gic_cpu_if_base, None);
    }

    /// A truncated second tuple (base present, size cut off) is equally incomplete.
    #[test]
    fn truncated_second_tuple_yields_no_bases() {
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
        push_begin_node(&mut struct_block, "intc@8000000");
        let mut reg = Vec::new();
        push_tuple(&mut reg, 2, 0x0800_0000, 0x1_0000);
        reg.extend_from_slice(&0x0801_0000u64.to_be_bytes()); // base only — size missing
        push_prop(&mut struct_block, &mut strings, "reg", &reg);
        push_prop(
            &mut struct_block,
            &mut strings,
            "compatible",
            b"arm,cortex-a15-gic\0",
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
        let parsed = parse_boot_dtb(&dtb).expect("parsed");
        assert_eq!(parsed.gic_dist_base, None);
        assert_eq!(parsed.gic_cpu_if_base, None);
    }

    /// A nested controller-shaped child must not contaminate its parent's decision — the depth
    /// scope is what keeps the child's single `reg` out of the parent's tuples.
    #[test]
    fn nested_child_properties_do_not_contaminate_the_parent_node() {
        let mut spec = GicNodeSpec::qemu_like("intc@8000000");
        spec.nested_child = true;
        let parsed = parse_gic(2, &[spec]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }

    /// First VALID controller wins: an unsupported controller ahead of a GICv2 one must not
    /// consume the selection, and must not publish anything of its own.
    #[test]
    fn first_valid_controller_wins_across_multiple_nodes() {
        let mut unsupported = GicNodeSpec::qemu_like("intc@1000000");
        unsupported.compatible = b"arm,gic-v3\0";
        unsupported.tuples = Vec::from([(0x0100_0000, 0x1_0000), (0x0101_0000, 0x1_0000)]);
        let parsed = parse_gic(2, &[unsupported, GicNodeSpec::qemu_like("intc@8000000")]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }

    /// A second GICv2 controller must not overwrite the first one's committed bases.
    #[test]
    fn later_controller_does_not_overwrite_the_committed_bases() {
        let mut later = GicNodeSpec::qemu_like("intc@9000000");
        later.tuples = Vec::from([(0x0900_0000, 0x1_0000), (0x0901_0000, 0x1_0000)]);
        let parsed = parse_gic(2, &[GicNodeSpec::qemu_like("intc@8000000"), later]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }

    /// 32-bit address/size cells: the tuple offsets scale with the declared cell widths.
    #[test]
    fn single_cell_address_and_size_layout_maps_both_tuples() {
        let parsed = parse_gic(1, &[GicNodeSpec::qemu_like("intc@8000000")]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
    }

    /// The exact QEMU `virt` mapping, stated as the acceptance value.
    #[test]
    fn qemu_virt_mapping_is_dist_08000000_and_cpu_if_08010000() {
        let parsed = parse_gic(2, &[GicNodeSpec::qemu_like("intc@8000000")]);
        assert_eq!(parsed.gic_dist_base, Some(0x0800_0000));
        assert_eq!(parsed.gic_cpu_if_base, Some(0x0801_0000));
        // And the two are distinct — the defect made them the same value.
        assert_ne!(parsed.gic_dist_base, parsed.gic_cpu_if_base);
    }

    #[test]
    fn parse_boot_dtb_extracts_cpu_bitmap_from_cpu_nodes() {
        let mut struct_block = Vec::new();
        let strings: Vec<u8> = Vec::new();

        push_begin_node(&mut struct_block, "");
        push_begin_node(&mut struct_block, "cpus");
        push_begin_node(&mut struct_block, "cpu@0");
        push_be_u32(&mut struct_block, FDT_END_NODE);
        push_begin_node(&mut struct_block, "cpu@3");
        push_be_u32(&mut struct_block, FDT_END_NODE);
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

        let parsed = parse_boot_dtb(&dtb).expect("parsed");
        assert_eq!(parsed.present_cpu_bitmap, Some(0b1001));
        assert_eq!(parsed.psci_conduit, PsciConduit::Unknown);
    }
}
