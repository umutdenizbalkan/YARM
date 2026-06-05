// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::irqmux_abi::{
    IRQ_GRANT_RIGHT_ACK, IRQ_GRANT_RIGHT_BIND, IRQ_GRANT_RIGHT_ENABLE, IRQ_GRANT_RIGHT_MASK,
    IRQ_GRANT_RIGHT_REGISTER, IRQMUX_ROUTE_F_AUTHORIZED, IRQMUX_ROUTE_F_BOUND,
    IRQMUX_ROUTE_F_ENABLED, IRQMUX_ROUTE_F_MASKED, IRQMUX_ROUTE_F_REGISTERED, IrqGrantDescriptor,
    IrqGrantKey, IrqLine, IrqMuxCodecError, IrqMuxRequest, IrqMuxResponse, IrqMuxStatus,
    IrqPolarity, IrqRouteTarget, IrqTriggerMode, IrqVector,
};

pub const MAX_IRQ_ROUTES: usize = 32;
pub const MAX_IRQ_GRANTS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqRoute {
    pub line: IrqLine,
    pub vector: IrqVector,
    pub trigger: IrqTriggerMode,
    pub polarity: IrqPolarity,
    pub owner: IrqGrantKey,
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
    grants: [Option<IrqGrantDescriptor>; MAX_IRQ_GRANTS],
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
            grants: [None; MAX_IRQ_GRANTS],
            stats: IrqMuxStats {
                delivered: 0,
                masked: 0,
                disabled: 0,
                unregistered: 0,
                no_target: 0,
            },
        }
    }

    pub fn authorize_grant(&mut self, grant: IrqGrantDescriptor) -> IrqMuxStatus {
        if !grant.is_valid() {
            return IrqMuxStatus::BadRequest;
        }
        if let Some(index) = self.grant_index(grant.key.grant_id) {
            let current = self.grants[index].expect("grant index contains descriptor");
            if current == grant {
                return IrqMuxStatus::Ok;
            }
            if !same_grant_subject(current, grant) {
                return IrqMuxStatus::GrantMismatch;
            }
            if grant.key.generation <= current.key.generation {
                return IrqMuxStatus::GrantStale;
            }
            self.grants[index] = Some(grant);
            if let Some(route) = self
                .routes
                .iter_mut()
                .filter_map(Option::as_mut)
                .find(|route| route.owner.grant_id == grant.key.grant_id)
            {
                route.owner = grant.key;
                route.target = None;
                route.enabled = false;
                route.masked = true;
            }
            return IrqMuxStatus::Ok;
        }
        if self
            .grants
            .iter()
            .flatten()
            .any(|current| current.irq_line == grant.irq_line)
        {
            return IrqMuxStatus::Busy;
        }
        let Some(slot) = self.grants.iter_mut().find(|slot| slot.is_none()) else {
            return IrqMuxStatus::Busy;
        };
        *slot = Some(grant);
        IrqMuxStatus::Ok
    }

    pub fn revoke_grant(&mut self, key: IrqGrantKey) -> IrqMuxStatus {
        let index = match self.validate_grant_key(key) {
            Ok(index) => index,
            Err(status) => return status,
        };
        self.grants[index] = None;
        if let Some(route) = self
            .routes
            .iter_mut()
            .filter_map(Option::as_mut)
            .find(|route| route.owner == key)
        {
            route.target = None;
            route.enabled = false;
            route.masked = true;
        }
        IrqMuxStatus::Ok
    }

    pub fn register_line(
        &mut self,
        key: IrqGrantKey,
        line: IrqLine,
        vector: IrqVector,
        trigger: IrqTriggerMode,
        polarity: IrqPolarity,
    ) -> IrqMuxStatus {
        let grant = match self.authorized_grant(key, IRQ_GRANT_RIGHT_REGISTER) {
            Ok(grant) => grant,
            Err(status) => return status,
        };
        if grant.irq_line != line
            || grant.irq_vector != vector
            || grant.trigger != trigger
            || grant.polarity != polarity
        {
            return IrqMuxStatus::GrantMismatch;
        }
        if self.route(grant.irq_line).is_some() {
            return IrqMuxStatus::AlreadyRegistered;
        }
        let Some(slot) = self.routes.iter_mut().find(|slot| slot.is_none()) else {
            return IrqMuxStatus::Busy;
        };
        *slot = Some(IrqRoute {
            line: grant.irq_line,
            vector: grant.irq_vector,
            trigger: grant.trigger,
            polarity: grant.polarity,
            owner: grant.key,
            target: None,
            enabled: false,
            masked: true,
        });
        IrqMuxStatus::Ok
    }

    pub fn unregister_line(&mut self, key: IrqGrantKey, line: IrqLine) -> IrqMuxStatus {
        let grant_index = match self.authorized_route(key, line, IRQ_GRANT_RIGHT_REGISTER) {
            Ok((grant_index, _)) => grant_index,
            Err(status) => return status,
        };
        let route_index = self.route_index(line).expect("authorized route exists");
        self.routes[route_index] = None;
        self.grants[grant_index] = None;
        IrqMuxStatus::Ok
    }

    pub fn bind_driver(
        &mut self,
        key: IrqGrantKey,
        line: IrqLine,
        target: IrqRouteTarget,
    ) -> IrqMuxStatus {
        if target == 0 {
            return IrqMuxStatus::BadRequest;
        }
        if let Err(status) = self.authorized_route(key, line, IRQ_GRANT_RIGHT_BIND) {
            return status;
        }
        let route = self.route_mut(line).expect("authorized route exists");
        if route.target.is_some() {
            return IrqMuxStatus::Busy;
        }
        route.target = Some(target);
        IrqMuxStatus::Ok
    }

    pub fn unbind_driver(&mut self, key: IrqGrantKey, line: IrqLine) -> IrqMuxStatus {
        if let Err(status) = self.authorized_route(key, line, IRQ_GRANT_RIGHT_BIND) {
            return status;
        }
        let route = self.route_mut(line).expect("authorized route exists");
        if route.target.take().is_none() {
            return IrqMuxStatus::NotFound;
        }
        IrqMuxStatus::Ok
    }

    pub fn enable(&mut self, key: IrqGrantKey, line: IrqLine) -> IrqMuxStatus {
        self.set_enabled(key, line, true)
    }

    pub fn disable(&mut self, key: IrqGrantKey, line: IrqLine) -> IrqMuxStatus {
        self.set_enabled(key, line, false)
    }

    pub fn mask(&mut self, key: IrqGrantKey, line: IrqLine) -> IrqMuxStatus {
        self.set_masked(key, line, true)
    }

    pub fn unmask(&mut self, key: IrqGrantKey, line: IrqLine) -> IrqMuxStatus {
        self.set_masked(key, line, false)
    }

    pub fn ack(&self, key: IrqGrantKey, line: IrqLine) -> IrqMuxStatus {
        match self.authorized_route(key, line, IRQ_GRANT_RIGHT_ACK) {
            Ok(_) => IrqMuxStatus::Ok,
            Err(status) => status,
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
        let line = match request {
            IrqMuxRequest::AuthorizeGrant { grant } => grant.irq_line,
            IrqMuxRequest::RevokeGrant { key } => {
                self.grant(key.grant_id).map_or(0, |grant| grant.irq_line)
            }
            _ => request.line(),
        };
        let status = match request {
            IrqMuxRequest::AuthorizeGrant { grant } => self.authorize_grant(grant),
            IrqMuxRequest::RevokeGrant { key } => self.revoke_grant(key),
            IrqMuxRequest::RegisterLine {
                key,
                line,
                vector,
                trigger,
                polarity,
            } => self.register_line(key, line, vector, trigger, polarity),
            IrqMuxRequest::UnregisterLine { key, line } => self.unregister_line(key, line),
            IrqMuxRequest::BindDriver { key, line, target } => self.bind_driver(key, line, target),
            IrqMuxRequest::UnbindDriver { key, line } => self.unbind_driver(key, line),
            IrqMuxRequest::Enable { key, line } => self.enable(key, line),
            IrqMuxRequest::Disable { key, line } => self.disable(key, line),
            IrqMuxRequest::Mask { key, line } => self.mask(key, line),
            IrqMuxRequest::Unmask { key, line } => self.unmask(key, line),
            IrqMuxRequest::Ack { key, line } => self.ack(key, line),
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
            Err(IrqMuxCodecError::Malformed | IrqMuxCodecError::UnknownRights) => {
                self.response(0, IrqMuxStatus::BadRequest)
            }
            Err(IrqMuxCodecError::UnsupportedOpcode) => self.response(0, IrqMuxStatus::Unsupported),
        }
    }

    pub fn route(&self, line: IrqLine) -> Option<IrqRoute> {
        self.route_index(line).and_then(|index| self.routes[index])
    }

    pub fn grant(&self, grant_id: u64) -> Option<IrqGrantDescriptor> {
        self.grant_index(grant_id)
            .and_then(|index| self.grants[index])
    }

    pub fn route_count(&self) -> usize {
        self.routes.iter().flatten().count()
    }

    pub fn grant_count(&self) -> usize {
        self.grants.iter().flatten().count()
    }

    pub const fn stats(&self) -> IrqMuxStats {
        self.stats
    }

    fn grant_index(&self, grant_id: u64) -> Option<usize> {
        self.grants
            .iter()
            .position(|grant| grant.is_some_and(|grant| grant.key.grant_id == grant_id))
    }

    fn validate_grant_key(&self, key: IrqGrantKey) -> Result<usize, IrqMuxStatus> {
        let Some(index) = self.grant_index(key.grant_id) else {
            return Err(IrqMuxStatus::GrantNotFound);
        };
        let grant = self.grants[index].expect("grant index contains descriptor");
        if grant.key.driver_id != key.driver_id {
            return Err(IrqMuxStatus::GrantMismatch);
        }
        if key.generation < grant.key.generation {
            return Err(IrqMuxStatus::GrantStale);
        }
        if key.generation != grant.key.generation {
            return Err(IrqMuxStatus::GrantMismatch);
        }
        Ok(index)
    }

    fn authorized_grant(
        &self,
        key: IrqGrantKey,
        required_right: u32,
    ) -> Result<IrqGrantDescriptor, IrqMuxStatus> {
        let index = self.validate_grant_key(key)?;
        let grant = self.grants[index].expect("grant index contains descriptor");
        if grant.rights & required_right == 0 {
            Err(IrqMuxStatus::RightsMissing)
        } else {
            Ok(grant)
        }
    }

    fn authorized_route(
        &self,
        key: IrqGrantKey,
        line: IrqLine,
        required_right: u32,
    ) -> Result<(usize, IrqRoute), IrqMuxStatus> {
        let grant_index = self.validate_grant_key(key)?;
        let grant = self.grants[grant_index].expect("grant index contains descriptor");
        if grant.rights & required_right == 0 {
            return Err(IrqMuxStatus::RightsMissing);
        }
        if grant.irq_line != line {
            return Err(IrqMuxStatus::GrantMismatch);
        }
        let Some(route) = self.route(line) else {
            return Err(IrqMuxStatus::NotFound);
        };
        if route.owner != key {
            return Err(IrqMuxStatus::Unauthorized);
        }
        Ok((grant_index, route))
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

    fn set_enabled(&mut self, key: IrqGrantKey, line: IrqLine, enabled: bool) -> IrqMuxStatus {
        if let Err(status) = self.authorized_route(key, line, IRQ_GRANT_RIGHT_ENABLE) {
            return status;
        }
        self.route_mut(line)
            .expect("authorized route exists")
            .enabled = enabled;
        IrqMuxStatus::Ok
    }

    fn set_masked(&mut self, key: IrqGrantKey, line: IrqLine, masked: bool) -> IrqMuxStatus {
        if let Err(status) = self.authorized_route(key, line, IRQ_GRANT_RIGHT_MASK) {
            return status;
        }
        self.route_mut(line)
            .expect("authorized route exists")
            .masked = masked;
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
                key: None,
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
        if self.validate_grant_key(route.owner).is_ok() {
            route_flags |= IRQMUX_ROUTE_F_AUTHORIZED;
        }
        IrqMuxResponse {
            status,
            route_flags,
            line,
            vector: route.vector,
            target: route.target.unwrap_or(0),
            key: Some(route.owner),
            trigger: Some(route.trigger),
            polarity: Some(route.polarity),
        }
    }
}

fn same_grant_subject(current: IrqGrantDescriptor, next: IrqGrantDescriptor) -> bool {
    current.key.grant_id == next.key.grant_id
        && current.key.driver_id == next.key.driver_id
        && current.irq_line == next.irq_line
        && current.irq_vector == next.irq_vector
        && current.trigger == next.trigger
        && current.polarity == next.polarity
}

pub fn run() {
    yarm_user_rt::user_log!("IRQMUX_SRV_ENTRY");
    let mut service = IrqMuxService::new();
    yarm_user_rt::user_log!(
        "IRQMUX_SRV_READY routes={} grants={}",
        service.route_count(),
        service.grant_count()
    );

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
    use yarm_ipc_abi::irqmux_abi::IRQ_GRANT_RIGHT_ALL;

    fn grant(grant_id: u64, driver_id: u64, line: IrqLine) -> IrqGrantDescriptor {
        IrqGrantDescriptor {
            key: IrqGrantKey::new(grant_id, driver_id, 1),
            irq_line: line,
            irq_vector: line + 32,
            rights: IRQ_GRANT_RIGHT_ALL,
            trigger: IrqTriggerMode::Edge,
            polarity: IrqPolarity::High,
        }
    }

    fn authorize(service: &mut IrqMuxService, grant: IrqGrantDescriptor) {
        assert_eq!(service.authorize_grant(grant), IrqMuxStatus::Ok);
    }

    fn register(service: &mut IrqMuxService, grant: IrqGrantDescriptor) {
        authorize(service, grant);
        assert_eq!(
            service.register_line(
                grant.key,
                grant.irq_line,
                grant.irq_vector,
                grant.trigger,
                grant.polarity,
            ),
            IrqMuxStatus::Ok
        );
    }

    fn ready_route(service: &mut IrqMuxService, grant: IrqGrantDescriptor, target: u64) {
        register(service, grant);
        assert_eq!(
            service.bind_driver(grant.key, grant.irq_line, target),
            IrqMuxStatus::Ok
        );
        assert_eq!(service.enable(grant.key, grant.irq_line), IrqMuxStatus::Ok);
        assert_eq!(service.unmask(grant.key, grant.irq_line), IrqMuxStatus::Ok);
    }

    #[test]
    fn irqmux_authorized_register_succeeds_and_unauthorized_register_fails() {
        let mut service = IrqMuxService::new();
        let descriptor = grant(1, 7, 4);
        assert_eq!(
            service.register_line(
                descriptor.key,
                descriptor.irq_line,
                descriptor.irq_vector,
                descriptor.trigger,
                descriptor.polarity,
            ),
            IrqMuxStatus::GrantNotFound
        );
        authorize(&mut service, descriptor);
        assert_eq!(
            service.register_line(
                descriptor.key,
                descriptor.irq_line,
                descriptor.irq_vector,
                descriptor.trigger,
                descriptor.polarity,
            ),
            IrqMuxStatus::Ok
        );
        let route = service.route(4).expect("registered route");
        assert_eq!(route.owner, descriptor.key);
        assert!(!route.enabled);
        assert!(route.masked);
    }

    #[test]
    fn irqmux_rejects_wrong_driver_line_and_vector_authorizations() {
        let mut service = IrqMuxService::new();
        let descriptor = grant(1, 7, 4);
        authorize(&mut service, descriptor);
        let wrong_driver = IrqGrantKey::new(1, 8, 1);
        assert_eq!(
            service.register_line(
                wrong_driver,
                descriptor.irq_line,
                descriptor.irq_vector,
                descriptor.trigger,
                descriptor.polarity,
            ),
            IrqMuxStatus::GrantMismatch
        );
        let mut conflicting = grant(2, 9, 4);
        conflicting.irq_vector = 99;
        assert_eq!(service.authorize_grant(conflicting), IrqMuxStatus::Busy);
        assert_eq!(
            service.register_line(
                descriptor.key,
                descriptor.irq_line + 1,
                descriptor.irq_vector,
                descriptor.trigger,
                descriptor.polarity,
            ),
            IrqMuxStatus::GrantMismatch
        );
        assert_eq!(
            service.register_line(
                descriptor.key,
                descriptor.irq_line,
                descriptor.irq_vector + 1,
                descriptor.trigger,
                descriptor.polarity,
            ),
            IrqMuxStatus::GrantMismatch
        );
        assert_eq!(
            service.register_line(
                descriptor.key,
                descriptor.irq_line,
                descriptor.irq_vector,
                descriptor.trigger,
                descriptor.polarity,
            ),
            IrqMuxStatus::Ok
        );
        assert_eq!(
            service.bind_driver(descriptor.key, 5, 88),
            IrqMuxStatus::GrantMismatch
        );
    }

    #[test]
    fn irqmux_rejects_stale_generation_and_rotates_new_generation_safely() {
        let mut service = IrqMuxService::new();
        let first = IrqGrantDescriptor {
            key: IrqGrantKey::new(1, 7, 2),
            ..grant(1, 7, 4)
        };
        ready_route(&mut service, first, 77);
        let stale = IrqGrantDescriptor {
            key: IrqGrantKey::new(1, 7, 1),
            ..first
        };
        assert_eq!(service.authorize_grant(stale), IrqMuxStatus::GrantStale);
        let next = IrqGrantDescriptor {
            key: IrqGrantKey::new(1, 7, 3),
            ..first
        };
        assert_eq!(service.authorize_grant(next), IrqMuxStatus::Ok);
        let route = service.route(4).expect("route retained across rotation");
        assert_eq!(route.owner, next.key);
        assert_eq!(route.target, None);
        assert!(!route.enabled);
        assert!(route.masked);
        assert_eq!(service.enable(first.key, 4), IrqMuxStatus::GrantStale);
    }

    #[test]
    fn irqmux_missing_rights_rejects_control_operation() {
        let mut service = IrqMuxService::new();
        let descriptor = IrqGrantDescriptor {
            rights: IRQ_GRANT_RIGHT_REGISTER,
            ..grant(1, 7, 4)
        };
        register(&mut service, descriptor);
        assert_eq!(
            service.bind_driver(descriptor.key, 4, 77),
            IrqMuxStatus::RightsMissing
        );
        assert_eq!(
            service.enable(descriptor.key, 4),
            IrqMuxStatus::RightsMissing
        );
        assert_eq!(service.ack(descriptor.key, 4), IrqMuxStatus::RightsMissing);
    }

    #[test]
    fn irqmux_revoke_unbinds_disables_masks_and_prevents_control() {
        let mut service = IrqMuxService::new();
        let descriptor = grant(1, 7, 4);
        ready_route(&mut service, descriptor, 77);
        assert_eq!(service.revoke_grant(descriptor.key), IrqMuxStatus::Ok);
        let route = service.route(4).expect("revocation retains inert route");
        assert_eq!(route.target, None);
        assert!(!route.enabled);
        assert!(route.masked);
        assert_eq!(
            service.bind_driver(descriptor.key, 4, 99),
            IrqMuxStatus::GrantNotFound
        );
        assert_eq!(
            service.enable(descriptor.key, 4),
            IrqMuxStatus::GrantNotFound
        );
    }

    #[test]
    fn irqmux_duplicate_authorize_is_idempotent_and_mismatch_is_rejected() {
        let mut service = IrqMuxService::new();
        let descriptor = grant(1, 7, 4);
        authorize(&mut service, descriptor);
        assert_eq!(service.authorize_grant(descriptor), IrqMuxStatus::Ok);
        let mismatch = IrqGrantDescriptor {
            key: IrqGrantKey::new(1, 8, 2),
            ..descriptor
        };
        assert_eq!(
            service.authorize_grant(mismatch),
            IrqMuxStatus::GrantMismatch
        );
        assert_eq!(service.grant_count(), 1);
    }

    #[test]
    fn irqmux_unregister_clears_route_and_authorization() {
        let mut service = IrqMuxService::new();
        let descriptor = grant(1, 7, 4);
        ready_route(&mut service, descriptor, 77);
        assert_eq!(service.unregister_line(descriptor.key, 4), IrqMuxStatus::Ok);
        assert_eq!(service.route(4), None);
        assert_eq!(service.grant(1), None);
    }

    #[test]
    fn irqmux_enable_disable_mask_unmask_and_ack_require_owner() {
        let mut service = IrqMuxService::new();
        let owner = grant(1, 7, 4);
        let other = grant(2, 8, 5);
        register(&mut service, owner);
        authorize(&mut service, other);
        assert_eq!(service.enable(other.key, 4), IrqMuxStatus::GrantMismatch);
        assert_eq!(service.enable(owner.key, 4), IrqMuxStatus::Ok);
        assert_eq!(service.disable(owner.key, 4), IrqMuxStatus::Ok);
        assert_eq!(service.unmask(owner.key, 4), IrqMuxStatus::Ok);
        assert_eq!(service.mask(owner.key, 4), IrqMuxStatus::Ok);
        assert_eq!(service.ack(owner.key, 4), IrqMuxStatus::Ok);
    }

    #[test]
    fn irqmux_fake_dispatch_behavior_is_unchanged_for_authorized_route() {
        let mut service = IrqMuxService::new();
        let descriptor = grant(1, 7, 4);
        assert_eq!(
            service.dispatch_fake_irq(4),
            IrqDispatchResult::Unregistered
        );
        register(&mut service, descriptor);
        assert_eq!(service.dispatch_fake_irq(4), IrqDispatchResult::Disabled);
        assert_eq!(service.enable(descriptor.key, 4), IrqMuxStatus::Ok);
        assert_eq!(service.dispatch_fake_irq(4), IrqDispatchResult::Masked);
        assert_eq!(service.unmask(descriptor.key, 4), IrqMuxStatus::Ok);
        assert_eq!(service.dispatch_fake_irq(4), IrqDispatchResult::NoTarget);
        assert_eq!(service.bind_driver(descriptor.key, 4, 77), IrqMuxStatus::Ok);
        assert_eq!(
            service.dispatch_fake_irq(4),
            IrqDispatchResult::Delivered {
                line: 4,
                target: 77
            }
        );
    }

    #[test]
    fn irqmux_grant_and_route_tables_enforce_capacity() {
        let mut service = IrqMuxService::new();
        for index in 0..MAX_IRQ_GRANTS {
            let descriptor = grant(index as u64 + 1, index as u64 + 1, index as u32);
            authorize(&mut service, descriptor);
            assert_eq!(
                service.register_line(
                    descriptor.key,
                    descriptor.irq_line,
                    descriptor.irq_vector,
                    descriptor.trigger,
                    descriptor.polarity,
                ),
                IrqMuxStatus::Ok
            );
        }
        assert_eq!(service.grant_count(), MAX_IRQ_GRANTS);
        assert_eq!(service.route_count(), MAX_IRQ_ROUTES);
        assert_eq!(
            service.authorize_grant(grant(100, 100, 100)),
            IrqMuxStatus::Busy
        );
    }

    #[test]
    fn irqmux_wire_handler_rejects_malformed_unknown_rights_and_unsupported_messages() {
        let mut service = IrqMuxService::new();
        assert_eq!(
            service.handle_wire_request(1, &[0; 3]).status,
            IrqMuxStatus::BadRequest
        );
        let descriptor = grant(1, 7, 4);
        let (opcode, mut payload) = IrqMuxRequest::AuthorizeGrant { grant: descriptor }.encode();
        payload[32..36].copy_from_slice(&(1u32 << 31).to_le_bytes());
        assert_eq!(
            service.handle_wire_request(opcode, &payload).status,
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
