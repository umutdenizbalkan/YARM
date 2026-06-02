// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountServiceKind {
    Initramfs,
    RamFs,
    DevFs,
    Ext4,
    Fat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountPlan {
    pub order: [Option<MountServiceKind>; 5],
    pub count: usize,
    pub allow_fallback_to_fat: bool,
}

impl MountPlan {
    pub const fn baseline() -> Self {
        Self {
            order: [
                Some(MountServiceKind::Initramfs),
                Some(MountServiceKind::RamFs),
                Some(MountServiceKind::DevFs),
                Some(MountServiceKind::Ext4),
                None,
            ],
            count: 4,
            allow_fallback_to_fat: true,
        }
    }

    pub const fn with_fat_for_block_backend(block_backend_available: bool) -> Self {
        if block_backend_available {
            Self {
                order: [
                    Some(MountServiceKind::Initramfs),
                    Some(MountServiceKind::RamFs),
                    Some(MountServiceKind::DevFs),
                    Some(MountServiceKind::Ext4),
                    Some(MountServiceKind::Fat),
                ],
                count: 5,
                allow_fallback_to_fat: true,
            }
        } else {
            Self::baseline()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_mount_policy_includes_fat_only_when_block_backend_available() {
        let without = MountPlan::with_fat_for_block_backend(false);
        assert_eq!(without.count, 4);
        assert!(!without.order.contains(&Some(MountServiceKind::Fat)));

        let with = MountPlan::with_fat_for_block_backend(true);
        assert_eq!(with.count, 5);
        assert_eq!(with.order[4], Some(MountServiceKind::Fat));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountRecoveryReport {
    pub mounted_count: usize,
    pub recovered_with_fat: bool,
}
