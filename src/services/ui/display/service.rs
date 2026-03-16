extern crate std;

use std::println;

pub const BOOT_TO_SHELL_MARKER: &str = "[ui] boot-to-shell marker";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayStats {
    pub mode_sets: u64,
    pub frame_presents: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayService {
    stats: DisplayStats,
}

impl DisplayService {
    pub const fn new() -> Self {
        Self {
            stats: DisplayStats {
                mode_sets: 0,
                frame_presents: 0,
            },
        }
    }

    pub fn mode_set(&mut self) {
        self.stats.mode_sets = self.stats.mode_sets.saturating_add(1);
    }

    pub fn present(&mut self) {
        self.stats.frame_presents = self.stats.frame_presents.saturating_add(1);
    }

    pub const fn stats(&self) -> DisplayStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = DisplayService::new();
    svc.mode_set();
    svc.present();
    let s = svc.stats();
    println!("{}", BOOT_TO_SHELL_MARKER);
    println!(
        "display.srv online: mode_sets={}, frame_presents={}",
        s.mode_sets, s.frame_presents
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_tracks_modeset_and_present() {
        let mut svc = DisplayService::new();
        svc.mode_set();
        svc.present();
        assert_eq!(
            svc.stats(),
            DisplayStats {
                mode_sets: 1,
                frame_presents: 1,
            }
        );
    }

    #[test]
    fn boot_marker_is_stable() {
        assert_eq!(BOOT_TO_SHELL_MARKER, "[ui] boot-to-shell marker");
    }
}
