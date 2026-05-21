// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::super::common::vfs_ipc::{VfsBackend, VfsError};
use yarm_ipc_abi::devfs_abi::{DEVFS_NODE_KIND_BLOCK, DEVFS_NODE_KIND_CHAR};

/// Compatibility path-id constant used by mount/policy/interop tests.
pub const DEV_CONSOLE_PATH_PTR: u64 = 0x434F_4E53_4F4C_4500;
pub const DEV_CONSOLE_PATH: &[u8] = b"/dev/console";
/// Compatibility path-id constant used by mount/policy/interop tests.
pub const DEV_NULL_PATH_PTR: u64 = 0x4445_564E_554C_4C00;
pub const DEV_NULL_PATH: &[u8] = b"/dev/null";

const MAX_OPEN_HANDLES: usize = 16;
const MAX_DYNAMIC_NODES: usize = 16;
/// Maximum length of a stored `/dev/NAME` path.
const MAX_NODE_PATH_LEN: usize = 64;

const DEVFS_STATX_TYPE_CHAR_DEVICE: u64 = 0x2000_0000_0000_0000;
const DEVFS_STATX_TYPE_BLOCK_DEVICE: u64 = 0x6000_0000_0000_0000;
const DEVFS_MODE_OWNER_READ: u64 = 0o400;
const DEVFS_MODE_OWNER_WRITE: u64 = 0o200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DevNode {
    Console,
    Null,
    Dynamic(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenHandle {
    fd: u64,
    node: DevNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DynamicDevNodeEntry {
    path: [u8; MAX_NODE_PATH_LEN],
    path_len: usize,
    kind: u32,
    flags: u32,
    backend_cap: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevNodeRegisterError {
    TableFull,
    Duplicate,
    InvalidPath,
    NameTooLong,
    InvalidKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DevFsMetrics {
    pub open_count: u64,
    pub close_count: u64,
    pub read_count: u64,
    pub write_count: u64,
    pub statx_count: u64,
    pub console_bytes_written: u64,
    pub null_bytes_written: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevFsBackend {
    next_fd: u64,
    handles: [Option<OpenHandle>; MAX_OPEN_HANDLES],
    metrics: DevFsMetrics,
    dynamic_nodes: [Option<DynamicDevNodeEntry>; MAX_DYNAMIC_NODES],
    dynamic_count: usize,
}

impl Default for DevFsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DevFsBackend {
    pub const fn new() -> Self {
        Self {
            next_fd: 3,
            handles: [None; MAX_OPEN_HANDLES],
            metrics: DevFsMetrics {
                open_count: 0,
                close_count: 0,
                read_count: 0,
                write_count: 0,
                statx_count: 0,
                console_bytes_written: 0,
                null_bytes_written: 0,
                error_count: 0,
            },
            dynamic_nodes: [None; MAX_DYNAMIC_NODES],
            dynamic_count: 0,
        }
    }

    pub const fn metrics(&self) -> DevFsMetrics {
        self.metrics
    }

    /// Register a dynamic device node.
    ///
    /// `path` may be `/dev/NAME` or bare `NAME`; both are accepted and stored
    /// as `/dev/NAME`.  Returns an error if the kind is unrecognised, the name
    /// is too long, the path is otherwise invalid, the node already exists
    /// (including built-ins), or the dynamic-node table is full.
    pub fn register_dynamic_node(
        &mut self,
        path: &[u8],
        kind: u32,
        flags: u32,
        backend_cap: u64,
    ) -> Result<(), DevNodeRegisterError> {
        if kind != DEVFS_NODE_KIND_CHAR && kind != DEVFS_NODE_KIND_BLOCK {
            return Err(DevNodeRegisterError::InvalidKind);
        }
        let (full_path, full_len) = Self::normalize_node_path(path)?;

        // Reject duplicates against built-ins.
        if &full_path[..full_len] == DEV_CONSOLE_PATH
            || &full_path[..full_len] == DEV_NULL_PATH
        {
            return Err(DevNodeRegisterError::Duplicate);
        }

        // Reject duplicates against already-registered dynamic nodes.
        for i in 0..self.dynamic_count {
            if let Some(entry) = self.dynamic_nodes[i] {
                if &entry.path[..entry.path_len] == &full_path[..full_len] {
                    return Err(DevNodeRegisterError::Duplicate);
                }
            }
        }

        if self.dynamic_count >= MAX_DYNAMIC_NODES {
            return Err(DevNodeRegisterError::TableFull);
        }

        self.dynamic_nodes[self.dynamic_count] = Some(DynamicDevNodeEntry {
            path: full_path,
            path_len: full_len,
            kind,
            flags,
            backend_cap,
        });
        self.dynamic_count += 1;
        Ok(())
    }

    /// Normalize `path` to an absolute `/dev/NAME` representation stored in a
    /// fixed-size buffer.  Accepts `/dev/NAME` or bare `NAME`.  Rejects paths
    /// that contain a `/` in the name part, paths that start with a different
    /// prefix, and names that would exceed `MAX_NODE_PATH_LEN`.
    fn normalize_node_path(
        input: &[u8],
    ) -> Result<([u8; MAX_NODE_PATH_LEN], usize), DevNodeRegisterError> {
        const DEV_PREFIX: &[u8] = b"/dev/";

        let name = if input.starts_with(DEV_PREFIX) {
            &input[DEV_PREFIX.len()..]
        } else if input.starts_with(b"/") {
            // Absolute path not under /dev/ — reject.
            return Err(DevNodeRegisterError::InvalidPath);
        } else {
            // Bare name — treat as relative to /dev/.
            input
        };

        if name.is_empty() || name.iter().any(|&b| b == b'/') {
            return Err(DevNodeRegisterError::InvalidPath);
        }

        let full_len = DEV_PREFIX.len() + name.len();
        if full_len > MAX_NODE_PATH_LEN {
            return Err(DevNodeRegisterError::NameTooLong);
        }

        let mut buf = [0u8; MAX_NODE_PATH_LEN];
        buf[..DEV_PREFIX.len()].copy_from_slice(DEV_PREFIX);
        buf[DEV_PREFIX.len()..full_len].copy_from_slice(name);
        Ok((buf, full_len))
    }

    fn lookup_by_path(&self, path: &[u8]) -> Result<DevNode, VfsError> {
        if path == DEV_CONSOLE_PATH {
            return Ok(DevNode::Console);
        }
        if path == DEV_NULL_PATH {
            return Ok(DevNode::Null);
        }
        for i in 0..self.dynamic_count {
            if let Some(entry) = self.dynamic_nodes[i] {
                if &entry.path[..entry.path_len] == path {
                    return Ok(DevNode::Dynamic(i));
                }
            }
        }
        Err(VfsError::InvalidPath)
    }

    fn metadata_by_path(&self, path: &[u8]) -> Result<u64, VfsError> {
        let node = self.lookup_by_path(path)?;
        Ok(self.statx_for_node(node))
    }

    fn alloc_handle(&mut self, node: DevNode) -> Result<u64, VfsError> {
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        if let Some(slot) = self.handles.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(OpenHandle { fd, node });
            return Ok(fd);
        }
        Err(VfsError::NoFd)
    }

    fn node_for_fd(&self, fd: u64) -> Result<DevNode, VfsError> {
        self.handles
            .iter()
            .flatten()
            .find(|handle| handle.fd == fd)
            .map(|handle| handle.node)
            .ok_or(VfsError::BadFd)
    }

    fn close_fd(&mut self, fd: u64) -> Result<(), VfsError> {
        if let Some(slot) = self
            .handles
            .iter_mut()
            .find(|slot| slot.map(|handle| handle.fd == fd).unwrap_or(false))
        {
            *slot = None;
            return Ok(());
        }
        Err(VfsError::BadFd)
    }

    fn statx_for_node(&self, node: DevNode) -> u64 {
        match node {
            DevNode::Console => DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_WRITE,
            DevNode::Null => {
                DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
            }
            DevNode::Dynamic(i) => {
                let type_bits = self.dynamic_nodes[i]
                    .map(|e| {
                        if e.kind == DEVFS_NODE_KIND_BLOCK {
                            DEVFS_STATX_TYPE_BLOCK_DEVICE
                        } else {
                            DEVFS_STATX_TYPE_CHAR_DEVICE
                        }
                    })
                    .unwrap_or(DEVFS_STATX_TYPE_CHAR_DEVICE);
                type_bits | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
            }
        }
    }
}

impl VfsBackend for DevFsBackend {
    fn openat_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        let node = self.lookup_by_path(path);
        match node.and_then(|n| self.alloc_handle(n)) {
            Ok(fd) => {
                self.metrics.open_count = self.metrics.open_count.saturating_add(1);
                Ok(fd)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn close(&mut self, fd: u64) -> Result<u64, VfsError> {
        match self.close_fd(fd) {
            Ok(()) => {
                self.metrics.close_count = self.metrics.close_count.saturating_add(1);
                Ok(0)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn read(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        match self.node_for_fd(fd) {
            Ok(DevNode::Null) | Ok(DevNode::Dynamic(_)) => {
                self.metrics.read_count = self.metrics.read_count.saturating_add(1);
                let _ = len;
                Ok(0)
            }
            Ok(DevNode::Console) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(VfsError::Unsupported)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn write(&mut self, fd: u64, len: u64) -> Result<u64, VfsError> {
        match self.node_for_fd(fd) {
            Ok(DevNode::Console) => {
                self.metrics.write_count = self.metrics.write_count.saturating_add(1);
                self.metrics.console_bytes_written =
                    self.metrics.console_bytes_written.saturating_add(len);
                Ok(len)
            }
            Ok(DevNode::Null) => {
                self.metrics.write_count = self.metrics.write_count.saturating_add(1);
                self.metrics.null_bytes_written =
                    self.metrics.null_bytes_written.saturating_add(len);
                Ok(len)
            }
            Ok(DevNode::Dynamic(_)) => {
                self.metrics.write_count = self.metrics.write_count.saturating_add(1);
                Ok(len)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }

    fn statx_path(&mut self, path: &[u8]) -> Result<u64, VfsError> {
        match self.metadata_by_path(path) {
            Ok(stat) => {
                self.metrics.statx_count = self.metrics.statx_count.saturating_add(1);
                Ok(stat)
            }
            Err(err) => {
                self.metrics.error_count = self.metrics.error_count.saturating_add(1);
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_open_allocates_unique_handles() {
        let mut backend = DevFsBackend::new();
        let fd0 = backend.openat_path(DEV_CONSOLE_PATH).expect("console open");
        let fd1 = backend.openat_path(DEV_CONSOLE_PATH).expect("console open");
        let fd2 = backend.openat_path(DEV_NULL_PATH).expect("null open");
        assert_ne!(fd0, fd1);
        assert_ne!(fd1, fd2);
        assert_eq!(fd0, 3);
        assert_eq!(fd1, 4);
        assert_eq!(fd2, 5);
    }

    #[test]
    fn node_specific_read_write_semantics_are_enforced() {
        let mut backend = DevFsBackend::new();
        let console_fd = backend.openat_path(DEV_CONSOLE_PATH).expect("console");
        let null_fd = backend.openat_path(DEV_NULL_PATH).expect("null");
        assert_eq!(backend.write(console_fd, 11), Ok(11));
        assert_eq!(backend.write(null_fd, 7), Ok(7));
        assert_eq!(backend.read(null_fd, 64), Ok(0));
        assert_eq!(backend.read(console_fd, 64), Err(VfsError::Unsupported));
    }

    #[test]
    fn statx_contract_returns_node_specific_metadata() {
        let mut backend = DevFsBackend::new();
        let console = backend.statx_path(DEV_CONSOLE_PATH).expect("console stat");
        let null = backend.statx_path(DEV_NULL_PATH).expect("null stat");
        assert_eq!(
            console,
            DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_WRITE
        );
        assert_eq!(
            null,
            DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
        );
    }

    #[test]
    fn metrics_track_success_and_error_paths() {
        let mut backend = DevFsBackend::new();
        let console_fd = backend.openat_path(DEV_CONSOLE_PATH).expect("console");
        let null_fd = backend.openat_path(DEV_NULL_PATH).expect("null");
        let _ = backend.write(console_fd, 8).expect("console write");
        let _ = backend.write(null_fd, 5).expect("null write");
        let _ = backend
            .read(console_fd, 1)
            .expect_err("console read unsupported");
        let _ = backend.read(null_fd, 2).expect("null read");
        let _ = backend.close(console_fd).expect("close console");
        let _ = backend.close(null_fd).expect("close null");
        let _ = backend.close(999).expect_err("close bad fd");

        let metrics = backend.metrics();
        assert_eq!(metrics.open_count, 2);
        assert_eq!(metrics.close_count, 2);
        assert_eq!(metrics.read_count, 1);
        assert_eq!(metrics.write_count, 2);
        assert_eq!(metrics.console_bytes_written, 8);
        assert_eq!(metrics.null_bytes_written, 5);
        assert_eq!(metrics.error_count, 2);
    }

    #[test]
    fn byte_path_open_and_statx_work_for_known_nodes() {
        let mut backend = DevFsBackend::new();
        let console_fd = backend.openat_path(DEV_CONSOLE_PATH).expect("console open");
        let null_stat = backend.statx_path(DEV_NULL_PATH).expect("null stat");
        assert_eq!(console_fd, 3);
        assert_eq!(
            null_stat,
            DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
        );
    }

    #[test]
    fn byte_path_lookup_rejects_unknown_paths() {
        let mut backend = DevFsBackend::new();
        assert_eq!(backend.openat_path(b"/dev/unknown"), Err(VfsError::InvalidPath));
        assert_eq!(backend.statx_path(b"/dev/unknown"), Err(VfsError::InvalidPath));
    }

    // ── Dynamic node registration ───────────────────────────────────────────

    #[test]
    fn register_char_node_by_full_path_is_visible_via_statx_and_open() {
        let mut backend = DevFsBackend::new();
        backend
            .register_dynamic_node(b"/dev/uart0", DEVFS_NODE_KIND_CHAR, 0, 99)
            .expect("register uart0");
        let stat = backend.statx_path(b"/dev/uart0").expect("statx uart0");
        assert_eq!(
            stat,
            DEVFS_STATX_TYPE_CHAR_DEVICE | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
        );
        let fd = backend.openat_path(b"/dev/uart0").expect("open uart0");
        assert!(fd >= 3);
    }

    #[test]
    fn register_block_node_reports_block_device_type() {
        let mut backend = DevFsBackend::new();
        backend
            .register_dynamic_node(b"/dev/blk0", DEVFS_NODE_KIND_BLOCK, 0, 100)
            .expect("register blk0");
        let stat = backend.statx_path(b"/dev/blk0").expect("statx blk0");
        assert_eq!(
            stat,
            DEVFS_STATX_TYPE_BLOCK_DEVICE | DEVFS_MODE_OWNER_READ | DEVFS_MODE_OWNER_WRITE
        );
    }

    #[test]
    fn register_by_bare_name_normalizes_to_dev_prefix() {
        let mut backend = DevFsBackend::new();
        backend
            .register_dynamic_node(b"tty0", DEVFS_NODE_KIND_CHAR, 0, 55)
            .expect("register by bare name");
        // Must be visible under the full /dev/ path.
        assert!(backend.statx_path(b"/dev/tty0").is_ok());
        // The bare name is not a valid VFS path.
        assert_eq!(backend.statx_path(b"tty0"), Err(VfsError::InvalidPath));
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut backend = DevFsBackend::new();
        backend
            .register_dynamic_node(b"/dev/uart0", DEVFS_NODE_KIND_CHAR, 0, 1)
            .expect("first register");
        let err = backend
            .register_dynamic_node(b"/dev/uart0", DEVFS_NODE_KIND_CHAR, 0, 2)
            .expect_err("duplicate");
        assert_eq!(err, DevNodeRegisterError::Duplicate);
    }

    #[test]
    fn duplicate_against_builtin_is_rejected() {
        let mut backend = DevFsBackend::new();
        assert_eq!(
            backend.register_dynamic_node(b"/dev/null", DEVFS_NODE_KIND_CHAR, 0, 1),
            Err(DevNodeRegisterError::Duplicate)
        );
        assert_eq!(
            backend.register_dynamic_node(b"/dev/console", DEVFS_NODE_KIND_CHAR, 0, 1),
            Err(DevNodeRegisterError::Duplicate)
        );
    }

    #[test]
    fn invalid_kind_is_rejected() {
        let mut backend = DevFsBackend::new();
        assert_eq!(
            backend.register_dynamic_node(b"/dev/uart0", 99, 0, 1),
            Err(DevNodeRegisterError::InvalidKind)
        );
    }

    #[test]
    fn non_dev_absolute_path_is_rejected() {
        let mut backend = DevFsBackend::new();
        assert_eq!(
            backend.register_dynamic_node(b"/proc/uart0", DEVFS_NODE_KIND_CHAR, 0, 1),
            Err(DevNodeRegisterError::InvalidPath)
        );
    }

    #[test]
    fn path_with_slash_in_name_is_rejected() {
        let mut backend = DevFsBackend::new();
        assert_eq!(
            backend.register_dynamic_node(b"/dev/sub/uart0", DEVFS_NODE_KIND_CHAR, 0, 1),
            Err(DevNodeRegisterError::InvalidPath)
        );
    }

    #[test]
    fn table_full_error_when_max_nodes_reached() {
        let mut backend = DevFsBackend::new();
        for i in 0..MAX_DYNAMIC_NODES {
            // Build "/dev/nXX" as a fixed-size stack array.
            let mut path = [0u8; 9];
            path[0] = b'/';
            path[1] = b'd';
            path[2] = b'e';
            path[3] = b'v';
            path[4] = b'/';
            path[5] = b'n';
            path[6] = b'0' + (i / 10) as u8;
            path[7] = b'0' + (i % 10) as u8;
            assert!(
                backend
                    .register_dynamic_node(&path[..8], DEVFS_NODE_KIND_CHAR, 0, i as u64)
                    .is_ok(),
                "slot {i} should succeed"
            );
        }
        assert_eq!(
            backend.register_dynamic_node(b"/dev/overflow", DEVFS_NODE_KIND_CHAR, 0, 99),
            Err(DevNodeRegisterError::TableFull)
        );
    }

    #[test]
    fn dynamic_node_read_returns_zero_write_returns_len() {
        let mut backend = DevFsBackend::new();
        backend
            .register_dynamic_node(b"/dev/uart0", DEVFS_NODE_KIND_CHAR, 0, 1)
            .expect("register");
        let fd = backend.openat_path(b"/dev/uart0").expect("open");
        assert_eq!(backend.read(fd, 128), Ok(0));
        assert_eq!(backend.write(fd, 64), Ok(64));
    }

    #[test]
    fn builtin_nodes_still_work_after_dynamic_registration() {
        let mut backend = DevFsBackend::new();
        backend
            .register_dynamic_node(b"/dev/uart0", DEVFS_NODE_KIND_CHAR, 0, 1)
            .expect("register");
        let console_fd = backend.openat_path(DEV_CONSOLE_PATH).expect("console");
        let null_fd = backend.openat_path(DEV_NULL_PATH).expect("null");
        assert_eq!(backend.write(console_fd, 5), Ok(5));
        assert_eq!(backend.read(null_fd, 10), Ok(0));
    }
}
