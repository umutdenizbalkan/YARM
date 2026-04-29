// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use super::vfs_ipc::{VfsBackend, VfsError};
use super::vfs_service::VfsService;
use yarm_srv_common::service_loop::RequestResponseService;

#[derive(Debug)]
pub struct FsService<B: VfsBackend> {
    inner: VfsService<B>,
    handled: usize,
}

impl<B: VfsBackend> FsService<B> {
    pub const fn with_backend(backend: B) -> Self {
        Self {
            inner: VfsService::with_backend(backend),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub const fn backend(&self) -> &B {
        self.inner.backend()
    }

    pub fn backend_mut(&mut self) -> &mut B {
        self.inner.backend_mut()
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsError> {
        let reply = self.inner.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }
}

impl<B: VfsBackend> RequestResponseService<Message, Message> for FsService<B> {
    type Error = VfsError;

    fn service_name(&self) -> &'static str {
        "fs"
    }

    fn handle(&mut self, request: Message) -> Result<Message, Self::Error> {
        FsService::handle(self, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::vfs_ipc::{InMemoryBackend, openat_inline_message};
    use yarm_srv_common::service_loop::run_typed_request_loop;

    #[test]
    fn typed_request_loop_runs_all_requests() {
        let mut svc = FsService::with_backend(InMemoryBackend::new());
        let replies = run_typed_request_loop(
            &mut svc,
            [
                openat_inline_message(0, b"/dev/console", 0, 0)
                .expect("open"),
                openat_inline_message(0, b"/dev/null", 0, 0)
                .expect("open"),
            ],
        )
        .expect("loop");
        assert_eq!(replies.len(), 2);
        assert_eq!(svc.handled_count(), 2);
    }
}
