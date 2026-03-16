extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioGpuStats {
    pub frame_commits: u64,
}

pub fn run() {
    let s = VirtioGpuStats { frame_commits: 0 };
    println!(
        "virtio_gpu.srv scaffold online: frame_commits={}",
        s.frame_commits
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_gpu_stats_baseline() {
        let s = VirtioGpuStats { frame_commits: 0 };
        assert_eq!(s.frame_commits, 0);
    }
}
