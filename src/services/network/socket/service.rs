#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAdapterStats {
    pub opens: u64,
    pub closes: u64,
    pub reads: u64,
    pub writes: u64,
}

const SOCKET_FD_BASE: i32 = 1000;
const MAX_SOCKETS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAdapterService {
    stats: SocketAdapterStats,
    next_fd: i32,
    active: [Option<i32>; MAX_SOCKETS],
}

impl SocketAdapterService {
    pub const fn new() -> Self {
        Self {
            stats: SocketAdapterStats {
                opens: 0,
                closes: 0,
                reads: 0,
                writes: 0,
            },
            next_fd: SOCKET_FD_BASE,
            active: [None; MAX_SOCKETS],
        }
    }

    pub fn open(&mut self, domain: i32, sock_type: i32, _protocol: i32) -> Result<i32, ()> {
        if domain <= 0 || sock_type <= 0 {
            return Err(());
        }
        let fd = self.next_fd;
        self.next_fd = self.next_fd.saturating_add(1);
        let Some(slot) = self.active.iter_mut().find(|slot| slot.is_none()) else {
            return Err(());
        };
        *slot = Some(fd);
        self.stats.opens = self.stats.opens.saturating_add(1);
        Ok(fd)
    }

    pub fn is_socket_fd(&self, fd: i32) -> bool {
        self.active.iter().flatten().any(|active| *active == fd)
    }

    pub fn read(&mut self, fd: i32, len: usize) -> Result<usize, ()> {
        if !self.is_socket_fd(fd) {
            return Err(());
        }
        self.stats.reads = self.stats.reads.saturating_add(1);
        Ok(len.min(64))
    }

    pub fn write(&mut self, fd: i32, len: usize) -> Result<usize, ()> {
        if !self.is_socket_fd(fd) {
            return Err(());
        }
        self.stats.writes = self.stats.writes.saturating_add(1);
        Ok(len)
    }

    pub fn close(&mut self, fd: i32) -> Result<(), ()> {
        let Some(slot) = self
            .active
            .iter_mut()
            .find(|slot| slot.map(|active| active == fd).unwrap_or(false))
        else {
            return Err(());
        };
        *slot = None;
        self.stats.closes = self.stats.closes.saturating_add(1);
        Ok(())
    }

    pub const fn stats(&self) -> SocketAdapterStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = SocketAdapterService::new();
    let fd = svc.open(2, 1, 0).expect("socket open");
    let s = svc.stats();
    crate::yarm_log!(
        "socket.srv online: fd={}, opens={}, closes={}",
        fd,
        s.opens,
        s.closes
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_adapter_roundtrip_is_accounted() {
        let mut svc = SocketAdapterService::new();
        let fd = svc.open(2, 1, 0).expect("open");
        assert_eq!(svc.read(fd, 128).expect("read"), 64);
        assert_eq!(svc.write(fd, 32).expect("write"), 32);
        svc.close(fd).expect("close");
        assert_eq!(
            svc.stats(),
            SocketAdapterStats {
                opens: 1,
                closes: 1,
                reads: 1,
                writes: 1,
            }
        );
    }
}
