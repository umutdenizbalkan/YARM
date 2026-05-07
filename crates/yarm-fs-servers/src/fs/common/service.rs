// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use super::vfs_ipc::{VfsBackend, VfsError};
use super::vfs_service::VfsService;
use yarm_srv_common::service_loop::RequestResponseService;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceCleanup {
    VmAnonMapProducer { base: usize, len: usize, mem_cap: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceResponse {
    pub message: Message,
    pub cleanup: Option<ServiceCleanup>,
}

impl ServiceResponse {
    pub const fn message(message: Message) -> Self {
        Self {
            message,
            cleanup: None,
        }
    }

    pub const fn with_cleanup(message: Message, cleanup: ServiceCleanup) -> Self {
        Self {
            message,
            cleanup: Some(cleanup),
        }
    }

    pub const fn as_message(&self) -> &Message {
        &self.message
    }

    pub const fn into_message(self) -> Message {
        self.message
    }
}

impl core::ops::Deref for ServiceResponse {
    type Target = Message;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl From<ServiceResponse> for Message {
    fn from(value: ServiceResponse) -> Self {
        value.message
    }
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

    pub fn handle_response(&mut self, request: Message) -> Result<ServiceResponse, VfsError> {
        let reply = self.inner.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, VfsError> {
        Ok(self.handle_response(request)?.message)
    }
}

impl<B: VfsBackend> RequestResponseService<Message, ServiceResponse> for FsService<B> {
    type Error = VfsError;

    fn service_name(&self) -> &'static str {
        "fs"
    }

    fn handle(&mut self, request: Message) -> Result<ServiceResponse, Self::Error> {
        FsService::handle_response(self, request)
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
        assert!(replies.iter().all(|reply| reply.cleanup.is_none()));
        assert_eq!(svc.handled_count(), 2);
    }
}
