// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellStats {
    pub sessions_started: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellService {
    stats: ShellStats,
}

impl ShellService {
    pub const fn new() -> Self {
        Self {
            stats: ShellStats {
                sessions_started: 0,
            },
        }
    }

    pub fn start_session(&mut self) {
        self.stats.sessions_started = self.stats.sessions_started.saturating_add(1);
    }

    pub const fn stats(&self) -> ShellStats {
        self.stats
    }
}

pub fn run() {
    let mut s = ShellService::new();
    s.start_session();
    crate::yarm_log!(
        "shell.srv online: sessions_started={}",
        s.stats().sessions_started
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_session_counter_increments() {
        let mut s = ShellService::new();
        s.start_session();
        assert_eq!(s.stats().sessions_started, 1);
    }
}
