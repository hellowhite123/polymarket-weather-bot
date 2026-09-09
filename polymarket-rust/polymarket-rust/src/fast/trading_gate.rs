use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingGateState {
    Starting,
    Initializing,
    Reconciling,
    RiskSync,
    Ready,
    Halted,
}

pub struct TradingGate {
    state: RwLock<TradingGateState>,
    reason: RwLock<Option<String>>,
}

impl TradingGate {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(TradingGateState::Starting),
            reason: RwLock::new(None),
        }
    }

    pub fn state(&self) -> TradingGateState {
        *self.state.read().expect("trading gate state lock poisoned")
    }

    pub fn set_state(&self, state: TradingGateState) {
        let mut current = self
            .state
            .write()
            .expect("trading gate state lock poisoned");

        *current = state;

        if state != TradingGateState::Halted {
            let mut reason = self
                .reason
                .write()
                .expect("trading gate reason lock poisoned");

            *reason = None;
        }
    }

    pub fn halt(&self, reason: impl Into<String>) {
        {
            let mut current = self
                .state
                .write()
                .expect("trading gate state lock poisoned");

            *current = TradingGateState::Halted;
        }

        {
            let mut stored_reason = self
                .reason
                .write()
                .expect("trading gate reason lock poisoned");

            *stored_reason = Some(reason.into());
        }
    }

    pub fn reason(&self) -> Option<String> {
        self.reason
            .read()
            .expect("trading gate reason lock poisoned")
            .clone()
    }

    pub fn ensure_ready(&self) -> Result<(), String> {
        let state = self.state();

        if state == TradingGateState::Ready {
            Ok(())
        } else {
            Err(format!(
                "trading gate is not ready: state={state:?}, reason={:?}",
                self.reason()
            ))
        }
    }

    /// 根据启动对账结果决定闸门状态。
    ///
    /// 仓位与订单都同步才进入 `Ready`，否则保持 `Reconciling`（FAIL-CLOSED）。
    /// 若当前已 `Halted`，则保持不变（halt 优先，不被对账覆盖）。
    pub fn reconcile_complete(&self, positions_in_sync: bool, orders_in_sync: bool) {
        if self.state() == TradingGateState::Halted {
            return;
        }
        if positions_in_sync && orders_in_sync {
            self.set_state(TradingGateState::Ready);
        } else {
            self.set_state(TradingGateState::Reconciling);
        }
    }
}

impl Default for TradingGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_state_is_starting() {
        let gate = TradingGate::new();
        assert_eq!(gate.state(), TradingGateState::Starting);
    }

    #[test]
    fn test_ensure_ready_blocks_when_starting() {
        let gate = TradingGate::new();
        assert!(gate.ensure_ready().is_err());
    }

    #[test]
    fn test_ensure_ready_blocks_when_reconciling() {
        let gate = TradingGate::new();
        gate.set_state(TradingGateState::Reconciling);
        assert!(gate.ensure_ready().is_err());
    }

    #[test]
    fn test_ensure_ready_allows_when_ready() {
        let gate = TradingGate::new();
        gate.set_state(TradingGateState::Ready);
        assert!(gate.ensure_ready().is_ok());
    }

    #[test]
    fn test_set_state_transition() {
        let gate = TradingGate::new();
        gate.set_state(TradingGateState::Initializing);
        assert_eq!(gate.state(), TradingGateState::Initializing);
        gate.set_state(TradingGateState::Reconciling);
        assert_eq!(gate.state(), TradingGateState::Reconciling);
        gate.set_state(TradingGateState::Ready);
        assert_eq!(gate.state(), TradingGateState::Ready);
    }

    #[test]
    fn test_halt_blocks_trading_and_records_reason() {
        let gate = TradingGate::new();
        gate.set_state(TradingGateState::Ready);
        gate.halt("reconcile mismatch");
        assert_eq!(gate.state(), TradingGateState::Halted);
        assert_eq!(gate.reason(), Some("reconcile mismatch".to_string()));
        assert!(gate.ensure_ready().is_err());
    }

    #[test]
    fn test_set_state_clears_reason_unless_halted() {
        let gate = TradingGate::new();
        gate.halt("some reason");
        assert_eq!(gate.reason(), Some("some reason".to_string()));
        gate.set_state(TradingGateState::Ready);
        assert_eq!(gate.reason(), None);
    }

    #[test]
    fn test_reconcile_complete_both_in_sync_enters_ready() {
        let gate = TradingGate::new();
        gate.reconcile_complete(true, true);
        assert_eq!(gate.state(), TradingGateState::Ready);
        assert!(gate.ensure_ready().is_ok());
    }

    #[test]
    fn test_reconcile_complete_position_mismatch_stays_reconciling() {
        let gate = TradingGate::new();
        gate.reconcile_complete(false, true);
        assert_eq!(gate.state(), TradingGateState::Reconciling);
        assert!(gate.ensure_ready().is_err());
    }

    #[test]
    fn test_reconcile_complete_order_mismatch_stays_reconciling() {
        let gate = TradingGate::new();
        gate.reconcile_complete(true, false);
        assert_eq!(gate.state(), TradingGateState::Reconciling);
        assert!(gate.ensure_ready().is_err());
    }

    #[test]
    fn test_reconcile_complete_both_mismatch_stays_reconciling() {
        let gate = TradingGate::new();
        gate.reconcile_complete(false, false);
        assert_eq!(gate.state(), TradingGateState::Reconciling);
        assert!(gate.ensure_ready().is_err());
    }

    #[test]
    fn test_reconcile_complete_does_not_override_halted() {
        let gate = TradingGate::new();
        gate.halt("api_key_init_failed");
        gate.reconcile_complete(true, true);
        assert_eq!(gate.state(), TradingGateState::Halted);
        assert_eq!(gate.reason(), Some("api_key_init_failed".to_string()));
        assert!(gate.ensure_ready().is_err());
    }
}
