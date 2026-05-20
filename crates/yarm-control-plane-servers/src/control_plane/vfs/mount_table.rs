// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub const MAX_MOUNT_ENTRIES: usize = 8;

#[derive(Clone, Copy)]
struct MountEntry {
    prefix: &'static [u8],
    name: &'static str,
    send_cap: u32,
}

/// Table-driven VFS mount router.
///
/// Entries are registered with a path prefix and a send capability.
/// `route()` performs a longest-prefix match and returns the matching
/// send cap and mount name, or `None` if no entry covers the path.
pub struct VfsMountTable {
    entries: [MountEntry; MAX_MOUNT_ENTRIES],
    count: usize,
}

impl VfsMountTable {
    const EMPTY: MountEntry = MountEntry {
        prefix: b"",
        name: "",
        send_cap: 0,
    };

    pub const fn new() -> Self {
        Self {
            entries: [Self::EMPTY; MAX_MOUNT_ENTRIES],
            count: 0,
        }
    }

    /// Register a mount entry.
    ///
    /// Returns `false` if the table is full or `send_cap` is 0 (invalid cap).
    pub fn register(
        &mut self,
        prefix: &'static [u8],
        name: &'static str,
        send_cap: u32,
    ) -> bool {
        if self.count >= MAX_MOUNT_ENTRIES || send_cap == 0 {
            return false;
        }
        self.entries[self.count] = MountEntry {
            prefix,
            name,
            send_cap,
        };
        self.count += 1;
        true
    }

    /// Longest-prefix match against the registered mount entries.
    ///
    /// Returns `Some((send_cap, name))` for the entry whose prefix is the
    /// longest prefix of `path`, or `None` if no entry matches.
    pub fn route(&self, path: &[u8]) -> Option<(u32, &'static str)> {
        let mut best_prefix_len = 0usize;
        let mut best: Option<(u32, &'static str)> = None;
        for entry in &self.entries[..self.count] {
            if entry.prefix.is_empty() {
                continue;
            }
            if path.starts_with(entry.prefix) && entry.prefix.len() > best_prefix_len {
                best_prefix_len = entry.prefix.len();
                best = Some((entry.send_cap, entry.name));
            }
        }
        best
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
    fn mount_table_routes_initramfs_and_dev_prefixes() {
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/initramfs/", "initramfs", 10));
        assert!(table.register(b"/dev/", "devfs", 20));
        assert_eq!(table.len(), 2);
        assert!(!table.is_empty());

        let (cap, name) = table.route(b"/initramfs/boot-marker").unwrap();
        assert_eq!(cap, 10);
        assert_eq!(name, "initramfs");

        let (cap, name) = table.route(b"/dev/null").unwrap();
        assert_eq!(cap, 20);
        assert_eq!(name, "devfs");
    }

    #[test]
    fn mount_table_returns_none_for_unregistered_path() {
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/initramfs/", "initramfs", 10));

        assert!(table.route(b"/proc/1").is_none());
        assert!(table.route(b"").is_none());
        assert!(table.route(b"/dev/null").is_none());
    }

    #[test]
    fn mount_table_longest_prefix_wins() {
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/dev/", "devfs", 1));
        assert!(table.register(b"/dev/pts/", "devpts", 2));

        let (cap, name) = table.route(b"/dev/pts/0").unwrap();
        assert_eq!(cap, 2);
        assert_eq!(name, "devpts");

        let (cap, name) = table.route(b"/dev/null").unwrap();
        assert_eq!(cap, 1);
        assert_eq!(name, "devfs");
    }

    #[test]
    fn mount_table_rejects_zero_send_cap() {
        let mut table = VfsMountTable::new();
        assert!(!table.register(b"/dev/", "devfs", 0));
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn mount_table_is_empty_when_new() {
        let table = VfsMountTable::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        assert!(table.route(b"/dev/null").is_none());
    }

    #[test]
    fn mount_table_caps_at_max_entries() {
        const PREFIXES: [&[u8]; MAX_MOUNT_ENTRIES] = [
            b"/a/", b"/b/", b"/c/", b"/d/", b"/e/", b"/f/", b"/g/", b"/h/",
        ];
        let mut table = VfsMountTable::new();
        for (i, &prefix) in PREFIXES.iter().enumerate() {
            assert!(table.register(prefix, "x", (i + 1) as u32));
        }
        assert_eq!(table.len(), MAX_MOUNT_ENTRIES);
        assert!(!table.register(b"/z/", "z", 99));
    }

    #[test]
    fn mount_table_exact_prefix_boundary_is_respected() {
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/dev/", "devfs", 5));

        // Path that starts with /dev/ → matches
        assert!(table.route(b"/dev/console").is_some());
        // Path equal to prefix → starts_with succeeds
        assert!(table.route(b"/dev/").is_some());
        // Path /dev (no trailing slash) → does NOT start with b"/dev/"
        assert!(table.route(b"/dev").is_none());
    }
}
