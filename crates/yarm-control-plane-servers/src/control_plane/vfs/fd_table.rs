// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::mount_table::MountLabel;

/// Maximum number of simultaneously tracked open file descriptors.
pub const MAX_FD_ENTRIES: usize = 32;

#[derive(Clone, Copy)]
struct FdEntry {
    fd: u64,
    /// Identity of the owning client (source TID encoded as the reply-cap
    /// number minted by the kernel for the OPENAT call).  Only the client
    /// whose `client_id` matches may look up or close this entry.
    client_id: u64,
    backend_cap: u32,
    label: MountLabel,
}

/// Per-session fd → backend routing table for vfs_server.
///
/// Each entry is scoped to a `(fd, client_id)` pair so that two different
/// clients can hold the same numeric fd without interfering with each other.
/// `client_id` is the reply-cap value the kernel delivered alongside the
/// OPENAT request; in this microkernel's IPC model that number encodes the
/// calling thread's identity and remains stable across calls from the same
/// thread.
///
/// When vfs_server forwards a `VFS_OP_OPENAT` to a backend and receives
/// the reply fd, it inserts `(fd, client_id, backend_cap, label)` here.
/// Subsequent fd-bearing operations (READ, CLOSE, …) pass their own
/// `client_id`; only matching entries are returned or removed.
pub struct VfsFdTable {
    entries: [FdEntry; MAX_FD_ENTRIES],
    count: usize,
}

impl VfsFdTable {
    const EMPTY: FdEntry = FdEntry {
        fd: u64::MAX,
        client_id: 0,
        backend_cap: 0,
        label: MountLabel::EMPTY,
    };

    pub const fn new() -> Self {
        Self {
            entries: [Self::EMPTY; MAX_FD_ENTRIES],
            count: 0,
        }
    }

    /// Record `fd` as belonging to `client_id` via `backend_cap`.
    ///
    /// Returns `false` if the table is full.  Duplicate `(fd, client_id)`
    /// pairs are not deduplicated; callers should close before re-opening
    /// the same fd.
    pub fn insert(&mut self, fd: u64, backend_cap: u32, name: &str, client_id: u64) -> bool {
        if self.count >= MAX_FD_ENTRIES {
            return false;
        }
        self.entries[self.count] = FdEntry {
            fd,
            client_id,
            backend_cap,
            label: MountLabel::from_str(name),
        };
        self.count += 1;
        true
    }

    /// Return `(backend_cap, label)` for the entry whose `fd` **and**
    /// `client_id` both match.  Returns `None` if no such entry exists,
    /// including when another client holds the same fd number.
    pub fn lookup(&self, fd: u64, client_id: u64) -> Option<(u32, MountLabel)> {
        for entry in &self.entries[..self.count] {
            if entry.fd == fd && entry.client_id == client_id {
                return Some((entry.backend_cap, entry.label));
            }
        }
        None
    }

    /// Remove the entry whose `fd` **and** `client_id` both match
    /// (swap-remove for O(1) deletion).  A different client's entry for
    /// the same fd is left untouched.
    pub fn remove(&mut self, fd: u64, client_id: u64) {
        for i in 0..self.count {
            if self.entries[i].fd == fd && self.entries[i].client_id == client_id {
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

    const CLIENT_A: u64 = 100;
    const CLIENT_B: u64 = 200;

    #[test]
    fn fd_table_insert_and_lookup() {
        let mut t = VfsFdTable::new();
        assert!(t.insert(3, 10, "initramfs", CLIENT_A));
        assert!(t.insert(4, 20, "devfs", CLIENT_A));
        assert_eq!(t.len(), 2);

        let (cap, label) = t.lookup(3, CLIENT_A).unwrap();
        assert_eq!(cap, 10);
        assert_eq!(label.as_str(), "initramfs");

        let (cap, label) = t.lookup(4, CLIENT_A).unwrap();
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");
    }

    #[test]
    fn fd_table_lookup_missing_returns_none() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "x", CLIENT_A);
        assert!(t.lookup(99, CLIENT_A).is_none());
        assert!(t.lookup(0, CLIENT_A).is_none());
    }

    #[test]
    fn fd_table_remove_shrinks_count() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "a", CLIENT_A);
        t.insert(4, 20, "b", CLIENT_A);
        t.remove(3, CLIENT_A);
        assert_eq!(t.len(), 1);
        assert!(t.lookup(3, CLIENT_A).is_none());
        assert!(t.lookup(4, CLIENT_A).is_some());
    }

    #[test]
    fn fd_table_remove_nonexistent_is_noop() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "a", CLIENT_A);
        t.remove(99, CLIENT_A);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn fd_table_is_empty_when_new() {
        let t = VfsFdTable::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(t.lookup(3, CLIENT_A).is_none());
    }

    #[test]
    fn fd_table_caps_at_max_entries() {
        let mut t = VfsFdTable::new();
        for i in 0..MAX_FD_ENTRIES {
            assert!(t.insert(i as u64, (i + 1) as u32, "x", CLIENT_A));
        }
        assert_eq!(t.len(), MAX_FD_ENTRIES);
        assert!(!t.insert(999, 1, "overflow", CLIENT_A));
    }

    #[test]
    fn fd_table_remove_then_insert_reuses_slot() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "a", CLIENT_A);
        t.remove(3, CLIENT_A);
        assert!(t.insert(3, 20, "b", CLIENT_A));
        assert_eq!(t.len(), 1);
        let (cap, _) = t.lookup(3, CLIENT_A).unwrap();
        assert_eq!(cap, 20);
    }

    #[test]
    fn fd_table_label_is_stored_and_retrieved() {
        let mut t = VfsFdTable::new();
        t.insert(7, 99, "initramfs", CLIENT_A);
        let (_, label) = t.lookup(7, CLIENT_A).unwrap();
        assert_eq!(label.as_str(), "initramfs");
    }

    #[test]
    fn fd_table_label_truncates_beyond_max() {
        // Names longer than 32 bytes are silently truncated.
        let long_name = "a".repeat(64);
        let mut t = VfsFdTable::new();
        t.insert(1, 1, &long_name, CLIENT_A);
        let (_, label) = t.lookup(1, CLIENT_A).unwrap();
        assert_eq!(label.as_str().len(), 32);
    }

    // ── fd isolation tests ──────────────────────────────────────────────────

    #[test]
    fn fd_isolation_same_fd_number_coexists_for_different_clients() {
        let mut t = VfsFdTable::new();
        // Both clients open fd=3 — to different backends.
        assert!(t.insert(3, 10, "initramfs", CLIENT_A));
        assert!(t.insert(3, 20, "devfs", CLIENT_B));
        assert_eq!(t.len(), 2);

        let (cap_a, label_a) = t.lookup(3, CLIENT_A).unwrap();
        let (cap_b, label_b) = t.lookup(3, CLIENT_B).unwrap();
        assert_eq!(cap_a, 10);
        assert_eq!(label_a.as_str(), "initramfs");
        assert_eq!(cap_b, 20);
        assert_eq!(label_b.as_str(), "devfs");
    }

    #[test]
    fn fd_isolation_lookup_by_wrong_client_returns_none() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "initramfs", CLIENT_A);

        // Client B cannot look up Client A's fd.
        assert!(t.lookup(3, CLIENT_B).is_none());
        // Client A can still look up their own fd.
        assert!(t.lookup(3, CLIENT_A).is_some());
    }

    #[test]
    fn fd_isolation_close_by_wrong_client_is_noop() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "initramfs", CLIENT_A);

        // Client B attempts to close fd=3; must have no effect.
        t.remove(3, CLIENT_B);
        assert_eq!(t.len(), 1);
        // Client A's entry is untouched.
        assert!(t.lookup(3, CLIENT_A).is_some());
    }

    #[test]
    fn fd_isolation_close_by_owner_does_not_affect_other_client() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "initramfs", CLIENT_A);
        t.insert(3, 20, "devfs", CLIENT_B);

        // Client A closes their fd=3.
        t.remove(3, CLIENT_A);
        assert_eq!(t.len(), 1);

        // Client A can no longer look it up.
        assert!(t.lookup(3, CLIENT_A).is_none());
        // Client B's fd=3 is unaffected.
        let (cap, label) = t.lookup(3, CLIENT_B).unwrap();
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");
    }

    #[test]
    fn fd_isolation_multiple_fds_per_client_are_independent() {
        let mut t = VfsFdTable::new();
        t.insert(3, 10, "initramfs", CLIENT_A);
        t.insert(4, 11, "devfs", CLIENT_A);
        t.insert(3, 20, "ramfs", CLIENT_B);
        t.insert(5, 21, "initramfs", CLIENT_B);
        assert_eq!(t.len(), 4);

        // Client A closes fd=3; Client B's fd=3 and all of CLIENT_A fd=4 survive.
        t.remove(3, CLIENT_A);
        assert_eq!(t.len(), 3);
        assert!(t.lookup(3, CLIENT_A).is_none());
        assert!(t.lookup(4, CLIENT_A).is_some());
        assert!(t.lookup(3, CLIENT_B).is_some());
        assert!(t.lookup(5, CLIENT_B).is_some());
    }

    #[test]
    fn fd_isolation_caps_at_max_with_two_clients() {
        let mut t = VfsFdTable::new();
        // Fill half from CLIENT_A, half from CLIENT_B.
        for i in 0..(MAX_FD_ENTRIES / 2) {
            assert!(t.insert(i as u64, 1, "a", CLIENT_A));
            assert!(t.insert(i as u64, 2, "b", CLIENT_B));
        }
        assert_eq!(t.len(), MAX_FD_ENTRIES);
        // Table is full; neither client can insert another.
        assert!(!t.insert(999, 3, "overflow", CLIENT_A));
        assert!(!t.insert(999, 3, "overflow", CLIENT_B));

        // After Client A closes one fd the table has room again.
        t.remove(0, CLIENT_A);
        assert_eq!(t.len(), MAX_FD_ENTRIES - 1);
        assert!(t.insert(999, 3, "new", CLIENT_A));
    }
}
