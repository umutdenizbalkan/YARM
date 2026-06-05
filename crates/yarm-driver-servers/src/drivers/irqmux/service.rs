// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::irqmux_abi::{
    IRQMUX_ROUTE_F_BOUND, IRQMUX_ROUTE_F_ENABLED, IRQMUX_ROUTE_F_MASKED, IRQMUX_ROUTE_F_REGISTERED,
    IrqLine, IrqMuxCodecError, IrqMuxRequest, IrqMuxResponse, IrqMuxStatus, IrqPolarity,
    IrqRouteTarget, IrqTriggerMode, IrqVector,
};

pub const MAX_IRQ_ROUTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqRoute {
    pub line: IrqLine,
    pub vector: IrqVector,
    pub trigger: IrqTriggerMode,
    pub polarity: IrqPolarity,
    pub target: Option<IrqRouteTarget>,
    pub enabled: bool,
    pub masked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqDispatchResult {
    Delivered {
        line: IrqLine,
        target: IrqRouteTarget,
    },
    Masked,
    Disabled,
    Unregistered,
    NoTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxStats {
    pub delivered: u64,
    pub masked: u64,
    pub disabled: u64,
    pub unregistered: u64,
    pub no_target: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxService {
    routes: [Option<IrqRoute>; MAX_IRQ_ROUTES],
    stats: IrqMuxStats,
}

impl Default for IrqMuxService {
    fn default() -> Self {
        Self::new()
    }
}

impl IrqMuxService {
    pub const fn new() -> Self {
        Self {
            routes: [None; MAX_IRQ_ROUTES],
            stats: IrqMuxStats {
                delivered: 0,
                masked: 0,
                disabled: 0,
                unregistered: 0,
                no_target: 0,
            },
        }
    }

    pub fn register_line(
        &mut self,
        line: IrqLine,
        vector: IrqVector,
        trigger: IrqTriggerMode,
        polarity: IrqPolarity,
    ) -> IrqMuxStatus {
        if self.route(line).is_some() {
            return IrqMuxStatus::AlreadyRegistered;
        }
        let Some(slot) = self.routes.iter_mut().find(|slot| slot.is_none()) else {
            return IrqMuxStatus::Busy;
        };
        *slot = Some(IrqRoute {
            line,
            vector,
            trigger,
            polarity,
            target: None,
            enabled: false,
            masked: true,
        });
        IrqMuxStatus::Ok
    }

    pub fn unregister_line(&mut self, line: IrqLine) -> IrqMuxStatus {
        let Some(index) = self.route_index(line) else {
            return IrqMuxStatus::NotFound;
        };
        self.routes[index] = None;
        IrqMuxStatus::Ok
    }

    pub fn bind_driver(&mut self, line: IrqLine, target: IrqRouteTarget) -> IrqMuxStatus {
        if target == 0 {
            return IrqMuxStatus::BadRequest;
        }
        let Some(route) = self.route_mut(line) else {
            return IrqMuxStatus::NotFound;
        };
        if route.target.is_some() {
            return IrqMuxStatus::Busy;
        }
        route.target = Some(target);
        IrqMuxStatus::Ok
    }

    pub fn unbind_driver(&mut self, line: IrqLine) -> IrqMuxStatus {
        let Some(route) = self.route_mut(line) else {
            return IrqMuxStatus::NotFound;
        };
        if route.target.take().is_none() {
            return IrqMuxStatus::NotFound;
        }
        IrqMuxStatus::Ok
    }

    pub fn enable(&mut self, line: IrqLine) -> IrqMuxStatus {
        self.set_enabled(line, true)
    }

    pub fn disable(&mut self, line: IrqLine) -> IrqMuxStatus {
        self.set_enabled(line, false)
    }

    pub fn mask(&mut self, line: IrqLine) -> IrqMuxStatus {
        self.set_masked(line, true)
    }

    pub fn unmask(&mut self, line: IrqLine) -> IrqMuxStatus {
        self.set_masked(line, false)
    }

    pub fn ack(&self, line: IrqLine) -> IrqMuxStatus {
        if self.route(line).is_some() {
            IrqMuxStatus::Ok
        } else {
            IrqMuxStatus::NotFound
        }
    }

    pub fn dispatch_fake_irq(&mut self, line: IrqLine) -> IrqDispatchResult {
        let result = match self.route(line) {
            None => IrqDispatchResult::Unregistered,
            Some(route) if !route.enabled => IrqDispatchResult::Disabled,
            Some(route) if route.masked => IrqDispatchResult::Masked,
            Some(route) => match route.target {
                Some(target) => IrqDispatchResult::Delivered { line, target },
                None => IrqDispatchResult::NoTarget,
            },
        };
        match result {
            IrqDispatchResult::Delivered { .. } => {
                self.stats.delivered = self.stats.delivered.saturating_add(1)
            }
            IrqDispatchResult::Masked => self.stats.masked = self.stats.masked.saturating_add(1),
            IrqDispatchResult::Disabled => {
                self.stats.disabled = self.stats.disabled.saturating_add(1)
            }
            IrqDispatchResult::Unregistered => {
                self.stats.unregistered = self.stats.unregistered.saturating_add(1)
            }
            IrqDispatchResult::NoTarget => {
                self.stats.no_target = self.stats.no_target.saturating_add(1)
            }
        }
        result
    }

    pub fn handle_request(&mut self, request: IrqMuxRequest) -> IrqMuxResponse {
        let line = request.line();
        let status = match request {
            IrqMuxRequest::RegisterLine {
                line,
                vector,
                trigger,
                polarity,
            } => self.register_line(line, vector, trigger, polarity),
            IrqMuxRequest::UnregisterLine { line } => self.unregister_line(line),
            IrqMuxRequest::BindDriver { line, target } => self.bind_driver(line, target),
            IrqMuxRequest::UnbindDriver { line } => self.unbind_driver(line),
            IrqMuxRequest::Enable { line } => self.enable(line),
            IrqMuxRequest::Disable { line } => self.disable(line),
            IrqMuxRequest::Mask { line } => self.mask(line),
            IrqMuxRequest::Unmask { line } => self.unmask(line),
            IrqMuxRequest::Ack { line } => self.ack(line),
            IrqMuxRequest::InjectTestIrq { line } => match self.dispatch_fake_irq(line) {
                IrqDispatchResult::Delivered { .. } => IrqMuxStatus::Ok,
                IrqDispatchResult::Masked => IrqMuxStatus::Masked,
                IrqDispatchResult::Disabled => IrqMuxStatus::Disabled,
                IrqDispatchResult::Unregistered | IrqDispatchResult::NoTarget => {
                    IrqMuxStatus::NotFound
                }
            },
            IrqMuxRequest::GetStatus { line } => {
                if self.route(line).is_some() {
                    IrqMuxStatus::Ok
                } else {
                    IrqMuxStatus::NotFound
                }
            }
        };
        self.response(line, status)
    }

    pub fn handle_wire_request(&mut self, opcode: u16, payload: &[u8]) -> IrqMuxResponse {
        match IrqMuxRequest::decode(opcode, payload) {
            Ok(request) => self.handle_request(request),
            Err(IrqMuxCodecError::Malformed) => self.response(0, IrqMuxStatus::BadRequest),
            Err(IrqMuxCodecError::UnsupportedOpcode) => self.response(0, IrqMuxStatus::Unsupported),
        }
    }

    pub fn route(&self, line: IrqLine) -> Option<IrqRoute> {
        self.route_index(line).and_then(|index| self.routes[index])
    }

    pub fn route_count(&self) -> usize {
        self.routes.iter().filter(|route| route.is_some()).count()
    }

    pub const fn stats(&self) -> IrqMuxStats {
        self.stats
    }

    fn route_index(&self, line: IrqLine) -> Option<usize> {
        self.routes
            .iter()
            .position(|route| route.is_some_and(|route| route.line == line))
    }

    fn route_mut(&mut self, line: IrqLine) -> Option<&mut IrqRoute> {
        self.routes
            .iter_mut()
            .filter_map(Option::as_mut)
            .find(|route| route.line == line)
    }

    fn set_enabled(&mut self, line: IrqLine, enabled: bool) -> IrqMuxStatus {
        let Some(route) = self.route_mut(line) else {
            return IrqMuxStatus::NotFound;
        };
        route.enabled = enabled;
        IrqMuxStatus::Ok
    }

    fn set_masked(&mut self, line: IrqLine, masked: bool) -> IrqMuxStatus {
        let Some(route) = self.route_mut(line) else {
            return IrqMuxStatus::NotFound;
        };
        route.masked = masked;
        IrqMuxStatus::Ok
    }

    fn response(&self, line: IrqLine, status: IrqMuxStatus) -> IrqMuxResponse {
        let Some(route) = self.route(line) else {
            return IrqMuxResponse {
                status,
                route_flags: 0,
                line,
                vector: 0,
                target: 0,
                trigger: None,
                polarity: None,
            };
        };
        let mut route_flags = IRQMUX_ROUTE_F_REGISTERED;
        if route.target.is_some() {
            route_flags |= IRQMUX_ROUTE_F_BOUND;
        }
        if route.enabled {
            route_flags |= IRQMUX_ROUTE_F_ENABLED;
        }
        if route.masked {
            route_flags |= IRQMUX_ROUTE_F_MASKED;
        }
        IrqMuxResponse {
            status,
            route_flags,
            line,
            vector: route.vector,
            target: route.target.unwrap_or(0),
            trigger: Some(route.trigger),
            polarity: Some(route.polarity),
        }
    }
}

pub fn run() {
    yarm_user_rt::user_log!("IRQMUX_SRV_ENTRY");
    let mut service = IrqMuxService::new();
    yarm_user_rt::user_log!("IRQMUX_SRV_READY routes={}", service.route_count());

    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("IRQMUX_SRV_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("IRQMUX_SRV_RECV_LOOP cap={}", recv_cap);
    loop {
        // SAFETY: irqmux owns its startup-provided service receive endpoint.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let response = service
                    .handle_wire_request(received.message.opcode, received.message.as_slice());
                if response.status == IrqMuxStatus::Unsupported {
                    yarm_user_rt::user_log!(
                        "IRQMUX_SRV_UNSUPPORTED_OPCODE opcode={}",
                        received.message.opcode
                    );
                }
                let Some(reply_cap) = received.reply_cap else {
                    continue;
                };
                if let Ok(reply) = yarm_user_rt::ipc::Message::with_header(
                    0,
                    received.message.opcode,
                    0,
                    None,
                    &response.encode(),
                ) {
                    // SAFETY: the reply capability accompanied this received request.
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                }
            }
            Ok(None) => {}
            Err(error) => {
                yarm_user_rt::user_log!("IRQMUX_SRV_RECV_ERR err={:?}", error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register(service: &mut IrqMuxService, line: IrqLine) -> IrqMuxStatus {
        service.register_line(line, line + 32, IrqTriggerMode::Edge, IrqPolarity::High)
    }

    fn ready_route(service: &mut IrqMuxService, line: IrqLine, target: IrqRouteTarget) {
        assert_eq!(register(service, line), IrqMuxStatus::Ok);
        assert_eq!(service.bind_driver(line, target), IrqMuxStatus::Ok);
        assert_eq!(service.enable(line), IrqMuxStatus::Ok);
        assert_eq!(service.unmask(line), IrqMuxStatus::Ok);
    }

    #[test]
    fn irqmux_registers_line_disabled_and_masked() {
        let mut service = IrqMuxService::new();
        assert_eq!(register(&mut service, 4), IrqMuxStatus::Ok);
        let route = service.route(4).expect("registered route");
        assert_eq!(route.vector, 36);
        assert!(!route.enabled);
        assert!(route.masked);
        assert_eq!(service.route_count(), 1);
    }

    #[test]
    fn irqmux_rejects_duplicate_registration() {
        let mut service = IrqMuxService::new();
        assert_eq!(register(&mut service, 4), IrqMuxStatus::Ok);
        assert_eq!(register(&mut service, 4), IrqMuxStatus::AlreadyRegistered);
    }

    #[test]
    fn irqmux_unregister_clears_bound_route() {
        let mut service = IrqMuxService::new();
        ready_route(&mut service, 4, 77);
        assert_eq!(service.unregister_line(4), IrqMuxStatus::Ok);
        assert_eq!(service.route(4), None);
        assert_eq!(service.route_count(), 0);
    }

    #[test]
    fn irqmux_binds_driver_target_and_rejects_unknown_line() {
        let mut service = IrqMuxService::new();
        assert_eq!(service.bind_driver(4, 77), IrqMuxStatus::NotFound);
        assert_eq!(register(&mut service, 4), IrqMuxStatus::Ok);
        assert_eq!(service.bind_driver(4, 77), IrqMuxStatus::Ok);
        assert_eq!(service.route(4).and_then(|route| route.target), Some(77));
    }

    #[test]
    fn irqmux_enable_disable_controls_dispatch() {
        let mut service = IrqMuxService::new();
        ready_route(&mut service, 4, 77);
        assert_eq!(service.disable(4), IrqMuxStatus::Ok);
        assert_eq!(service.dispatch_fake_irq(4), IrqDispatchResult::Disabled);
        assert_eq!(service.enable(4), IrqMuxStatus::Ok);
        assert_eq!(
            service.dispatch_fake_irq(4),
            IrqDispatchResult::Delivered {
                line: 4,
                target: 77
            }
        );
    }

    #[test]
    fn irqmux_mask_unmask_controls_dispatch() {
        let mut service = IrqMuxService::new();
        ready_route(&mut service, 4, 77);
        assert_eq!(service.mask(4), IrqMuxStatus::Ok);
        assert_eq!(service.dispatch_fake_irq(4), IrqDispatchResult::Masked);
        assert_eq!(service.unmask(4), IrqMuxStatus::Ok);
        assert_eq!(
            service.dispatch_fake_irq(4),
            IrqDispatchResult::Delivered {
                line: 4,
                target: 77
            }
        );
    }

    #[test]
    fn irqmux_ack_rejects_unknown_line() {
        let service = IrqMuxService::new();
        assert_eq!(service.ack(8), IrqMuxStatus::NotFound);
    }

    #[test]
    fn irqmux_fake_dispatch_reports_all_route_gates() {
        let mut service = IrqMuxService::new();
        assert_eq!(
            service.dispatch_fake_irq(1),
            IrqDispatchResult::Unregistered
        );
        assert_eq!(register(&mut service, 1), IrqMuxStatus::Ok);
        assert_eq!(service.dispatch_fake_irq(1), IrqDispatchResult::Disabled);
        assert_eq!(service.enable(1), IrqMuxStatus::Ok);
        assert_eq!(service.dispatch_fake_irq(1), IrqDispatchResult::Masked);
        assert_eq!(service.unmask(1), IrqMuxStatus::Ok);
        assert_eq!(service.dispatch_fake_irq(1), IrqDispatchResult::NoTarget);
        assert_eq!(service.bind_driver(1, 100), IrqMuxStatus::Ok);
        assert_eq!(
            service.dispatch_fake_irq(1),
            IrqDispatchResult::Delivered {
                line: 1,
                target: 100
            }
        );
    }

    #[test]
    fn irqmux_route_table_enforces_capacity_and_reuses_slots() {
        let mut service = IrqMuxService::new();
        for line in 0..MAX_IRQ_ROUTES as IrqLine {
            assert_eq!(register(&mut service, line), IrqMuxStatus::Ok);
        }
        assert_eq!(register(&mut service, 1000), IrqMuxStatus::Busy);
        assert_eq!(service.unregister_line(7), IrqMuxStatus::Ok);
        assert_eq!(register(&mut service, 1000), IrqMuxStatus::Ok);
        assert_eq!(service.route_count(), MAX_IRQ_ROUTES);
    }

    #[test]
    fn irqmux_wire_handler_rejects_malformed_and_unsupported_messages() {
        let mut service = IrqMuxService::new();
        assert_eq!(
            service.handle_wire_request(1, &[0; 3]).status,
            IrqMuxStatus::BadRequest
        );
        assert_eq!(
            service
                .handle_wire_request(u16::MAX, &[0; IrqMuxRequest::ENCODED_LEN])
                .status,
            IrqMuxStatus::Unsupported
        );
    }
}
