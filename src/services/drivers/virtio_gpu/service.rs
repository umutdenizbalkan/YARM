#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioGpuStats {
    pub frame_commits: u64,
    pub mode_sets: u64,
    pub rejected_commits: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioGpuService {
    stats: VirtioGpuStats,
    mode_active: bool,
}

impl VirtioGpuService {
    pub const fn new() -> Self {
        Self {
            stats: VirtioGpuStats {
                frame_commits: 0,
                mode_sets: 0,
                rejected_commits: 0,
            },
            mode_active: false,
        }
    }

    pub fn mode_set(&mut self) {
        self.mode_active = true;
        self.stats.mode_sets = self.stats.mode_sets.saturating_add(1);
    }

    pub fn commit_frame(&mut self) {
        if self.mode_active {
            self.stats.frame_commits = self.stats.frame_commits.saturating_add(1);
        } else {
            self.stats.rejected_commits = self.stats.rejected_commits.saturating_add(1);
        }
    }

    pub const fn stats(&self) -> VirtioGpuStats {
        self.stats
    }
}

pub fn run() {
    let mut s = VirtioGpuService::new();
    s.mode_set();
    s.commit_frame();
    let stats = s.stats();
    crate::yarm_log!(
        "virtio_gpu.srv online: frame_commits={}, mode_sets={}, rejected_commits={}",
        stats.frame_commits, stats.mode_sets, stats.rejected_commits
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_gpu_rejects_commit_before_modeset() {
        let mut s = VirtioGpuService::new();
        s.commit_frame();
        s.mode_set();
        s.commit_frame();
        assert_eq!(
            s.stats(),
            VirtioGpuStats {
                frame_commits: 1,
                mode_sets: 1,
                rejected_commits: 1,
            }
        );
    }
}
