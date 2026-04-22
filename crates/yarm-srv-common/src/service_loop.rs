// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub trait RequestResponseService<Request, Response> {
    type Error;

    fn service_name(&self) -> &'static str;
    fn handle(&mut self, request: Request) -> Result<Response, Self::Error>;
}

pub fn run_typed_request_loop<S, Request, Response, const N: usize>(
    service: &mut S,
    requests: [Request; N],
) -> Result<[Response; N], S::Error>
where
    S: RequestResponseService<Request, Response>,
    Request: Copy,
{
    let mut replies = [const { None }; N];
    let mut idx = 0;
    while idx < N {
        replies[idx] = Some(service.handle(requests[idx])?);
        idx += 1;
    }
    Ok(replies.map(|reply| reply.expect("all replies are populated")))
}
