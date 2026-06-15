// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub const MAX_MOUNT_ENTRIES: usize = 8;

/// Maximum byte length of a stored prefix (normalized path + trailing slash).
/// 96 bytes normalized + 1 for the appended `/` = 97.
pub const MAX_PREFIX_LEN: usize = 97;

const MAX_LABEL_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountRegisterError {
    TableFull,
    DuplicatePrefix,
    InvalidSendCap,
    PrefixTooLong,
    InvalidPrefix,
}

/// Compact owned label (≤ 32 bytes UTF-8) returned from routing lookups.
///
/// Owns its bytes so that callers are free to mutate the mount table or fd
/// table after the lookup without lifetime conflicts.
#[derive(Clone, Copy)]
pub(crate) struct MountLabel {
    bytes: [u8; MAX_LABEL_LEN],
    len: usize,
}

impl MountLabel {
    pub(crate) const EMPTY: Self = Self {
        bytes: [0u8; MAX_LABEL_LEN],
        len: 0,
    };

    pub(crate) fn from_bytes(src: &[u8]) -> Self {
        let mut out = Self::EMPTY;
        let n = src.len().min(MAX_LABEL_LEN);
        out.bytes[..n].copy_from_slice(&src[..n]);
        out.len = n;
        out
    }

    pub(crate) fn from_str(src: &str) -> Self {
        Self::from_bytes(src.as_bytes())
    }

    pub(crate) fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("?")
    }
}

#[derive(Clone, Copy)]
struct MountEntry {
    prefix: [u8; MAX_PREFIX_LEN],
    prefix_len: usize,
    label: MountLabel,
    send_cap: u32,
    flags: u32,
}

/// Table-driven VFS mount router.
///
/// Entries are registered either at boot time via `register()` (trusted static
/// data, no normalization) or dynamically at runtime via `insert_dynamic()`
/// (path is normalized and validated before insertion).
///
/// `route()` performs a longest-prefix match and returns a copy of the matching
/// send cap and label, or `None` if no entry covers the path.
pub struct VfsMountTable {
    entries: [MountEntry; MAX_MOUNT_ENTRIES],
    count: usize,
}

impl VfsMountTable {
    const EMPTY_ENTRY: MountEntry = MountEntry {
        prefix: [0u8; MAX_PREFIX_LEN],
        prefix_len: 0,
        label: MountLabel::EMPTY,
        send_cap: 0,
        flags: 0,
    };

    pub const fn new() -> Self {
        Self {
            entries: [Self::EMPTY_ENTRY; MAX_MOUNT_ENTRIES],
            count: 0,
        }
    }

    /// Register a trusted (pre-validated) boot mount.
    ///
    /// Stores `prefix` and `name` as-is — no normalization is applied, making
    /// this suitable for static boot mounts like `b"/initramfs/"` and `b"/dev/"`.
    /// Returns `false` if the table is full, the send cap is zero, the prefix
    /// is too long, or the prefix is already registered.
    pub fn register(&mut self, prefix: &[u8], name: &str, send_cap: u32) -> bool {
        if send_cap == 0 || self.count >= MAX_MOUNT_ENTRIES || prefix.len() > MAX_PREFIX_LEN {
            return false;
        }
        // Reject duplicate prefixes.
        for i in 0..self.count {
            let e = &self.entries[i];
            if e.prefix_len == prefix.len() && e.prefix[..e.prefix_len] == prefix[..] {
                return false;
            }
        }
        let mut entry = Self::EMPTY_ENTRY;
        entry.prefix[..prefix.len()].copy_from_slice(prefix);
        entry.prefix_len = prefix.len();
        entry.label = MountLabel::from_str(name);
        entry.send_cap = send_cap;
        self.entries[self.count] = entry;
        self.count += 1;
        true
    }

    /// Register a dynamic mount received via `VFS_OP_MOUNT_REGISTER`.
    ///
    /// The `raw_prefix` is normalized with [`super::path::normalize`] before
    /// storage.  A trailing `/` is appended to the normalized result (unless
    /// the result is the root `/`).  Duplicate prefixes are rejected after
    /// normalization.
    pub fn insert_dynamic(
        &mut self,
        raw_prefix: &[u8],
        send_cap: u32,
        flags: u32,
    ) -> Result<(), MountRegisterError> {
        if send_cap == 0 {
            return Err(MountRegisterError::InvalidSendCap);
        }

        let norm =
            super::path::normalize(raw_prefix).map_err(|_| MountRegisterError::InvalidPrefix)?;
        let base = norm.as_bytes();

        // Build the stored prefix: normalized base + trailing '/' (except root).
        let need_slash = base != b"/" && !base.ends_with(b"/");
        let stored_len = base.len() + need_slash as usize;
        if stored_len > MAX_PREFIX_LEN {
            return Err(MountRegisterError::PrefixTooLong);
        }

        let mut prefix_buf = [0u8; MAX_PREFIX_LEN];
        prefix_buf[..base.len()].copy_from_slice(base);
        if need_slash {
            prefix_buf[base.len()] = b'/';
        }

        // Reject duplicate prefixes.
        for i in 0..self.count {
            let e = &self.entries[i];
            if e.prefix_len == stored_len && e.prefix[..e.prefix_len] == prefix_buf[..stored_len] {
                return Err(MountRegisterError::DuplicatePrefix);
            }
        }

        if self.count >= MAX_MOUNT_ENTRIES {
            return Err(MountRegisterError::TableFull);
        }

        let label_src = &prefix_buf[..stored_len];
        let mut entry = Self::EMPTY_ENTRY;
        entry.prefix[..stored_len].copy_from_slice(label_src);
        entry.prefix_len = stored_len;
        entry.label = MountLabel::from_bytes(label_src);
        entry.send_cap = send_cap;
        entry.flags = flags;
        self.entries[self.count] = entry;
        self.count += 1;
        Ok(())
    }

    /// Longest-prefix match against all registered mount entries.
    ///
    /// Returns a copy of `(send_cap, label)` for the entry whose prefix is the
    /// longest matching prefix of `path`, or `None` if no entry matches.
    pub(crate) fn route(&self, path: &[u8]) -> Option<(u32, MountLabel)> {
        let mut best_len = 0usize;
        let mut best: Option<(u32, MountLabel)> = None;
        for entry in &self.entries[..self.count] {
            if entry.prefix_len == 0 {
                continue;
            }
            if Self::matches_entry(path, entry) && entry.prefix_len > best_len {
                best_len = entry.prefix_len;
                best = Some((entry.send_cap, entry.label));
            }
        }
        best
    }

    fn matches_entry(path: &[u8], entry: &MountEntry) -> bool {
        let prefix = &entry.prefix[..entry.prefix_len];
        // Root mount "/" covers all absolute paths.
        if prefix == b"/" {
            return path.starts_with(b"/");
        }
        // Stored non-root prefixes are normalized with trailing slash.
        // They must match:
        // - the exact mount root without trailing slash (e.g. "/dev"), or
        // - any child path under that root (e.g. "/dev/null").
        if let Some(base) = prefix.strip_suffix(b"/") {
            path == base || path.starts_with(prefix)
        } else {
            path == prefix || path.starts_with(prefix)
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

    // ── register() ───────────────────────────────────────────────────────────

    #[test]
    fn mount_table_routes_initramfs_and_dev_prefixes() {
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/initramfs/", "initramfs", 10));
        assert!(table.register(b"/dev/", "devfs", 20));
        assert_eq!(table.len(), 2);
        assert!(!table.is_empty());

        let (cap, label) = table.route(b"/initramfs/boot-marker").unwrap();
        assert_eq!(cap, 10);
        assert_eq!(label.as_str(), "initramfs");

        let (cap, label) = table.route(b"/dev/null").unwrap();
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");

        // Exact mount root (without trailing slash) must route too.
        let (cap, label) = table.route(b"/dev").unwrap();
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");
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

        let (cap, label) = table.route(b"/dev/pts/0").unwrap();
        assert_eq!(cap, 2);
        assert_eq!(label.as_str(), "devpts");

        let (cap, label) = table.route(b"/dev/null").unwrap();
        assert_eq!(cap, 1);
        assert_eq!(label.as_str(), "devfs");

        // Mount root exact-match remains stable under longest-prefix logic.
        let (cap, label) = table.route(b"/dev").unwrap();
        assert_eq!(cap, 1);
        assert_eq!(label.as_str(), "devfs");
    }

    #[test]
    fn mount_table_rejects_zero_send_cap() {
        let mut table = VfsMountTable::new();
        assert!(!table.register(b"/dev/", "devfs", 0));
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn dynamic_mount_routes_exact_root_and_children() {
        let mut table = VfsMountTable::new();
        table
            .insert_dynamic(b"/sys//", 77, 0)
            .expect("insert dynamic");
        let (cap_root, label_root) = table.route(b"/sys").expect("root route");
        assert_eq!(cap_root, 77);
        assert_eq!(label_root.as_str(), "/sys/");

        let (cap_child, label_child) = table.route(b"/sys/kernel").expect("child route");
        assert_eq!(cap_child, 77);
        assert_eq!(label_child.as_str(), "/sys/");
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

        assert!(table.route(b"/dev/console").is_some());
        assert!(table.route(b"/dev/").is_some());
        // Exact mount root is intentionally accepted for a trailing-slash mount.
        assert!(table.route(b"/dev").is_some());
        // Prefix boundary still matters: "/device" must not match "/dev/".
        assert!(table.route(b"/device").is_none());
    }

    #[test]
    fn mount_table_rejects_duplicate_prefix_in_register() {
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/dev/", "devfs", 10));
        // Same prefix again — rejected even with different name/cap.
        assert!(!table.register(b"/dev/", "devfs2", 20));
        assert_eq!(table.len(), 1);
    }

    // ── insert_dynamic() ─────────────────────────────────────────────────────

    #[test]
    fn insert_dynamic_normalizes_and_adds_trailing_slash() {
        let mut table = VfsMountTable::new();
        // Input has no trailing slash; normalized result gains one.
        table.insert_dynamic(b"/proc", 5, 0).expect("insert");
        assert_eq!(table.len(), 1);
        // Route should find it with trailing slash in the path.
        let (cap, _) = table.route(b"/proc/1").unwrap();
        assert_eq!(cap, 5);
    }

    #[test]
    fn insert_dynamic_normalizes_dot_and_double_slash() {
        let mut table = VfsMountTable::new();
        table.insert_dynamic(b"//sys//./fs/", 7, 0).expect("insert");
        // After normalization: /sys/fs/ — routes correctly.
        let (cap, _) = table.route(b"/sys/fs/cgroup").unwrap();
        assert_eq!(cap, 7);
    }

    #[test]
    fn insert_dynamic_normalizes_dotdot() {
        let mut table = VfsMountTable::new();
        // /a/b/../c → /a/c → stored as /a/c/
        table.insert_dynamic(b"/a/b/../c", 9, 0).expect("insert");
        let (cap, _) = table.route(b"/a/c/file").unwrap();
        assert_eq!(cap, 9);
        assert!(table.route(b"/a/b/file").is_none());
    }

    #[test]
    fn insert_dynamic_rejects_duplicate_normalized_prefix() {
        let mut table = VfsMountTable::new();
        table.insert_dynamic(b"/dev", 10, 0).expect("first insert");
        // Normalize again: /dev → /dev/ — same stored prefix.
        let err = table.insert_dynamic(b"/dev/", 20, 0).unwrap_err();
        assert_eq!(err, MountRegisterError::DuplicatePrefix);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn insert_dynamic_rejects_zero_send_cap() {
        let mut table = VfsMountTable::new();
        let err = table.insert_dynamic(b"/dev", 0, 0).unwrap_err();
        assert_eq!(err, MountRegisterError::InvalidSendCap);
    }

    #[test]
    fn insert_dynamic_rejects_non_absolute_prefix() {
        let mut table = VfsMountTable::new();
        let err = table.insert_dynamic(b"relative/path", 1, 0).unwrap_err();
        assert_eq!(err, MountRegisterError::InvalidPrefix);
    }

    #[test]
    fn insert_dynamic_rejects_empty_prefix() {
        let mut table = VfsMountTable::new();
        let err = table.insert_dynamic(b"", 1, 0).unwrap_err();
        assert_eq!(err, MountRegisterError::InvalidPrefix);
    }

    #[test]
    fn insert_dynamic_rejects_when_full() {
        let mut table = VfsMountTable::new();
        for i in 0..MAX_MOUNT_ENTRIES {
            let mut prefix = [0u8; 4];
            prefix[0] = b'/';
            prefix[1] = b'a' + i as u8;
            prefix[2] = b'0' + i as u8;
            prefix[3] = b'/';
            table
                .insert_dynamic(&prefix, (i + 1) as u32, 0)
                .expect("fill");
        }
        assert_eq!(table.len(), MAX_MOUNT_ENTRIES);
        let err = table.insert_dynamic(b"/z", 99, 0).unwrap_err();
        assert_eq!(err, MountRegisterError::TableFull);
    }

    #[test]
    fn insert_dynamic_and_register_deduplicate_after_normalization() {
        let mut table = VfsMountTable::new();
        // Static boot mount stored as-is (with trailing slash).
        assert!(table.register(b"/initramfs/", "initramfs", 10));
        // Dynamic attempt with same effective prefix — rejected.
        let err = table.insert_dynamic(b"/initramfs//./", 20, 0).unwrap_err();
        assert_eq!(err, MountRegisterError::DuplicatePrefix);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn insert_dynamic_longer_prefix_wins_over_static_shorter() {
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/dev/", "devfs", 1));
        table.insert_dynamic(b"/dev/pts", 2, 0).expect("insert");

        // /dev/pts/0 → longer prefix wins.
        let (cap, _) = table.route(b"/dev/pts/0").unwrap();
        assert_eq!(cap, 2);

        // /dev/null → shorter boot prefix.
        let (cap, _) = table.route(b"/dev/null").unwrap();
        assert_eq!(cap, 1);
    }

    #[test]
    fn mount_table_root_dynamic_mount_matches_all_paths() {
        let mut table = VfsMountTable::new();
        // "/" is kept as-is (no trailing slash added to root).
        table.insert_dynamic(b"/", 42, 0).expect("insert root");
        let (cap, _) = table.route(b"/anything/here").unwrap();
        assert_eq!(cap, 42);
    }

    #[test]
    fn mount_table_flags_are_stored_and_not_visible_via_route() {
        // flags are internal; route() does not return them (reserved for future use).
        let mut table = VfsMountTable::new();
        table
            .insert_dynamic(b"/mnt", 3, 0xDEAD_BEEF)
            .expect("insert");
        let (cap, _) = table.route(b"/mnt/file").unwrap();
        assert_eq!(cap, 3);
    }

    // ── Stage 87: VFS routing safety ─────────────────────────────────────────

    fn make_boot_table() -> VfsMountTable {
        // Representative table matching the YARM boot-time mount configuration:
        //   /initramfs/ → initramfs_srv (cap=10, static boot mount)
        //   /dev/       → devfs_srv     (cap=20, static boot mount)
        //   /ram/       → ramfs_srv     (cap=30, dynamic — registered after RAMFS spawn)
        // Not present in static table: /fat (INIT_SPAWN_FAT_SRV=false, needs virtio_blk)
        // /ext4 is registered dynamically via VFS_OP_MOUNT_REGISTER after ext4_srv spawn (Stage 88)
        let mut table = VfsMountTable::new();
        assert!(table.register(b"/initramfs/", "initramfs", 10));
        assert!(table.register(b"/dev/", "devfs", 20));
        table
            .insert_dynamic(b"/ram", 30, 0)
            .expect("ramfs dynamic mount");
        table
    }

    #[test]
    fn stage87_routing_dev_routes_to_devfs() {
        let table = make_boot_table();
        let (cap, label) = table.route(b"/dev").expect("/dev must route to devfs");
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");
    }

    #[test]
    fn stage87_routing_dev_null_routes_to_devfs() {
        let table = make_boot_table();
        let (cap, label) = table
            .route(b"/dev/null")
            .expect("/dev/null must route to devfs");
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");
    }

    #[test]
    fn stage87_routing_dev_console_routes_to_devfs() {
        let table = make_boot_table();
        let (cap, label) = table
            .route(b"/dev/console")
            .expect("/dev/console must route to devfs");
        assert_eq!(cap, 20);
        assert_eq!(label.as_str(), "devfs");
    }

    #[test]
    fn stage87_routing_ram_routes_to_ramfs() {
        let table = make_boot_table();
        let (cap, _) = table.route(b"/ram").expect("/ram must route to RAMFS");
        assert_eq!(cap, 30);
        let (cap, _) = table
            .route(b"/ram/file.txt")
            .expect("/ram/file.txt must route to RAMFS");
        assert_eq!(cap, 30);
    }

    #[test]
    fn stage87_routing_fat_absent_when_not_registered() {
        // /fat is not in the default boot table (INIT_SPAWN_FAT_SRV=false).
        let table = make_boot_table();
        assert!(
            table.route(b"/fat").is_none(),
            "/fat must return None when FAT is not registered (INIT_SPAWN_FAT_SRV=false)"
        );
        assert!(
            table.route(b"/fat/hello.txt").is_none(),
            "/fat/hello.txt must return None when FAT is not registered"
        );
    }

    #[test]
    fn stage87_routing_initramfs_paths_accessible() {
        let table = make_boot_table();
        let (cap, label) = table
            .route(b"/initramfs/sbin/init")
            .expect("/initramfs/sbin/init must route to initramfs");
        assert_eq!(cap, 10);
        assert_eq!(label.as_str(), "initramfs");
        let (cap, _) = table
            .route(b"/initramfs/boot-marker")
            .expect("/initramfs/boot-marker must route to initramfs");
        assert_eq!(cap, 10);
    }

    #[test]
    fn stage87_routing_ext4_absent_by_default() {
        // ext4 is registered dynamically (VFS_EXT4_LIVE_MOUNT_ENABLED=true, Stage 88),
        // not in the static boot table.  Before dynamic registration /ext4 is absent.
        let table = make_boot_table();
        assert!(
            table.route(b"/ext4").is_none(),
            "/ext4 must return None before dynamic registration (not in static boot table)"
        );
        assert!(
            table.route(b"/mnt/ext4").is_none(),
            "/mnt/ext4 must return None (ext4 is mounted at /ext4, not /mnt/ext4)"
        );
    }

    #[test]
    fn stage87_routing_devpts_longer_prefix_wins() {
        // /dev/pts/ prefix should beat /dev/ when registered.
        let mut table = make_boot_table();
        table
            .insert_dynamic(b"/dev/pts", 25, 0)
            .expect("devpts mount");

        let (cap, _) = table.route(b"/dev/pts/0").expect("/dev/pts/0 must route");
        assert_eq!(cap, 25, "/dev/pts/0 must match the longer /dev/pts/ prefix");

        let (cap, _) = table.route(b"/dev/null").expect("/dev/null must route");
        assert_eq!(cap, 20, "/dev/null must still match /dev/ prefix");
    }

    #[test]
    fn stage87_routing_fat_enabled_when_dynamically_registered() {
        // Prove that /fat is routable once fat_srv registers it via VFS_OP_MOUNT_REGISTER.
        // This would happen if INIT_SPAWN_FAT_SRV were true and fat_srv reached READY.
        let mut table = make_boot_table();
        table
            .insert_dynamic(b"/fat", 40, 0)
            .expect("fat dynamic mount");

        let (cap, _) = table
            .route(b"/fat")
            .expect("/fat must route once registered");
        assert_eq!(cap, 40);
        let (cap, _) = table
            .route(b"/fat/hello.txt")
            .expect("/fat/hello.txt must route");
        assert_eq!(cap, 40);
        // Other routes remain unaffected.
        let (cap, _) = table.route(b"/dev/null").expect("/dev/null still routes");
        assert_eq!(cap, 20);
    }

    // ── Stage 88: ext4 live read-only VFS route ──────────────────────────────

    fn make_stage88_table() -> VfsMountTable {
        // Stage 88: ext4 is dynamically registered after ext4_srv spawns successfully.
        // Represents the VFS mount table state after INIT_EXT4_SPAWN_OK + VFS_MOUNT_REGISTER_EXT4_OK.
        //   /initramfs/ → initramfs_srv (cap=10, static)
        //   /dev/       → devfs_srv     (cap=20, static)
        //   /ram/       → ramfs_srv     (cap=30, dynamic)
        //   /ext4/      → ext4_srv      (cap=50, dynamic, readonly flags=1)
        let mut table = make_boot_table();
        table
            .insert_dynamic(b"/ext4", 50, 1)
            .expect("ext4 dynamic mount (Stage 88)");
        table
    }

    #[test]
    fn stage88_routing_ext4_dynamically_registered_routes_root() {
        let table = make_stage88_table();
        let (cap, _) = table
            .route(b"/ext4")
            .expect("/ext4 must route after registration");
        assert_eq!(cap, 50);
    }

    #[test]
    fn stage88_routing_ext4_file_paths_route_to_ext4srv() {
        let table = make_stage88_table();
        let (cap, _) = table
            .route(b"/ext4/file.bin")
            .expect("/ext4/file.bin must route to ext4_srv");
        assert_eq!(cap, 50);
        let (cap, _) = table
            .route(b"/ext4/service.bin")
            .expect("/ext4/service.bin must route to ext4_srv");
        assert_eq!(cap, 50);
        let (cap, _) = table
            .route(b"/ext4/oversize.bin")
            .expect("/ext4/oversize.bin must route to ext4_srv");
        assert_eq!(cap, 50);
    }

    #[test]
    fn stage88_routing_ext4_does_not_shadow_other_mounts() {
        let table = make_stage88_table();
        // /dev still routes to devfs (cap=20).
        let (cap, _) = table
            .route(b"/dev/null")
            .expect("/dev/null must still route");
        assert_eq!(cap, 20);
        // /ram still routes to ramfs (cap=30).
        let (cap, _) = table
            .route(b"/ram/file.txt")
            .expect("/ram/file.txt must still route");
        assert_eq!(cap, 30);
        // /initramfs still routes to initramfs_srv (cap=10).
        let (cap, _) = table
            .route(b"/initramfs/boot-marker")
            .expect("/initramfs/boot-marker must still route");
        assert_eq!(cap, 10);
    }

    #[test]
    fn stage88_routing_fat_still_absent_in_stage88_table() {
        // FAT remains disabled (INIT_SPAWN_FAT_SRV=false, needs virtio_blk).
        let table = make_stage88_table();
        assert!(
            table.route(b"/fat").is_none(),
            "/fat must remain unroutable (FAT spawn disabled)"
        );
        assert!(
            table.route(b"/fat/hello.txt").is_none(),
            "/fat/hello.txt must remain unroutable"
        );
    }

    #[test]
    fn stage88_routing_ext4_absent_before_dynamic_registration() {
        // Before ext4_srv spawns and registers, /ext4 is not in the table.
        let table = make_boot_table();
        assert!(
            table.route(b"/ext4").is_none(),
            "/ext4 must be absent from boot table before dynamic registration"
        );
        assert!(
            table.route(b"/ext4/file.bin").is_none(),
            "/ext4/file.bin must be absent before registration"
        );
    }

    // ── Stage 91: VFS routing safety tests ───────────────────────────────────

    #[test]
    fn stage91_routing_initramfs_ext4_srv_path_routes_to_initramfs() {
        // /initramfs/sbin/ext4_srv is an initramfs path, routed to initramfs_srv (cap=10).
        let table = make_boot_table();
        let (cap, _) = table
            .route(b"/initramfs/sbin/ext4_srv")
            .expect("/initramfs/sbin/ext4_srv must route (it's an initramfs path)");
        assert_eq!(
            cap, 10,
            "/initramfs/sbin/ext4_srv must route to initramfs_srv (cap=10)"
        );
    }

    #[test]
    fn stage91_routing_initramfs_ramfs_srv_path_routes_to_initramfs() {
        // /initramfs/sbin/ramfs_srv is an initramfs path, routed to initramfs_srv (cap=10).
        let table = make_boot_table();
        let (cap, _) = table
            .route(b"/initramfs/sbin/ramfs_srv")
            .expect("/initramfs/sbin/ramfs_srv must route (it's an initramfs path)");
        assert_eq!(
            cap, 10,
            "/initramfs/sbin/ramfs_srv must route to initramfs_srv (cap=10)"
        );
    }

    #[test]
    fn stage91_routing_initramfs_driver_manager_routes_to_initramfs() {
        // /initramfs/sbin/driver_manager is an initramfs path, routed to initramfs_srv (cap=10).
        let table = make_boot_table();
        let (cap, _) = table
            .route(b"/initramfs/sbin/driver_manager")
            .expect("/initramfs/sbin/driver_manager must route");
        assert_eq!(
            cap, 10,
            "/initramfs/sbin/driver_manager must route to initramfs_srv (cap=10)"
        );
    }

    #[test]
    fn stage91_routing_optional_mounts_do_not_shadow_core_mounts() {
        // After adding ext4 and fat (hypothetically) to the table,
        // the core mounts (/initramfs, /dev, /ram) must remain unaffected.
        let mut table = make_stage88_table();
        // Hypothetically also register fat (as would happen if FAT were enabled).
        table
            .insert_dynamic(b"/fat", 40, 0)
            .expect("fat registration");

        // Core mounts remain intact.
        let (cap, _) = table
            .route(b"/initramfs/sbin/ext4_srv")
            .expect("initramfs still routes");
        assert_eq!(cap, 10, "/initramfs must still route to initramfs_srv");

        let (cap, _) = table
            .route(b"/dev/null")
            .expect("/dev/null must still route");
        assert_eq!(cap, 20, "/dev must still route to devfs");

        let (cap, _) = table
            .route(b"/ram/file.txt")
            .expect("/ram/file.txt must still route");
        assert_eq!(cap, 30, "/ram must still route to ramfs_srv");
    }

    #[test]
    fn stage91_routing_ext4_present_after_dynamic_registration_stage88_table() {
        // /ext4 is routable in the stage88 table (after ext4_srv registered).
        let table = make_stage88_table();
        let (cap, _) = table
            .route(b"/ext4/service.bin")
            .expect("/ext4/service.bin must route");
        assert_eq!(cap, 50, "/ext4 paths must route to ext4_srv (cap=50)");
    }

    #[test]
    fn stage91_routing_fat_only_routes_when_registered() {
        // /fat must not route before registration (FAT is disabled by default).
        let table = make_boot_table();
        assert!(
            table.route(b"/fat").is_none(),
            "/fat must not route before registration (INIT_SPAWN_FAT_SRV=false)"
        );
        assert!(
            table.route(b"/fat/hello.txt").is_none(),
            "/fat/hello.txt must not route before registration"
        );

        // After hypothetical registration (as would happen if FAT were enabled):
        let mut table_with_fat = make_boot_table();
        table_with_fat
            .insert_dynamic(b"/fat", 40, 0)
            .expect("fat registration");
        let (cap, _) = table_with_fat
            .route(b"/fat/hello.txt")
            .expect("/fat/hello.txt must route after registration");
        assert_eq!(cap, 40);
    }

    #[test]
    fn stage91_routing_ram_unaffected_by_ext4_addition() {
        // Adding /ext4 to the table must not affect /ram routing.
        let table_before = make_boot_table();
        let table_after = make_stage88_table();

        let (cap_before, _) = table_before
            .route(b"/ram/test.txt")
            .expect("/ram must route");
        let (cap_after, _) = table_after
            .route(b"/ram/test.txt")
            .expect("/ram must still route after ext4 added");
        assert_eq!(
            cap_before, cap_after,
            "/ram routing cap must be identical before and after ext4 registration"
        );
        assert_eq!(cap_after, 30, "/ram must route to ramfs_srv (cap=30)");
    }
}
