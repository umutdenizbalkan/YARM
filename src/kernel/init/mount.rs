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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountRecoveryReport {
    pub mounted_count: usize,
    pub recovered_with_fat: bool,
}
