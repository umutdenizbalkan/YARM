extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs::{FilesystemService, VfsLiteError};

const MAX_CACHE_LINES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionPolicy {
    OverwriteOldest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCacheConfig {
    pub max_lines: usize,
    pub writeback_batch: usize,
    pub eviction: EvictionPolicy,
}

impl BlockCacheConfig {
    pub const fn baseline() -> Self {
        Self {
            max_lines: 8,
            writeback_batch: 2,
            eviction: EvictionPolicy::OverwriteOldest,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLine {
    pub block: u64,
    pub value: u64,
    pub dirty: bool,
    pub age: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCache {
    config: BlockCacheConfig,
    lines: [Option<CacheLine>; MAX_CACHE_LINES],
    puts: u64,
    gets: u64,
    evictions: u64,
    clock: u64,
    writeback_cursor: usize,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockCache {
    pub const fn new() -> Self {
        Self::with_config(BlockCacheConfig::baseline())
    }

    pub const fn with_config(config: BlockCacheConfig) -> Self {
        Self {
            config,
            lines: [None; MAX_CACHE_LINES],
            puts: 0,
            gets: 0,
            evictions: 0,
            clock: 0,
            writeback_cursor: 0,
        }
    }

    pub const fn config(&self) -> BlockCacheConfig {
        self.config
    }

    pub const fn stats(&self) -> (u64, u64, u64) {
        (self.puts, self.gets, self.evictions)
    }

    pub fn put(&mut self, block: u64, value: u64) {
        self.puts = self.puts.saturating_add(1);
        self.clock = self.clock.saturating_add(1);
        if let Some(slot) = self
            .lines
            .iter_mut()
            .take(self.config.max_lines.min(MAX_CACHE_LINES))
            .find(|slot| slot.map(|line| line.block == block).unwrap_or(false))
        {
            *slot = Some(CacheLine {
                block,
                value,
                dirty: true,
                age: self.clock,
            });
            return;
        }
        if let Some(slot) = self
            .lines
            .iter_mut()
            .take(self.config.max_lines.min(MAX_CACHE_LINES))
            .find(|slot| slot.is_none())
        {
            *slot = Some(CacheLine {
                block,
                value,
                dirty: true,
                age: self.clock,
            });
            return;
        }

        self.evictions = self.evictions.saturating_add(1);
        let idx = match self.config.eviction {
            EvictionPolicy::OverwriteOldest => self
                .lines
                .iter()
                .take(self.config.max_lines.min(MAX_CACHE_LINES))
                .enumerate()
                .filter_map(|(i, slot)| slot.map(|line| (i, line.age)))
                .min_by_key(|(_, age)| *age)
                .map(|(i, _)| i)
                .unwrap_or(0),
        };
        self.lines[idx] = Some(CacheLine {
            block,
            value,
            dirty: true,
            age: self.clock,
        });
    }

    pub fn get(&mut self, block: u64) -> Option<u64> {
        self.gets = self.gets.saturating_add(1);
        self.lines
            .iter()
            .take(self.config.max_lines.min(MAX_CACHE_LINES))
            .flatten()
            .find(|line| line.block == block)
            .map(|line| line.value)
    }

    pub fn writeback_tick(&mut self) -> u64 {
        let mut flushed: u64 = 0;
        let limit = self.config.max_lines.min(MAX_CACHE_LINES);
        if limit == 0 {
            return 0;
        }
        for _ in 0..self.config.writeback_batch {
            let idx = self.writeback_cursor % limit;
            self.writeback_cursor = self.writeback_cursor.saturating_add(1);
            if let Some(mut line) = self.lines[idx] {
                if line.dirty {
                    line.dirty = false;
                    self.lines[idx] = Some(line);
                    flushed = flushed.saturating_add(1);
                }
            }
        }
        flushed
    }

    pub fn flush_dirty_count(&mut self) -> u64 {
        let mut flushed: u64 = 0;
        for slot in &mut self.lines {
            if let Some(mut line) = *slot {
                if line.dirty {
                    line.dirty = false;
                    *slot = Some(line);
                    flushed = flushed.saturating_add(1);
                }
            }
        }
        flushed
    }
}

#[derive(Debug)]
pub struct BlkCacheService {
    cache: BlockCache,
}

impl Default for BlkCacheService {
    fn default() -> Self {
        Self::new()
    }
}

impl BlkCacheService {
    pub const fn new() -> Self {
        Self {
            cache: BlockCache::new(),
        }
    }

    pub const fn with_config(config: BlockCacheConfig) -> Self {
        Self {
            cache: BlockCache::with_config(config),
        }
    }

    pub const fn stats(&self) -> (u64, u64, u64) {
        self.cache.stats()
    }

    pub fn cache_mut(&mut self) -> &mut BlockCache {
        &mut self.cache
    }
}

impl FilesystemService for BlkCacheService {
    fn service_name(&self) -> &'static str {
        "blkcache"
    }

    fn dispatch(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        if request.opcode == 1 {
            self.cache.put(1, 1);
        } else if request.opcode == 2 {
            let _ = self.cache.get(1);
        } else if request.opcode == 3 {
            let _ = self.cache.writeback_tick();
        }
        Message::with_header(0, request.opcode, 0, None, &[0]).map_err(|_| VfsLiteError::Malformed)
    }
}

pub fn run() {
    let mut svc = BlkCacheService::new();
    let put = Message::with_header(0, 1, 0, None, &[0]).expect("put");
    let get = Message::with_header(0, 2, 0, None, &[0]).expect("get");
    let flush = Message::with_header(0, 3, 0, None, &[0]).expect("flush");
    let _ = svc.dispatch(put).expect("put rep");
    let _ = svc.dispatch(get).expect("get rep");
    let _ = svc.dispatch(flush).expect("flush rep");
    let (puts, gets, evictions) = svc.stats();
    println!(
        "blkcache.srv demo: puts={}, gets={}, evictions={}",
        puts, gets, evictions
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_put_get_and_flush() {
        let mut cache = BlockCache::new();
        cache.put(7, 11);
        assert_eq!(cache.get(7), Some(11));
        assert_eq!(cache.flush_dirty_count(), 1);
    }

    #[test]
    fn writeback_tick_respects_batch() {
        let mut cache = BlockCache::with_config(BlockCacheConfig {
            max_lines: 4,
            writeback_batch: 1,
            eviction: EvictionPolicy::OverwriteOldest,
        });
        cache.put(1, 10);
        cache.put(2, 20);
        assert_eq!(cache.writeback_tick(), 1);
    }
}
