extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::vfs::{FilesystemService, VfsLiteError};

#[derive(Debug, Default)]
pub struct BlkCacheService {
    puts: u64,
    gets: u64,
}

impl BlkCacheService {
    pub const fn new() -> Self {
        Self { puts: 0, gets: 0 }
    }

    pub const fn stats(&self) -> (u64, u64) {
        (self.puts, self.gets)
    }
}

impl FilesystemService for BlkCacheService {
    fn service_name(&self) -> &'static str {
        "blkcache"
    }

    fn dispatch(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        // tiny opcode convention for scaffold
        if request.opcode == 1 {
            self.puts = self.puts.saturating_add(1);
        } else {
            self.gets = self.gets.saturating_add(1);
        }
        Message::with_header(0, request.opcode, 0, None, &[0]).map_err(|_| VfsLiteError::Malformed)
    }
}

pub fn run() {
    let mut svc = BlkCacheService::new();
    let put = Message::with_header(0, 1, 0, None, &[0]).expect("put");
    let get = Message::with_header(0, 2, 0, None, &[0]).expect("get");
    let _ = svc.dispatch(put).expect("put rep");
    let _ = svc.dispatch(get).expect("get rep");
    let (puts, gets) = svc.stats();
    println!("blkcache.srv demo: puts={}, gets={}", puts, gets);
}
