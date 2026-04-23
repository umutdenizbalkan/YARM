// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositorStats {
    pub composed_frames: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositorService {
    stats: CompositorStats,
}

impl CompositorService {
    pub const fn new() -> Self {
        Self {
            stats: CompositorStats { composed_frames: 0 },
        }
    }

    pub fn compose(&mut self) {
        self.stats.composed_frames = self.stats.composed_frames.saturating_add(1);
    }

    pub const fn stats(&self) -> CompositorStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = CompositorService::new();
    svc.compose();
    yarm_user_rt::user_log!(
        "compositor.srv online: composed_frames={}",
        svc.stats().composed_frames
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositor_replay_is_deterministic() {
        let mut svc = CompositorService::new();
        for _ in 0..4 {
            svc.compose();
        }
        assert_eq!(svc.stats().composed_frames, 4);
    }
}
