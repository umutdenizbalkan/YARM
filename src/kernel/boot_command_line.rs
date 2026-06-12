// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::lock::SpinLock;

/// Maximum number of boot command-line bytes retained by the kernel.
///
/// The storage is policy-neutral and keeps arbitrary bytes; UTF-8 validation is
/// left to consumers that interpret a particular option.
pub const BOOT_COMMAND_LINE_MAX_BYTES: usize = 2048;
pub const YARM_BOOT_OPTION_MAX_KEY_BYTES: usize = 64;
pub const YARM_BOOT_OPTION_MAX_VALUE_BYTES: usize = 1024;
pub const YARM_MANIFEST_PATH_MAX_BYTES: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootCommandLineStatus {
    Absent,
    Captured,
    Truncated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BootCommandLine {
    bytes: [u8; BOOT_COMMAND_LINE_MAX_BYTES],
    len: usize,
    status: BootCommandLineStatus,
}

impl core::fmt::Debug for BootCommandLine {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("BootCommandLine")
            .field("bytes", &self.raw_cmdline())
            .field("status", &self.status)
            .finish()
    }
}

impl BootCommandLine {
    pub const fn absent() -> Self {
        Self {
            bytes: [0; BOOT_COMMAND_LINE_MAX_BYTES],
            len: 0,
            status: BootCommandLineStatus::Absent,
        }
    }

    /// Copies bytes through the first NUL, if present, into fixed-size storage.
    ///
    /// Empty input and an immediately terminating NUL are represented as absent.
    /// Inputs longer than the fixed buffer are truncated and marked accordingly.
    /// Bytes are stored losslessly; invalid UTF-8 is not rejected.
    pub fn set_raw_cmdline_from_bytes(&mut self, source: &[u8]) {
        self.bytes.fill(0);
        let nul = source.iter().position(|byte| *byte == 0);
        let source_len = nul.unwrap_or(source.len());
        self.len = core::cmp::min(source_len, BOOT_COMMAND_LINE_MAX_BYTES);
        self.bytes[..self.len].copy_from_slice(&source[..self.len]);
        self.status = if self.len == 0 {
            BootCommandLineStatus::Absent
        } else if source_len > BOOT_COMMAND_LINE_MAX_BYTES {
            BootCommandLineStatus::Truncated
        } else {
            BootCommandLineStatus::Captured
        };
    }

    pub fn raw_cmdline(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub const fn status(&self) -> BootCommandLineStatus {
        self.status
    }

    pub const fn cmdline_was_truncated(&self) -> bool {
        matches!(self.status, BootCommandLineStatus::Truncated)
    }
}

static BOOT_COMMAND_LINE: SpinLock<BootCommandLine> = SpinLock::new(BootCommandLine::absent());

pub fn set_raw_cmdline_from_bytes(source: &[u8]) -> BootCommandLine {
    let mut command_line = BOOT_COMMAND_LINE.lock();
    command_line.set_raw_cmdline_from_bytes(source);
    *command_line
}

pub fn boot_command_line() -> BootCommandLine {
    *BOOT_COMMAND_LINE.lock()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlatformOption {
    #[default]
    Auto,
    QemuVirt,
    Rpi5,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BootPhase {
    Entry,
    Uart,
    Dtb,
    Mmu,
    #[default]
    Kernel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct YarmBootOptions<'a> {
    pub manifest_path: Option<&'a [u8]>,
    pub platform: PlatformOption,
    pub boot_phase: BootPhase,
    pub max_cpus: Option<usize>,
}

/// Parses YARM-owned `key=value` tokens without applying any boot policy.
///
/// Tokens are ASCII-whitespace separated. Tokens without `=` and keys outside
/// the `yarm.` namespace are ignored. Duplicate recognized keys use last-wins
/// semantics. Unknown `yarm.*` keys are ignored. The returned manifest path is
/// borrowed from `raw` and is never read from CPIO or acted upon here.
pub fn parse_yarm_boot_options(raw: &[u8]) -> YarmBootOptions<'_> {
    let mut options = YarmBootOptions::default();
    for token in raw.split(|byte| byte.is_ascii_whitespace()) {
        if token.is_empty() {
            continue;
        }
        let Some(separator) = token.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = &token[..separator];
        let value = &token[separator + 1..];
        if key.len() > YARM_BOOT_OPTION_MAX_KEY_BYTES
            || value.len() > YARM_BOOT_OPTION_MAX_VALUE_BYTES
            || !key.starts_with(b"yarm.")
        {
            continue;
        }
        match key {
            b"yarm.manifest" => {
                options.manifest_path = valid_manifest_path(value).then_some(value);
            }
            b"yarm.platform" => {
                if let Some(platform) = parse_platform_option(value) {
                    options.platform = platform;
                }
            }
            b"yarm.boot_phase" => {
                if let Some(phase) = parse_boot_phase(value) {
                    options.boot_phase = phase;
                }
            }
            b"yarm.max_cpus" => {
                if let Some(max_cpus) = parse_positive_usize(value) {
                    options.max_cpus = Some(max_cpus);
                }
            }
            _ => {}
        }
    }
    options
}

fn parse_platform_option(value: &[u8]) -> Option<PlatformOption> {
    match value {
        b"auto" => Some(PlatformOption::Auto),
        b"qemu-virt" => Some(PlatformOption::QemuVirt),
        b"rpi5" => Some(PlatformOption::Rpi5),
        _ => None,
    }
}

fn parse_boot_phase(value: &[u8]) -> Option<BootPhase> {
    match value {
        b"entry" => Some(BootPhase::Entry),
        b"uart" => Some(BootPhase::Uart),
        b"dtb" => Some(BootPhase::Dtb),
        b"mmu" => Some(BootPhase::Mmu),
        b"kernel" => Some(BootPhase::Kernel),
        _ => None,
    }
}

fn parse_positive_usize(value: &[u8]) -> Option<usize> {
    if value.is_empty() {
        return None;
    }
    let mut parsed = 0usize;
    for byte in value {
        if !byte.is_ascii_digit() {
            return None;
        }
        parsed = parsed
            .checked_mul(10)?
            .checked_add((byte - b'0') as usize)?;
    }
    (parsed > 0).then_some(parsed)
}

fn valid_manifest_path(path: &[u8]) -> bool {
    !path.is_empty()
        && path.len() <= YARM_MANIFEST_PATH_MAX_BYTES
        && path[0] == b'/'
        && path
            .iter()
            .all(|byte| !byte.is_ascii_whitespace() && !byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_defaults_to_absent() {
        let command_line = BootCommandLine::absent();
        assert_eq!(command_line.raw_cmdline(), b"");
        assert_eq!(command_line.status(), BootCommandLineStatus::Absent);
    }

    #[test]
    fn command_line_copies_normal_and_invalid_bytes_losslessly() {
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(b"console=ttyS0 \xff");
        assert_eq!(command_line.raw_cmdline(), b"console=ttyS0 \xff");
        assert_eq!(command_line.status(), BootCommandLineStatus::Captured);
    }

    #[test]
    fn command_line_accepts_exact_maximum() {
        let source = [b'x'; BOOT_COMMAND_LINE_MAX_BYTES];
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(&source);
        assert_eq!(command_line.raw_cmdline(), source);
        assert!(!command_line.cmdline_was_truncated());
    }

    #[test]
    fn command_line_truncates_overlong_input() {
        let source = [b'x'; BOOT_COMMAND_LINE_MAX_BYTES + 1];
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(&source);
        assert_eq!(
            command_line.raw_cmdline().len(),
            BOOT_COMMAND_LINE_MAX_BYTES
        );
        assert!(command_line.cmdline_was_truncated());
    }

    #[test]
    fn command_line_stops_at_nul_before_applying_limit() {
        let mut command_line = BootCommandLine::absent();
        command_line.set_raw_cmdline_from_bytes(b"yarm.manifest=/boot/a\0ignored");
        assert_eq!(command_line.raw_cmdline(), b"yarm.manifest=/boot/a");
        assert_eq!(command_line.status(), BootCommandLineStatus::Captured);
    }

    #[test]
    fn parser_extracts_manifest_and_ignores_linux_options() {
        let parsed = parse_yarm_boot_options(
            b"console=ttyS0 rdinit=/init other=value yarm.manifest=/boot/services-core.txt",
        );
        assert_eq!(
            parsed.manifest_path,
            Some(b"/boot/services-core.txt".as_slice())
        );
    }

    #[test]
    fn parser_uses_last_manifest_value() {
        let parsed = parse_yarm_boot_options(
            b"yarm.manifest=/boot/first.txt yarm.unknown=x yarm.manifest=/boot/last.txt",
        );
        assert_eq!(parsed.manifest_path, Some(b"/boot/last.txt".as_slice()));
    }

    #[test]
    fn parser_rejects_invalid_manifest_paths() {
        for raw in [
            b"yarm.manifest=relative".as_slice(),
            b"yarm.manifest=".as_slice(),
            b"yarm.manifest=/boot/control\x01path".as_slice(),
            b"yarm.manifest=\"/boot/has space\"".as_slice(),
        ] {
            assert_eq!(parse_yarm_boot_options(raw).manifest_path, None, "{raw:?}");
        }
    }

    #[test]
    fn platform_phase_and_cpu_options_parse_with_qemu_preserving_defaults() {
        let defaults = parse_yarm_boot_options(b"");
        assert_eq!(defaults.platform, PlatformOption::Auto);
        assert_eq!(defaults.boot_phase, BootPhase::Kernel);
        assert_eq!(defaults.max_cpus, None);

        let parsed =
            parse_yarm_boot_options(b"yarm.platform=rpi5 yarm.boot_phase=uart yarm.max_cpus=1");
        assert_eq!(parsed.platform, PlatformOption::Rpi5);
        assert_eq!(parsed.boot_phase, BootPhase::Uart);
        assert_eq!(parsed.max_cpus, Some(1));
    }

    #[test]
    fn recognized_boot_options_are_last_wins_and_invalid_values_are_ignored() {
        let parsed = parse_yarm_boot_options(
            b"yarm.platform=qemu-virt yarm.platform=rpi5 yarm.boot_phase=dtb \
              yarm.boot_phase=bogus yarm.max_cpus=4 yarm.max_cpus=0",
        );
        assert_eq!(parsed.platform, PlatformOption::Rpi5);
        assert_eq!(parsed.boot_phase, BootPhase::Dtb);
        assert_eq!(parsed.max_cpus, Some(4));
    }
}
