// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::mount_table::MountLabel;

/// Maximum number of simultaneously tracked open file descriptors.
pub const MAX_FD_ENTRIES: usize = 32;

#[derive(Clone, Copy)]
struct FdEntry {
    fd: u64,
    backend_cap: u32,
    label: MountLabel,
}

/// Per-session fd → backend routing table for vfs_server.
///
/// When vfs_server forwards a `VFS_OP_OPENAT` to a backend and receives
/// the reply fd, it inserts `(fd, backend_cap, name)` here.  Subsequent
/// fd-bearing operations (READ, WRITE, CLOSE, …) look up the fd to
/// determine which backend to forward to.
pub struct VfsFdTable {
    entries: [FdEntry; MAX_FD_ENTRIES],
    count: usize,
}

impl VfsFdTable {
    const EMPTY: FdEntry = FdEntry {
        fd: u64::MAX,
        backend_cap: 0,
        label: MountLabel::EMPTY,
    };

    pub const fn new() -> Self {
        Self {
            entries: [Self::EMPTY; MAX_FD_ENTRIES],
            count: 0,
        }
    }

    /// Record `fd` as belonging to the backend reachable via `backend_cap`.
    ///
    /// Returns `false` if the table is full.  Duplicate fds are not
    /// deduplicated; callers should close before re-opening the same fd.
    pub fn insert(&mut self, fd: u64, backend_cap: u32, name: &str) -> bool {
        if self.count >= MAX_FD_ENTRIES {
            return false;
        }
        self.entries[self.count] = FdEntry {
            fd,
            backend_cap,
            label: MountLabel::from_str(name),
        };
        self.count += 1;
        true
    }

    /// Return a copy of `(backend_cap, label)` for the first entry matching `fd`.
    pub fn lookup(&self, fd: u64) -> Option<(u32, MountLabel)> {
        for entry in &self.entries[..self.count] {
            if entry.fd == fd {
                return Some((entry.backend_cap, entry.label));
            }
        }
        None
    }

    /// Remove the entry for `fd` (swap-remove for O(1) deletion).
    pub fn remove(&mut self, fd: u64) {
        for i in 0..self.count {
            if self.entries[i].fd == fd {
                self.count -= 1;
                self.entries[i] = self.entries[self.count];
                self.entries[self.count] = Self::EMPTY;
                return;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fd_table_insert_and_lookup() {
        let mut t = VfsFdTable::new();
        assert!(t.insert(3, 10, "initramfs"));
        assert!(t.insert(4, 20, "devfs"));
        assert_eq!(t.len(), 2);

        let (cap, label) = t.lookup(3).unwrap();
        assert_eq!(cap, 10);
        assert_eq!(label.as_str(), "initramfs");

        let (cap, label) = t.lookup(4).unwrap();
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");
    }

    #[test]
    fn fd_table_lookup_missing_returns_none() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "x");
        assert!(t.lookup(99).is_none());
        assert!(t.lookup(0).is_none());
    }

    #[test]
    fn fd_table_remove_shrinks_count() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "a");
        t.insert(4, 20, "b");
        t.remove(3);
        assert_eq!(t.len(), 1);
        assert!(t.lookup(3).is_none());
        assert!(t.lookup(4).is_some());
    }

    #[test]
    fn fd_table_remove_nonexistent_is_noop() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "a");
        t.remove(99);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn fd_table_is_empty_when_new() {
        let t = VfsFdTable::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(t.lookup(3).is_none());
    }

    #[test]
    fn fd_table_caps_at_max_entries() {
        let mut t = VfsFdTable::new();
        for i in 0..MAX_FD_ENTRIES {
            assert!(t.insert(i as u64, (i + 1) as u32, "x"));
        }
        assert_eq!(t.len(), MAX_FD_ENTRIES);
        assert!(!t.insert(999, 1, "overflow"));
    }

    #[test]
    fn fd_table_remove_then_insert_reuses_slot() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "a");
        t.remove(3);
        assert!(t.insert(3, 20, "b"));
        assert_eq!(t.len(), 1);
        let (cap, _) = t.lookup(3).unwrap();
        assert_eq!(cap, 20);
    }

    #[test]
    fn fd_table_label_is_stored_and_retrieved() {
        let mut t = VfsFdTable::new();
        t.insert(7, 99, "initramfs");
        let (_, label) = t.lookup(7).unwrap();
        assert_eq!(label.as_str(), "initramfs");
    }

    #[test]
    fn fd_table_label_truncates_beyond_max() {
        // Names longer than 32 bytes are silently truncated.
        let long_name = "a".repeat(64);
        let mut t = VfsFdTable::new();
        t.insert(1, 1, &long_name);
        let (_, label) = t.lookup(1).unwrap();
        assert_eq!(label.as_str().len(), 32);
    }
}
