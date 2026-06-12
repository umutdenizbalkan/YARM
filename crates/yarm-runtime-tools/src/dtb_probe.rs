// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Bounds-checked Flattened Device Tree inspection for the hosted `dtb_probe`
//! command. This parser is intentionally separate from kernel DTB handling.

use std::collections::BTreeMap;
use std::fmt::Write as _;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
const HEADER_LEN: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError(String);

impl ProbeError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProbeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdtHeader {
    pub total_size: u32,
    pub off_dt_struct: u32,
    pub off_dt_strings: u32,
    pub off_mem_rsvmap: u32,
    pub version: u32,
    pub last_comp_version: u32,
    pub boot_cpuid_phys: u32,
    pub size_dt_strings: u32,
    pub size_dt_struct: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub path: String,
    pub name: String,
    pub properties: Vec<Property>,
    /// Cell widths inherited from the parent and therefore used by `reg`.
    pub parent_address_cells: u32,
    pub parent_size_cells: u32,
    /// Cell widths this node provides to its children.
    pub address_cells: u32,
    pub size_cells: u32,
}

impl Node {
    pub fn property(&self, name: &str) -> Option<&[u8]> {
        self.properties
            .iter()
            .find(|property| property.name == name)
            .map(|property| property.value.as_slice())
    }

    pub fn has_property(&self, name: &str) -> bool {
        self.property(name).is_some()
    }

    pub fn compatible(&self) -> Vec<String> {
        self.property("compatible")
            .map(decode_string_list)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFdt {
    pub header: FdtHeader,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegRange {
    pub address: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdoutPath<'a> {
    pub raw: String,
    pub device: String,
    pub options: Option<String>,
    pub node: Option<&'a Node>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    QemuVirt,
    Rpi5Bcm2712,
    Unknown,
}

impl Platform {
    fn label(self) -> &'static str {
        match self {
            Self::QemuVirt => "qemu-virt",
            Self::Rpi5Bcm2712 => "rpi5-bcm2712",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StackFrame {
    node_index: usize,
}

pub fn parse_fdt(bytes: &[u8]) -> Result<ParsedFdt, ProbeError> {
    let header = parse_header(bytes)?;
    let total_size = header.total_size as usize;
    let struct_block = checked_block(
        bytes,
        header.off_dt_struct as usize,
        header.size_dt_struct as usize,
        total_size,
        "structure",
    )?;
    let strings = checked_block(
        bytes,
        header.off_dt_strings as usize,
        header.size_dt_strings as usize,
        total_size,
        "strings",
    )?;

    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<StackFrame> = Vec::new();
    let mut cursor = 0usize;
    let mut saw_end = false;

    while cursor < struct_block.len() {
        let token = read_be_u32_at(struct_block, cursor)?;
        cursor = checked_add(cursor, 4, "structure token")?;
        match token {
            FDT_BEGIN_NODE => {
                let (raw_name, next) = read_cstr(struct_block, cursor, "node name")?;
                cursor = align4(next)?;
                let name = String::from_utf8_lossy(raw_name).into_owned();
                let (parent_address_cells, parent_size_cells, path) =
                    if let Some(parent) = stack.last() {
                        let parent_node = &nodes[parent.node_index];
                        (
                            parent_node.address_cells,
                            parent_node.size_cells,
                            join_path(&parent_node.path, &name),
                        )
                    } else {
                        (2, 1, "/".to_string())
                    };
                let node_index = nodes.len();
                nodes.push(Node {
                    path,
                    name,
                    properties: Vec::new(),
                    parent_address_cells,
                    parent_size_cells,
                    address_cells: parent_address_cells,
                    size_cells: parent_size_cells,
                });
                stack.push(StackFrame { node_index });
            }
            FDT_END_NODE => {
                if stack.pop().is_none() {
                    return Err(ProbeError::new("FDT_END_NODE without an open node"));
                }
            }
            FDT_PROP => {
                let length = read_be_u32_at(struct_block, cursor)? as usize;
                let name_offset =
                    read_be_u32_at(struct_block, checked_add(cursor, 4, "property header")?)?
                        as usize;
                cursor = checked_add(cursor, 8, "property header")?;
                let end = checked_add(cursor, length, "property value")?;
                let value = struct_block.get(cursor..end).ok_or_else(|| {
                    ProbeError::new("property value extends past structure block")
                })?;
                cursor = align4(end)?;
                if cursor > struct_block.len() {
                    return Err(ProbeError::new(
                        "property padding extends past structure block",
                    ));
                }
                let (raw_name, _) = read_cstr(strings, name_offset, "property name")?;
                let name = std::str::from_utf8(raw_name)
                    .map_err(|_| ProbeError::new("property name is not UTF-8"))?
                    .to_string();
                let frame = stack
                    .last()
                    .ok_or_else(|| ProbeError::new("property found outside a node"))?;
                let node = &mut nodes[frame.node_index];
                if name == "#address-cells" {
                    node.address_cells = decode_single_cell(value, "#address-cells")?;
                } else if name == "#size-cells" {
                    node.size_cells = decode_single_cell(value, "#size-cells")?;
                }
                node.properties.push(Property {
                    name,
                    value: value.to_vec(),
                });
            }
            FDT_NOP => {}
            FDT_END => {
                if !stack.is_empty() {
                    return Err(ProbeError::new("FDT_END encountered with unclosed nodes"));
                }
                saw_end = true;
                break;
            }
            other => {
                return Err(ProbeError::new(format!(
                    "unknown structure token 0x{other:08x}"
                )));
            }
        }
    }

    if !saw_end {
        return Err(ProbeError::new("structure block has no FDT_END token"));
    }
    if nodes.first().is_none_or(|node| node.path != "/") {
        return Err(ProbeError::new("structure block has no root node"));
    }
    Ok(ParsedFdt { header, nodes })
}

pub fn parse_header(bytes: &[u8]) -> Result<FdtHeader, ProbeError> {
    if bytes.len() < HEADER_LEN {
        return Err(ProbeError::new(format!(
            "file is too short for an FDT header: {} bytes",
            bytes.len()
        )));
    }
    let magic = read_be_u32_at(bytes, 0)?;
    if magic != FDT_MAGIC {
        return Err(ProbeError::new(format!(
            "invalid FDT magic 0x{magic:08x}; expected 0x{FDT_MAGIC:08x}"
        )));
    }
    let header = FdtHeader {
        total_size: read_be_u32_at(bytes, 4)?,
        off_dt_struct: read_be_u32_at(bytes, 8)?,
        off_dt_strings: read_be_u32_at(bytes, 12)?,
        off_mem_rsvmap: read_be_u32_at(bytes, 16)?,
        version: read_be_u32_at(bytes, 20)?,
        last_comp_version: read_be_u32_at(bytes, 24)?,
        boot_cpuid_phys: read_be_u32_at(bytes, 28)?,
        size_dt_strings: read_be_u32_at(bytes, 32)?,
        size_dt_struct: read_be_u32_at(bytes, 36)?,
    };
    if header.total_size as usize > bytes.len() {
        return Err(ProbeError::new(format!(
            "FDT total size {} exceeds file length {}",
            header.total_size,
            bytes.len()
        )));
    }
    if (header.total_size as usize) < HEADER_LEN {
        return Err(ProbeError::new("FDT total size is smaller than its header"));
    }
    Ok(header)
}

pub fn decode_be_cells(bytes: &[u8]) -> Result<Vec<u32>, ProbeError> {
    if bytes.len() % 4 != 0 {
        return Err(ProbeError::new(format!(
            "cell array length {} is not a multiple of four",
            bytes.len()
        )));
    }
    bytes
        .chunks_exact(4)
        .map(|chunk| Ok(u32::from_be_bytes(chunk.try_into().expect("exact chunk"))))
        .collect()
}

pub fn decode_string(value: &[u8]) -> Option<String> {
    let end = value
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(value.len());
    std::str::from_utf8(&value[..end]).ok().map(str::to_string)
}

pub fn decode_string_list(value: &[u8]) -> Vec<String> {
    value
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok().map(str::to_string))
        .collect()
}

pub fn parse_reg_ranges(
    value: &[u8],
    address_cells: u32,
    size_cells: u32,
) -> Result<Vec<RegRange>, ProbeError> {
    if address_cells > 2 || size_cells > 2 {
        return Err(ProbeError::new(format!(
            "reg uses unsupported cell widths {address_cells}/{size_cells}; raw cells retained"
        )));
    }
    let tuple_cells = address_cells
        .checked_add(size_cells)
        .ok_or_else(|| ProbeError::new("reg tuple cell count overflow"))?
        as usize;
    if tuple_cells == 0 {
        return Err(ProbeError::new("reg tuple has zero cells"));
    }
    let cells = decode_be_cells(value)?;
    if cells.len() % tuple_cells != 0 {
        return Err(ProbeError::new(format!(
            "reg has {} cells, not a multiple of tuple width {}",
            cells.len(),
            tuple_cells
        )));
    }
    let mut ranges = Vec::new();
    for tuple in cells.chunks_exact(tuple_cells) {
        ranges.push(RegRange {
            address: cells_to_u64(&tuple[..address_cells as usize])?,
            size: cells_to_u64(&tuple[address_cells as usize..])?,
        });
    }
    Ok(ranges)
}

pub fn aliases(parsed: &ParsedFdt) -> BTreeMap<String, String> {
    node(parsed, "/aliases")
        .map(|aliases| {
            aliases
                .properties
                .iter()
                .filter_map(|property| {
                    decode_string(&property.value).map(|value| (property.name.clone(), value))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn resolve_stdout_path(parsed: &ParsedFdt) -> Option<StdoutPath<'_>> {
    let raw = node(parsed, "/chosen")?
        .property("stdout-path")
        .and_then(decode_string)?;
    let (reference, options) = match raw.split_once(':') {
        Some((path, options)) => (path.to_string(), Some(options.to_string())),
        None => (raw.clone(), None),
    };
    let device = if reference.starts_with('/') {
        reference
    } else {
        aliases(parsed)
            .get(&reference)
            .cloned()
            .unwrap_or(reference)
    };
    Some(StdoutPath {
        node: node(parsed, &device),
        raw,
        device,
        options,
    })
}

pub fn classify_platform(parsed: &ParsedFdt) -> Platform {
    let compatible = node(parsed, "/").map(Node::compatible).unwrap_or_default();
    if compatible
        .iter()
        .any(|value| value.contains("bcm2712") || value.contains("raspberrypi,5-model-b"))
    {
        Platform::Rpi5Bcm2712
    } else if compatible.iter().any(|value| {
        value == "linux,dummy-virt" || value == "qemu,virt" || value.contains("qemu-virt")
    }) {
        Platform::QemuVirt
    } else {
        Platform::Unknown
    }
}

pub fn classify_node(node: &Node) -> NodeClasses {
    let name = node.name.to_ascii_lowercase();
    let path = node.path.to_ascii_lowercase();
    let compatibles = node.compatible();
    let combined = format!("{path} {}", compatibles.join(" ").to_ascii_lowercase());
    NodeClasses {
        serial: name.starts_with("serial@")
            || name.starts_with("uart@")
            || combined.contains("pl011")
            || combined.contains("ns16550"),
        interrupt_controller: node.has_property("interrupt-controller"),
        gpio_pinctrl: node.has_property("gpio-controller")
            || name.contains("gpio")
            || name.contains("pinctrl")
            || combined.contains("gpio")
            || combined.contains("pinctrl"),
        pwm_fan: name.contains("pwm")
            || name.contains("fan")
            || name.contains("cooling")
            || combined.contains("pwm")
            || combined.contains("fan")
            || combined.contains("cooling"),
        pcie_rp1: name.contains("pci")
            || name.contains("pcie")
            || name.contains("rp1")
            || combined.contains("pci")
            || combined.contains("pcie")
            || combined.contains("rp1")
            || combined.contains("brcm,bcm2712"),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeClasses {
    pub serial: bool,
    pub interrupt_controller: bool,
    pub gpio_pinctrl: bool,
    pub pwm_fan: bool,
    pub pcie_rp1: bool,
}

pub fn render_report(parsed: &ParsedFdt) -> String {
    let mut out = String::new();
    let header = parsed.header;
    writeln!(out, "== FDT Header ==").unwrap();
    writeln!(out, "magic: ok (0x{FDT_MAGIC:08x})").unwrap();
    writeln!(
        out,
        "total-size: {} (0x{:x})",
        header.total_size, header.total_size
    )
    .unwrap();
    writeln!(out, "version: {}", header.version).unwrap();
    writeln!(out, "last-compatible-version: {}", header.last_comp_version).unwrap();
    writeln!(out, "boot-cpu-id: {}", header.boot_cpuid_phys).unwrap();
    writeln!(out, "structure-size: {}", header.size_dt_struct).unwrap();
    writeln!(out, "strings-size: {}", header.size_dt_strings).unwrap();

    section(&mut out, "Root compatible");
    if let Some(root) = node(parsed, "/") {
        line_strings(&mut out, "compatible", &root.compatible());
    } else {
        writeln!(out, "<missing>").unwrap();
    }

    section(&mut out, "/chosen");
    if let Some(chosen) = node(parsed, "/chosen") {
        line_string_property(&mut out, chosen, "bootargs");
        line_string_property(&mut out, chosen, "stdout-path");
        line_scalar_property(&mut out, chosen, "linux,initrd-start");
        line_scalar_property(&mut out, chosen, "linux,initrd-end");
    } else {
        writeln!(out, "<missing>").unwrap();
    }

    section(&mut out, "Resolved stdout");
    render_resolved_stdout(&mut out, parsed);

    section(&mut out, "Memory ranges");
    let memory = memory_nodes(parsed);
    render_nodes(&mut out, &memory, false, false);

    section(&mut out, "Reserved memory");
    let reserved: Vec<_> = parsed
        .nodes
        .iter()
        .filter(|node| node.path.starts_with("/reserved-memory/"))
        .collect();
    render_nodes(&mut out, &reserved, true, false);

    let categories: [(&str, fn(NodeClasses) -> bool); 5] = [
        ("Serial/UART nodes", is_serial),
        ("Interrupt controller nodes", is_interrupt_controller),
        ("GPIO/pinctrl nodes", is_gpio_pinctrl),
        ("PWM/fan/cooling nodes", is_pwm_fan),
        ("PCIe/RP1-ish nodes", is_pcie_rp1),
    ];
    for (title, include) in categories {
        section(&mut out, title);
        let nodes: Vec<_> = parsed
            .nodes
            .iter()
            .filter(|node| include(classify_node(node)))
            .collect();
        render_nodes(&mut out, &nodes, false, title == "PCIe/RP1-ish nodes");
    }
    out
}

pub fn render_yarm_readiness(parsed: &ParsedFdt) -> String {
    render_yarm_readiness_with_options(parsed, false)
}

pub fn render_yarm_readiness_with_options(parsed: &ParsedFdt, verbose_nodes: bool) -> String {
    let mut out = String::new();
    let platform = classify_platform(parsed);
    let memory: Vec<_> = memory_nodes(parsed)
        .into_iter()
        .filter(|node| node_is_usable(node))
        .collect();
    let stdout = resolve_stdout_path(parsed);
    let serial = stdout
        .as_ref()
        .and_then(|stdout| stdout.node)
        .filter(|node| classify_node(node).serial && node_is_usable(node))
        .or_else(|| first_usable(parsed, |classes| classes.serial));
    let interrupt = first_usable(parsed, |classes| classes.interrupt_controller);
    let rp1_pcie = rp1_pcie_summary(parsed);
    let initrd = initrd_range(parsed);

    writeln!(out, "== YARM Readiness ==").unwrap();
    writeln!(out, "platform: {}", platform.label()).unwrap();
    writeln!(out, "memory-ranges:").unwrap();
    if memory.is_empty() {
        writeln!(out, "  <none>").unwrap();
    } else {
        for memory_node in &memory {
            render_readiness_ranges(&mut out, memory_node);
        }
    }
    match initrd {
        Some((start, end)) if end > start => {
            writeln!(
                out,
                "initrd: present start=0x{start:x} end=0x{end:x} size=0x{:x}",
                end - start
            )
            .unwrap();
        }
        _ => writeln!(out, "initrd: missing").unwrap(),
    }
    match &stdout {
        Some(stdout) => {
            writeln!(out, "stdout-path-raw: {}", stdout.raw).unwrap();
            writeln!(out, "stdout-path-resolved: {}", stdout.device).unwrap();
        }
        None => {
            writeln!(out, "stdout-path-raw: <missing>").unwrap();
            writeln!(out, "stdout-path-resolved: <missing>").unwrap();
        }
    }
    line_candidate(&mut out, "first-usable-serial", serial);
    line_candidate(&mut out, "interrupt-controller", interrupt);
    render_rp1_pcie(&mut out, &rp1_pcie, verbose_nodes);

    writeln!(out, "warnings:").unwrap();
    let mut warnings = Vec::new();
    if platform == Platform::Unknown {
        warnings.push("unrecognized platform; YARM AArch64 first-boot assumptions are unverified");
    }
    if platform == Platform::Rpi5Bcm2712 {
        warnings.push("Raspberry Pi 5 bare-metal boot and RP1 device support are not implemented");
    }
    if memory.is_empty() {
        warnings.push("no usable memory range was discovered");
    }
    if initrd.is_none_or(|(start, end)| end <= start) {
        warnings.push(
            "linux,initrd-start/end are missing or invalid; initramfs /init cannot be handed off",
        );
    }
    if stdout.as_ref().and_then(|stdout| stdout.node).is_none() {
        warnings.push("stdout-path is missing or does not resolve to a DT node");
    }
    if serial.is_none() {
        warnings.push("no enabled serial node is available for early boot markers");
    }
    if interrupt.is_none() {
        warnings.push("no enabled interrupt-controller node was found");
    }
    if platform == Platform::Rpi5Bcm2712 && rp1_pcie.rp1_node.is_none() {
        warnings.push(
            "BCM2712 tree has no RP1/PCIe candidate; Pi 5 peripheral discovery is incomplete",
        );
    }
    if warnings.is_empty() {
        writeln!(out, "  <none>").unwrap();
    } else {
        for warning in warnings {
            writeln!(out, "  - {warning}").unwrap();
        }
    }
    out
}

#[derive(Debug, Default)]
struct Rp1PcieSummary<'a> {
    pcie_controller: Option<&'a Node>,
    rp1_node: Option<&'a Node>,
    gpio: Option<&'a Node>,
    pwm_count: usize,
    uart_count: usize,
    ethernet_present: bool,
    usb_count: usize,
    child_count: usize,
    verbose_nodes: Vec<&'a Node>,
}

fn rp1_pcie_summary(parsed: &ParsedFdt) -> Rp1PcieSummary<'_> {
    let rp1_node = parsed
        .nodes
        .iter()
        .filter(|node| is_rp1_node(node) && node_is_usable(node))
        .min_by(|left, right| left.path.cmp(&right.path));
    let pcie_controller = rp1_node.and_then(|rp1| {
        ancestors(&rp1.path)
            .filter_map(|path| node(parsed, path))
            .find(|node| is_pcie_controller(node) && node_is_usable(node))
    });

    let mut summary = Rp1PcieSummary {
        pcie_controller,
        rp1_node,
        ..Rp1PcieSummary::default()
    };
    let Some(rp1) = rp1_node else {
        return summary;
    };

    let descendant_prefix = format!("{}/", rp1.path);
    let mut descendants: Vec<_> = parsed
        .nodes
        .iter()
        .filter(|node| node.path.starts_with(&descendant_prefix) && node_is_usable(node))
        .collect();
    descendants.sort_by(|left, right| left.path.cmp(&right.path));

    summary.gpio = descendants.iter().copied().find(|node| is_rp1_gpio(node));
    summary.pwm_count = descendants.iter().filter(|node| is_rp1_pwm(node)).count();
    summary.uart_count = descendants
        .iter()
        .filter(|node| classify_node(node).serial)
        .count();
    summary.ethernet_present = descendants.iter().any(|node| is_ethernet(node));
    summary.usb_count = descendants.iter().filter(|node| is_usb(node)).count();
    summary.child_count = descendants
        .iter()
        .filter(|node| direct_parent_path(&node.path) == Some(rp1.path.as_str()))
        .count();
    summary.verbose_nodes = parsed
        .nodes
        .iter()
        .filter(|node| is_rp1_pcie_node(node) && node_is_usable(node))
        .collect();
    summary
        .verbose_nodes
        .sort_by(|left, right| left.path.cmp(&right.path));
    summary
}

fn render_rp1_pcie(out: &mut String, summary: &Rp1PcieSummary<'_>, verbose_nodes: bool) {
    let Some(rp1_node) = summary.rp1_node else {
        writeln!(out, "rp1-pcie: missing").unwrap();
        return;
    };

    writeln!(out, "rp1-pcie:").unwrap();
    line_candidate_indented(out, "pcie-controller", summary.pcie_controller);
    line_candidate_indented(out, "rp1-node", Some(rp1_node));
    if let Some(gpio) = summary.gpio {
        writeln!(out, "  rp1-gpio: {}", gpio.path).unwrap();
    }
    writeln!(out, "  rp1-pwm-count: {}", summary.pwm_count).unwrap();
    writeln!(out, "  rp1-uart-count: {}", summary.uart_count).unwrap();
    writeln!(
        out,
        "  rp1-ethernet: {}",
        if summary.ethernet_present {
            "present"
        } else {
            "absent"
        }
    )
    .unwrap();
    writeln!(out, "  rp1-usb-count: {}", summary.usb_count).unwrap();
    writeln!(out, "  rp1-child-count: {}", summary.child_count).unwrap();
    if verbose_nodes {
        writeln!(out, "  nodes:").unwrap();
        for node in &summary.verbose_nodes {
            writeln!(out, "    - {}", node.path).unwrap();
        }
    }
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(direct_parent_path(path), |path| direct_parent_path(path))
}

fn direct_parent_path(path: &str) -> Option<&str> {
    let split = path.rfind('/')?;
    if split == 0 {
        Some("/")
    } else {
        Some(&path[..split])
    }
}

fn is_rp1_node(node: &Node) -> bool {
    let name = node.name.to_ascii_lowercase();
    name == "rp1"
        || name.starts_with("rp1@")
        || node
            .compatible()
            .iter()
            .any(|compatible| compatible.to_ascii_lowercase().contains("raspberrypi,rp1"))
}

fn is_pcie_controller(node: &Node) -> bool {
    let name = node.name.to_ascii_lowercase();
    let compatible = node.compatible().join(" ").to_ascii_lowercase();
    name.starts_with("pci@")
        || name.starts_with("pcie@")
        || compatible.contains("pcie")
        || compatible.contains("pci-host")
}

fn is_rp1_gpio(node: &Node) -> bool {
    node.has_property("gpio-controller") || node.name.to_ascii_lowercase().starts_with("gpio@")
}

fn is_rp1_pwm(node: &Node) -> bool {
    let name = node.name.to_ascii_lowercase();
    let compatible = node.compatible().join(" ").to_ascii_lowercase();
    name == "pwm" || name.starts_with("pwm@") || compatible.contains("pwm")
}

fn is_ethernet(node: &Node) -> bool {
    let name = node.name.to_ascii_lowercase();
    let compatible = node.compatible().join(" ").to_ascii_lowercase();
    name == "ethernet" || name.starts_with("ethernet@") || compatible.contains("ethernet")
}

fn is_usb(node: &Node) -> bool {
    let name = node.name.to_ascii_lowercase();
    name == "usb" || name.starts_with("usb@")
}

fn memory_nodes(parsed: &ParsedFdt) -> Vec<&Node> {
    parsed
        .nodes
        .iter()
        .filter(|node| {
            node.name == "memory"
                || node.name.starts_with("memory@")
                || node
                    .property("device_type")
                    .and_then(decode_string)
                    .as_deref()
                    == Some("memory")
        })
        .collect()
}

fn is_rp1_pcie_node(node: &Node) -> bool {
    let name = node.name.to_ascii_lowercase();
    let path = node.path.to_ascii_lowercase();
    let compatible = node.compatible().join(" ").to_ascii_lowercase();
    name.contains("pci")
        || name.contains("pcie")
        || name.contains("rp1")
        || path.contains("pci")
        || path.contains("pcie")
        || path.contains("rp1")
        || compatible.contains("pci")
        || compatible.contains("pcie")
        || compatible.contains("rp1")
}

fn initrd_range(parsed: &ParsedFdt) -> Option<(u64, u64)> {
    let chosen = node(parsed, "/chosen")?;
    Some((
        decode_scalar(chosen.property("linux,initrd-start")?).ok()?,
        decode_scalar(chosen.property("linux,initrd-end")?).ok()?,
    ))
}

fn first_usable(parsed: &ParsedFdt, include: fn(NodeClasses) -> bool) -> Option<&Node> {
    parsed
        .nodes
        .iter()
        .find(|node| include(classify_node(node)) && node_is_usable(node))
}

fn node_is_usable(node: &Node) -> bool {
    node.property("status")
        .and_then(decode_string)
        .is_none_or(|status| status == "okay" || status == "ok")
}

fn line_candidate(out: &mut String, label: &str, candidate: Option<&Node>) {
    match candidate {
        Some(node) => writeln!(out, "{label}: {}", node.path).unwrap(),
        None => writeln!(out, "{label}: <none>").unwrap(),
    }
}

fn line_candidate_indented(out: &mut String, label: &str, candidate: Option<&Node>) {
    match candidate {
        Some(node) => writeln!(out, "  {label}: {}", node.path).unwrap(),
        None => writeln!(out, "  {label}: <none>").unwrap(),
    }
}

fn render_readiness_ranges(out: &mut String, node: &Node) {
    match node
        .property("reg")
        .map(|value| parse_reg_ranges(value, node.parent_address_cells, node.parent_size_cells))
    {
        Some(Ok(ranges)) if !ranges.is_empty() => {
            for range in ranges {
                writeln!(
                    out,
                    "  {} address=0x{:x} size=0x{:x}",
                    node.path, range.address, range.size
                )
                .unwrap();
            }
        }
        Some(Ok(_)) => writeln!(out, "  {} <empty>", node.path).unwrap(),
        Some(Err(error)) => writeln!(out, "  {} <unparsed: {error}>", node.path).unwrap(),
        None => writeln!(out, "  {} <missing reg>", node.path).unwrap(),
    }
}

fn render_resolved_stdout(out: &mut String, parsed: &ParsedFdt) {
    let Some(stdout) = resolve_stdout_path(parsed) else {
        writeln!(out, "path: <missing>").unwrap();
        return;
    };
    writeln!(out, "path: {}", stdout.device).unwrap();
    if let Some(options) = stdout.options {
        writeln!(out, "options: {options}").unwrap();
    }
    match stdout.node {
        Some(node) => {
            line_strings(out, "  compatible", &node.compatible());
            line_string_property_indented(out, node, "status");
            render_reg(out, node);
        }
        None => writeln!(out, "node: <not found>").unwrap(),
    }
}

fn is_serial(classes: NodeClasses) -> bool {
    classes.serial
}

fn is_interrupt_controller(classes: NodeClasses) -> bool {
    classes.interrupt_controller
}

fn is_gpio_pinctrl(classes: NodeClasses) -> bool {
    classes.gpio_pinctrl
}

fn is_pwm_fan(classes: NodeClasses) -> bool {
    classes.pwm_fan
}

fn is_pcie_rp1(classes: NodeClasses) -> bool {
    classes.pcie_rp1
}

fn render_nodes(out: &mut String, nodes: &[&Node], show_no_map: bool, show_ranges: bool) {
    if nodes.is_empty() {
        writeln!(out, "<none>").unwrap();
        return;
    }
    for node in nodes {
        writeln!(out, "path: {}", node.path).unwrap();
        line_strings(out, "  compatible", &node.compatible());
        line_string_property_indented(out, node, "status");
        render_reg(out, node);
        if show_ranges {
            render_raw_cells(out, node, "ranges");
        }
        if show_no_map {
            writeln!(out, "  no-map: {}", node.has_property("no-map")).unwrap();
        }
        if classify_node(node).interrupt_controller {
            writeln!(out, "  interrupt-controller: true").unwrap();
        }
        if node.has_property("gpio-controller") {
            writeln!(out, "  gpio-controller: true").unwrap();
        }
    }
}

fn render_reg(out: &mut String, node: &Node) {
    let Some(value) = node.property("reg") else {
        writeln!(out, "  reg: <missing>").unwrap();
        return;
    };
    match parse_reg_ranges(value, node.parent_address_cells, node.parent_size_cells) {
        Ok(ranges) if ranges.is_empty() => writeln!(out, "  reg: <empty>").unwrap(),
        Ok(ranges) => {
            for range in ranges {
                writeln!(
                    out,
                    "  reg: address=0x{:x} size=0x{:x}",
                    range.address, range.size
                )
                .unwrap();
            }
        }
        Err(error) => {
            writeln!(out, "  reg: <unparsed: {error}>").unwrap();
            render_raw_cells(out, node, "reg");
        }
    }
}

fn render_raw_cells(out: &mut String, node: &Node, property: &str) {
    let Some(value) = node.property(property) else {
        if property == "ranges" {
            writeln!(out, "  ranges: <missing>").unwrap();
        }
        return;
    };
    match decode_be_cells(value) {
        Ok(cells) => {
            let text = cells
                .iter()
                .map(|cell| format!("0x{cell:08x}"))
                .collect::<Vec<_>>()
                .join(" ");
            if property == "ranges" {
                writeln!(
                    out,
                    "  ranges-raw-cells: [{text}] (address translation deferred)"
                )
                .unwrap();
            } else {
                writeln!(out, "  {property}-raw-cells: [{text}]").unwrap();
            }
        }
        Err(error) => writeln!(out, "  {property}-raw-cells: <malformed: {error}>").unwrap(),
    }
}

fn line_string_property(out: &mut String, node: &Node, property: &str) {
    match node.property(property).and_then(decode_string) {
        Some(value) => writeln!(out, "{property}: {value}").unwrap(),
        None => writeln!(out, "{property}: <missing>").unwrap(),
    }
}

fn line_string_property_indented(out: &mut String, node: &Node, property: &str) {
    match node.property(property).and_then(decode_string) {
        Some(value) => writeln!(out, "  {property}: {value}").unwrap(),
        None => writeln!(out, "  {property}: <missing>").unwrap(),
    }
}

fn line_scalar_property(out: &mut String, node: &Node, property: &str) {
    match node.property(property).map(decode_scalar) {
        Some(Ok(value)) => writeln!(out, "{property}: 0x{value:x}").unwrap(),
        Some(Err(error)) => writeln!(out, "{property}: <malformed: {error}>").unwrap(),
        None => writeln!(out, "{property}: <missing>").unwrap(),
    }
}

fn line_strings(out: &mut String, label: &str, strings: &[String]) {
    if strings.is_empty() {
        writeln!(out, "{label}: <missing>").unwrap();
    } else {
        writeln!(out, "{label}: {}", strings.join(", ")).unwrap();
    }
}

fn section(out: &mut String, title: &str) {
    writeln!(out, "\n== {title} ==").unwrap();
}

fn node<'a>(parsed: &'a ParsedFdt, path: &str) -> Option<&'a Node> {
    parsed.nodes.iter().find(|node| node.path == path)
}

fn decode_scalar(value: &[u8]) -> Result<u64, ProbeError> {
    let cells = decode_be_cells(value)?;
    if !(1..=2).contains(&cells.len()) {
        return Err(ProbeError::new(format!(
            "expected one or two cells, found {}",
            cells.len()
        )));
    }
    cells_to_u64(&cells)
}

fn cells_to_u64(cells: &[u32]) -> Result<u64, ProbeError> {
    if cells.len() > 2 {
        return Err(ProbeError::new("value exceeds two 32-bit cells"));
    }
    Ok(cells
        .iter()
        .fold(0u64, |value, cell| (value << 32) | u64::from(*cell)))
}

fn decode_single_cell(value: &[u8], name: &str) -> Result<u32, ProbeError> {
    if value.len() != 4 {
        return Err(ProbeError::new(format!(
            "{name} must contain exactly one cell"
        )));
    }
    read_be_u32_at(value, 0)
}

fn read_be_u32_at(bytes: &[u8], offset: usize) -> Result<u32, ProbeError> {
    let end = checked_add(offset, 4, "u32 read")?;
    let raw: [u8; 4] = bytes
        .get(offset..end)
        .ok_or_else(|| ProbeError::new(format!("truncated big-endian u32 at offset {offset}")))?
        .try_into()
        .map_err(|_| ProbeError::new("internal u32 slice error"))?;
    Ok(u32::from_be_bytes(raw))
}

fn checked_block<'a>(
    bytes: &'a [u8],
    offset: usize,
    size: usize,
    total_size: usize,
    label: &str,
) -> Result<&'a [u8], ProbeError> {
    let end = checked_add(offset, size, label)?;
    if end > total_size {
        return Err(ProbeError::new(format!(
            "{label} block ends at {end}, beyond FDT total size {total_size}"
        )));
    }
    bytes
        .get(offset..end)
        .ok_or_else(|| ProbeError::new(format!("{label} block is outside the input")))
}

fn read_cstr<'a>(
    bytes: &'a [u8],
    offset: usize,
    label: &str,
) -> Result<(&'a [u8], usize), ProbeError> {
    let tail = bytes
        .get(offset..)
        .ok_or_else(|| ProbeError::new(format!("{label} offset {offset} is outside its block")))?;
    let length = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| ProbeError::new(format!("unterminated {label}")))?;
    Ok((&tail[..length], checked_add(offset, length + 1, label)?))
}

fn checked_add(left: usize, right: usize, label: &str) -> Result<usize, ProbeError> {
    left.checked_add(right)
        .ok_or_else(|| ProbeError::new(format!("integer overflow while parsing {label}")))
}

fn align4(value: usize) -> Result<usize, ProbeError> {
    checked_add(value, 3, "four-byte alignment").map(|value| value & !3)
}

fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn cells(values: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            push_u32(&mut out, *value);
        }
        out
    }

    #[test]
    fn big_endian_cell_decoding_and_malformed_length() {
        assert_eq!(
            decode_be_cells(&cells(&[1, 0xfeed_beef])),
            Ok(vec![1, 0xfeed_beef])
        );
        assert!(decode_be_cells(&[0, 1, 2]).is_err());
    }

    #[test]
    fn string_and_compatible_decoding_tolerate_missing_trailing_nul() {
        assert_eq!(
            decode_string(b"console=ttyAMA0\0ignored"),
            Some("console=ttyAMA0".into())
        );
        assert_eq!(
            decode_string_list(b"arm,pl011\0arm,primecell\0"),
            vec!["arm,pl011", "arm,primecell"]
        );
        assert_eq!(decode_string_list(b"brcm,bcm2712"), vec!["brcm,bcm2712"]);
    }

    #[test]
    fn reg_ranges_support_one_one_and_two_two_cells() {
        assert_eq!(
            parse_reg_ranges(&cells(&[0x1000, 0x100]), 1, 1),
            Ok(vec![RegRange {
                address: 0x1000,
                size: 0x100
            }])
        );
        assert_eq!(
            parse_reg_ranges(&cells(&[1, 0x2345_0000, 0, 0x2000]), 2, 2),
            Ok(vec![RegRange {
                address: 0x1_2345_0000,
                size: 0x2000
            }])
        );
        assert!(parse_reg_ranges(&cells(&[1, 2, 3]), 2, 2).is_err());
    }

    #[test]
    fn missing_properties_are_optional() {
        let node = Node {
            path: "/chosen".into(),
            name: "chosen".into(),
            properties: vec![],
            parent_address_cells: 2,
            parent_size_cells: 1,
            address_cells: 2,
            size_cells: 1,
        };
        assert_eq!(node.property("bootargs"), None);
        assert!(node.compatible().is_empty());
    }

    #[test]
    fn node_classification_covers_requested_groups() {
        let make = |path: &str, name: &str, compatible: &str, marker: Option<&str>| Node {
            path: path.into(),
            name: name.into(),
            properties: [
                Some(Property {
                    name: "compatible".into(),
                    value: format!("{compatible}\0").into_bytes(),
                }),
                marker.map(|name| Property {
                    name: name.into(),
                    value: vec![],
                }),
            ]
            .into_iter()
            .flatten()
            .collect(),
            parent_address_cells: 2,
            parent_size_cells: 1,
            address_cells: 2,
            size_cells: 1,
        };
        assert!(classify_node(&make("/soc/serial@0", "serial@0", "arm,pl011", None)).serial);
        assert!(
            classify_node(&make(
                "/intc",
                "intc",
                "arm,gic-v3",
                Some("interrupt-controller")
            ))
            .interrupt_controller
        );
        assert!(
            classify_node(&make(
                "/soc/gpio@0",
                "gpio@0",
                "brcm,gpio",
                Some("gpio-controller")
            ))
            .gpio_pinctrl
        );
        assert!(classify_node(&make("/fan", "cooling-fan", "pwm-fan", None)).pwm_fan);
        assert!(classify_node(&make("/axi/pcie@0/rp1", "rp1", "raspberrypi,rp1", None)).pcie_rp1);
        assert!(classify_node(&make("/soc", "soc", "brcm,bcm2712", None)).pcie_rp1);
    }

    #[test]
    fn aliases_resolve_stdout_device_and_options() {
        let parsed = parse_fdt(&synthetic_dtb()).expect("synthetic DTB parses");
        assert_eq!(
            aliases(&parsed).get("serial10").map(String::as_str),
            Some("/pl011@9000000")
        );
        let stdout = resolve_stdout_path(&parsed).expect("stdout path");
        assert_eq!(stdout.raw, "serial10:115200n8");
        assert_eq!(stdout.device, "/pl011@9000000");
        assert_eq!(stdout.options.as_deref(), Some("115200n8"));
        assert_eq!(
            stdout.node.map(|node| node.path.as_str()),
            Some("/pl011@9000000")
        );
    }

    #[test]
    fn readiness_report_covers_first_boot_inputs() {
        let parsed = parse_fdt(&synthetic_dtb()).expect("synthetic DTB parses");
        let report = render_yarm_readiness(&parsed);
        for expected in [
            "platform: qemu-virt",
            "memory-ranges:",
            "/memory@40000000 address=0x40000000 size=0x10000000",
            "initrd: present start=0x48000000 end=0x49000000 size=0x1000000",
            "stdout-path-raw: serial10:115200n8",
            "stdout-path-resolved: /pl011@9000000",
            "first-usable-serial: /pl011@9000000",
            "interrupt-controller: /intc@8000000",
            "rp1-pcie: missing",
            "warnings:\n  <none>",
        ] {
            assert!(
                report.contains(expected),
                "missing readiness line: {expected}\n{report}"
            );
        }
    }

    #[test]
    fn platform_classification_recognizes_bcm2712() {
        let mut parsed = parse_fdt(&synthetic_dtb()).expect("synthetic DTB parses");
        let root = parsed
            .nodes
            .iter_mut()
            .find(|node| node.path == "/")
            .unwrap();
        root.properties
            .iter_mut()
            .find(|property| property.name == "compatible")
            .unwrap()
            .value = b"raspberrypi,5-model-b\0brcm,bcm2712\0".to_vec();
        assert_eq!(classify_platform(&parsed), Platform::Rpi5Bcm2712);
        assert!(
            render_yarm_readiness(&parsed).contains(
                "Raspberry Pi 5 bare-metal boot and RP1 device support are not implemented"
            )
        );
    }

    #[test]
    fn readiness_rp1_pcie_output_is_concise_and_ordered() {
        let parsed = synthetic_rp1_tree();
        let report = render_yarm_readiness(&parsed);
        let expected = "\
rp1-pcie:
  pcie-controller: /axi/pcie@120000
  rp1-node: /axi/pcie@120000/rp1@0
  rp1-gpio: /axi/pcie@120000/rp1@0/gpio@d0000
  rp1-pwm-count: 2
  rp1-uart-count: 2
  rp1-ethernet: present
  rp1-usb-count: 2
  rp1-child-count: 8
warnings:";
        assert!(
            report.contains(expected),
            "unexpected readiness report:\n{report}"
        );
        assert!(!report.contains("uart@30000"));
        assert!(!report.contains("usb@200000"));
    }

    #[test]
    fn verbose_nodes_preserves_sorted_rp1_descendant_listing() {
        let parsed = synthetic_rp1_tree();
        let report = render_yarm_readiness_with_options(&parsed, true);
        let expected = "\
  nodes:
    - /axi/pcie@120000
    - /axi/pcie@120000/rp1@0
    - /axi/pcie@120000/rp1@0/ethernet@100000
    - /axi/pcie@120000/rp1@0/gpio@d0000
    - /axi/pcie@120000/rp1@0/pwm@98000
    - /axi/pcie@120000/rp1@0/pwm@9c000
    - /axi/pcie@120000/rp1@0/uart@30000
    - /axi/pcie@120000/rp1@0/uart@34000
    - /axi/pcie@120000/rp1@0/usb@200000
    - /axi/pcie@120000/rp1@0/usb@200000/port@1
    - /axi/pcie@120000/rp1@0/usb@300000
warnings:";
        assert!(
            report.contains(expected),
            "unexpected verbose report:\n{report}"
        );
    }

    #[test]
    fn rp1_readiness_keeps_pi5_classification() {
        let parsed = synthetic_rp1_tree();
        assert_eq!(classify_platform(&parsed), Platform::Rpi5Bcm2712);
        assert!(render_yarm_readiness(&parsed).contains("platform: rpi5-bcm2712"));
    }

    #[test]
    fn malformed_headers_and_properties_return_errors() {
        assert!(parse_fdt(&[]).is_err());
        let mut bad_magic = vec![0; HEADER_LEN];
        assert!(parse_fdt(&bad_magic).is_err());
        bad_magic[..4].copy_from_slice(&FDT_MAGIC.to_be_bytes());
        bad_magic[4..8].copy_from_slice(&(HEADER_LEN as u32).to_be_bytes());
        assert!(parse_fdt(&bad_magic).is_err());

        let mut malformed_property = synthetic_dtb();
        let struct_offset =
            u32::from_be_bytes(malformed_property[8..12].try_into().unwrap()) as usize;
        // Root BEGIN_NODE occupies eight bytes including its aligned empty name;
        // overwrite the first property's length with a value beyond the block.
        malformed_property[struct_offset + 12..struct_offset + 16]
            .copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse_fdt(&malformed_property).is_err());
    }

    #[test]
    fn parse_and_render_synthetic_tree_is_stable() {
        let dtb = synthetic_dtb();
        let parsed = parse_fdt(&dtb).expect("synthetic DTB parses");
        assert_eq!(parsed.nodes.len(), 6);
        let report = render_report(&parsed);
        for expected in [
            "magic: ok (0xd00dfeed)",
            "compatible: test,qemu-virt, test,board",
            "bootargs: console=ttyAMA0",
            "stdout-path: serial10:115200n8",
            "== Resolved stdout ==",
            "path: /pl011@9000000",
            "options: 115200n8",
            "linux,initrd-start: 0x48000000",
            "reg: address=0x40000000 size=0x10000000",
            "path: /pl011@9000000",
            "interrupt-controller: true",
        ] {
            assert!(
                report.contains(expected),
                "missing report line: {expected}\n{report}"
            );
        }
    }

    fn synthetic_rp1_tree() -> ParsedFdt {
        let mut parsed = parse_fdt(&synthetic_dtb()).expect("synthetic DTB parses");
        parsed
            .nodes
            .iter_mut()
            .find(|node| node.path == "/")
            .unwrap()
            .properties
            .iter_mut()
            .find(|property| property.name == "compatible")
            .unwrap()
            .value = b"raspberrypi,5-model-b\0brcm,bcm2712\0".to_vec();

        let make_node = |path: &str, compatible: &str, marker: Option<&str>| Node {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            properties: [
                (!compatible.is_empty()).then(|| Property {
                    name: "compatible".into(),
                    value: format!("{compatible}\0").into_bytes(),
                }),
                marker.map(|name| Property {
                    name: name.into(),
                    value: Vec::new(),
                }),
            ]
            .into_iter()
            .flatten()
            .collect(),
            parent_address_cells: 2,
            parent_size_cells: 2,
            address_cells: 2,
            size_cells: 2,
        };
        // Deliberately use non-path order so rendering must sort the verbose list.
        parsed.nodes.extend([
            make_node("/axi/pcie@120000/rp1@0/usb@300000", "generic-xhci", None),
            make_node("/axi/pcie@120000", "brcm,bcm2712-pcie", None),
            make_node("/axi/pcie@120000/rp1@0/uart@34000", "arm,pl011", None),
            make_node(
                "/axi/pcie@120000/rp1@0/gpio@d0000",
                "raspberrypi,rp1-gpio",
                Some("gpio-controller"),
            ),
            make_node(
                "/axi/pcie@120000/rp1@0/ethernet@100000",
                "raspberrypi,rp1-ethernet",
                None,
            ),
            make_node("/axi/pcie@120000/rp1@0", "raspberrypi,rp1", None),
            make_node("/axi/pcie@120000/rp1@0/usb@200000/port@1", "usb-port", None),
            make_node(
                "/axi/pcie@120000/rp1@0/pwm@9c000",
                "raspberrypi,rp1-pwm",
                None,
            ),
            make_node("/axi/pcie@120000/rp1@0/uart@30000", "arm,pl011", None),
            make_node("/axi/pcie@120000/rp1@0/usb@200000", "generic-xhci", None),
            make_node(
                "/axi/pcie@120000/rp1@0/pwm@98000",
                "raspberrypi,rp1-pwm",
                None,
            ),
        ]);
        parsed
    }

    fn synthetic_dtb() -> Vec<u8> {
        let names = [
            "#address-cells",
            "#size-cells",
            "compatible",
            "bootargs",
            "stdout-path",
            "linux,initrd-start",
            "linux,initrd-end",
            "device_type",
            "reg",
            "interrupt-controller",
            "serial10",
        ];
        let mut strings = Vec::new();
        let mut offsets = std::collections::BTreeMap::new();
        for name in names {
            offsets.insert(name, strings.len() as u32);
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }
        let mut structure = Vec::new();
        begin_node(&mut structure, "");
        property(&mut structure, offsets["#address-cells"], &cells(&[2]));
        property(&mut structure, offsets["#size-cells"], &cells(&[2]));
        property(
            &mut structure,
            offsets["compatible"],
            b"test,qemu-virt\0test,board\0",
        );

        begin_node(&mut structure, "aliases");
        property(&mut structure, offsets["serial10"], b"/pl011@9000000\0");
        end_node(&mut structure);

        begin_node(&mut structure, "chosen");
        property(&mut structure, offsets["bootargs"], b"console=ttyAMA0\0");
        property(
            &mut structure,
            offsets["stdout-path"],
            b"serial10:115200n8\0",
        );
        property(
            &mut structure,
            offsets["linux,initrd-start"],
            &cells(&[0, 0x4800_0000]),
        );
        property(
            &mut structure,
            offsets["linux,initrd-end"],
            &cells(&[0, 0x4900_0000]),
        );
        end_node(&mut structure);

        begin_node(&mut structure, "memory@40000000");
        property(&mut structure, offsets["device_type"], b"memory\0");
        property(
            &mut structure,
            offsets["reg"],
            &cells(&[0, 0x4000_0000, 0, 0x1000_0000]),
        );
        end_node(&mut structure);

        begin_node(&mut structure, "pl011@9000000");
        property(&mut structure, offsets["compatible"], b"arm,pl011\0");
        property(
            &mut structure,
            offsets["reg"],
            &cells(&[0, 0x0900_0000, 0, 0x1000]),
        );
        end_node(&mut structure);

        begin_node(&mut structure, "intc@8000000");
        property(&mut structure, offsets["compatible"], b"arm,gic-v3\0");
        property(&mut structure, offsets["interrupt-controller"], b"");
        end_node(&mut structure);

        end_node(&mut structure);
        push_u32(&mut structure, FDT_END);

        let off_mem_rsvmap = HEADER_LEN;
        let reserve_len = 16;
        let off_dt_struct = off_mem_rsvmap + reserve_len;
        let off_dt_strings = off_dt_struct + structure.len();
        let total = off_dt_strings + strings.len();
        let mut out = Vec::new();
        for value in [
            FDT_MAGIC,
            total as u32,
            off_dt_struct as u32,
            off_dt_strings as u32,
            off_mem_rsvmap as u32,
            17,
            16,
            0,
            strings.len() as u32,
            structure.len() as u32,
        ] {
            push_u32(&mut out, value);
        }
        out.resize(off_dt_struct, 0);
        out.extend_from_slice(&structure);
        out.extend_from_slice(&strings);
        out
    }

    fn begin_node(out: &mut Vec<u8>, name: &str) {
        push_u32(out, FDT_BEGIN_NODE);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        align_vec(out);
    }

    fn end_node(out: &mut Vec<u8>) {
        push_u32(out, FDT_END_NODE);
    }

    fn property(out: &mut Vec<u8>, name_offset: u32, value: &[u8]) {
        push_u32(out, FDT_PROP);
        push_u32(out, value.len() as u32);
        push_u32(out, name_offset);
        out.extend_from_slice(value);
        align_vec(out);
    }

    fn align_vec(out: &mut Vec<u8>) {
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
}
