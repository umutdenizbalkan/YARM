use crate::kernel::ipc::Message;
use crate::kernel::vfs::{VfsBackend, VfsLiteError, VfsLiteService};

#[derive(Debug)]
pub struct FsService<B: VfsBackend> {
    inner: VfsLiteService<B>,
    handled: usize,
}

impl<B: VfsBackend> FsService<B> {
    pub const fn with_backend(backend: B) -> Self {
        Self {
            inner: VfsLiteService::with_backend(backend),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsLiteError> {
        let reply = self.inner.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }
}
