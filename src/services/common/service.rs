// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::ipc::Message;
use crate::kernel::vfs::{VfsBackend, VfsError};
use crate::services::common::vfs_service::VfsService;

pub trait RequestResponseService {
    type Error;

    fn service_name(&self) -> &'static str;
    fn handle(&mut self, request: Message) -> Result<Message, Self::Error>;
}

pub fn run_typed_request_loop<S: RequestResponseService, const N: usize>(
    service: &mut S,
    requests: [Message; N],
) -> Result<[Message; N], S::Error> {
    let mut replies = [const { None }; N];
    let mut idx = 0;
    while idx < N {
        replies[idx] = Some(service.handle(requests[idx])?);
        idx += 1;
    }
    Ok(replies.map(|reply| reply.expect("all replies are populated")))
}

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

impl<B: VfsBackend> RequestResponseService for FsService<B> {
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
    use crate::kernel::vfs::{InMemoryBackend, OpenAtRequest, openat_message};

    #[test]
    fn typed_request_loop_runs_all_requests() {
        let mut svc = FsService::with_backend(InMemoryBackend::new());
        let replies = run_typed_request_loop(
            &mut svc,
            [
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: 0x1000,
                    flags: 0,
                    mode: 0,
                })
                .expect("open"),
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: 0x2000,
                    flags: 0,
                    mode: 0,
                })
                .expect("open"),
            ],
        )
        .expect("loop");
        assert_eq!(replies.len(), 2);
        assert_eq!(svc.handled_count(), 2);
    }
}
