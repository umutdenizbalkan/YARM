extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioGpuStats {
    pub frame_commits: u64,
    pub mode_sets: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioGpuService {
    stats: VirtioGpuStats,
}

impl VirtioGpuService {
    pub const fn new() -> Self {
        Self {
            stats: VirtioGpuStats {
                frame_commits: 0,
                mode_sets: 0,
            },
        }
    }

    pub fn mode_set(&mut self) {
        self.stats.mode_sets = self.stats.mode_sets.saturating_add(1);
    }

    pub fn commit_frame(&mut self) {
        self.stats.frame_commits = self.stats.frame_commits.saturating_add(1);
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
    println!(
        "virtio_gpu.srv online: frame_commits={}, mode_sets={}",
        stats.frame_commits, stats.mode_sets
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_gpu_tracks_modeset_and_commit() {
        let mut s = VirtioGpuService::new();
        s.mode_set();
        s.commit_frame();
        assert_eq!(
            s.stats(),
            VirtioGpuStats {
                frame_commits: 1,
                mode_sets: 1,
            }
        );
    }
}
