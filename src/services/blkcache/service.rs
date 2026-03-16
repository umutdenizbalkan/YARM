extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs::{FilesystemService, VfsLiteError};

const MAX_CACHE_LINES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLine {
    pub block: u64,
    pub value: u64,
    pub dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCache {
    lines: [Option<CacheLine>; MAX_CACHE_LINES],
    puts: u64,
    gets: u64,
    evictions: u64,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockCache {
    pub const fn new() -> Self {
        Self {
            lines: [None; MAX_CACHE_LINES],
            puts: 0,
            gets: 0,
            evictions: 0,
        }
    }

    pub const fn stats(&self) -> (u64, u64, u64) {
        (self.puts, self.gets, self.evictions)
    }

    pub fn put(&mut self, block: u64, value: u64) {
        self.puts = self.puts.saturating_add(1);
        if let Some(slot) = self
            .lines
            .iter_mut()
            .find(|slot| slot.map(|line| line.block == block).unwrap_or(false))
        {
            *slot = Some(CacheLine {
                block,
                value,
                dirty: true,
            });
            return;
        }
        if let Some(slot) = self.lines.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(CacheLine {
                block,
                value,
                dirty: true,
            });
            return;
        }
        self.evictions = self.evictions.saturating_add(1);
        self.lines[0] = Some(CacheLine {
            block,
            value,
            dirty: true,
        });
    }

    pub fn get(&mut self, block: u64) -> Option<u64> {
        self.gets = self.gets.saturating_add(1);
        self.lines
            .iter()
            .flatten()
            .find(|line| line.block == block)
            .map(|line| line.value)
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

#[derive(Debug, Default)]
pub struct BlkCacheService {
    cache: BlockCache,
}

impl BlkCacheService {
    pub const fn new() -> Self {
        Self {
            cache: BlockCache::new(),
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
            let _ = self.cache.flush_dirty_count();
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
}
