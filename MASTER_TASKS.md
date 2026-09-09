# MASTER_TASKS.md

**唯一工程修改任务来源。** 实施者（Work Buddy）默认只以本文件作为代码修改授权依据。

- 未经本文件明确授权的任务，实施者不得自行改动交易语义 / Risk Model / Strategy / 资金控制逻辑。
- 每个任务一次只做一个 Task Group，完成后走 `docs/CODE_REVIEW_STANDARD.md` 的验收门禁。
- 状态机：`TODO → IN_PROGRESS → FIXED → VERIFIED → CLOSED`（VERIFIED 由非实施者判定）。

---

## 任务清单

### 第一优先级（上线前必须闭环）

---

#### P0-10 StateSync full_reconcile 接入启动流程

- **Status**: TODO
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`、`polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- **Required Tests**:
  - `full_reconcile_called_on_startup`
  - `reconcile_detects_remote_local_mismatch`
- **Acceptance**: 启动流程调用 `RiskEngine.snapshot_positions() → StateSync.full_reconcile()`；对账结果驱动 TradingGate。

---

#### P0-11 TradingGate 进入 READY

- **Status**: TODO
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
  - `polymarket-rust/polymarket-rust/src/fast/trading_gate.rs`
- **Forbidden**: `strategy_engine/`
- **Required Tests**:
  - `trading_gate_blocks_submit_when_not_ready`
  - `gate_transitions_to_ready_after_reconcile_ok`
- **Acceptance**: 只有 `TradingGateState::Ready` 才能通过 `ensure_ready()` 提交真实订单；对账失败时闸门保持关闭。

---

#### P0-13 成交回报连接 RiskEngine.post_check

- **Status**: TODO
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/executor.rs`
  - `polymarket-rust/polymarket-rust/src/fast/fill_records.rs`
  - `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- **Forbidden**: `strategy_engine/`
- **Required Tests**:
  - `fill_triggers_post_check_position_update`
  - `post_check_updates_quantity_and_notional`
- **Acceptance**: 调用链 `FastExecutor Fill → OrderStore → RiskEngine.post_check()` 完整闭环，Buy `quantity += qty; notional += value`，Sell `quantity -= qty`。

---

### 第二优先级

---

#### P0-12 Pending Orders 状态同步

- **Status**: TODO
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
  - `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
- **Forbidden**: `strategy_engine/`
- **Required Tests**:
  - `pending_orders_synced_from_order_store`
- **Acceptance**: `OrderStore → HashSet<OrderID> → StateSync.reconcile_orders()` 接通。

---

#### P0-14 StateSync 后台周期对账循环

- **Status**: TODO
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
  - `polymarket-rust/polymarket-rust/src/fast/daemon.rs`
- **Forbidden**: `strategy_engine/`
- **Required Tests**:
  - `background_reconcile_runs_every_30s`
- **Acceptance**: tokio 后台任务每 30s 执行 fetch positions → fetch orders → reconcile → 更新状态。

---

#### P0-02 Docker Secret 与环境变量一致性

- **Status**: DONE（09-09 修复 + 4 测试全绿 + commit a963231）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `Dockerfile`
  - `docker-compose.yml`
  - `docker-entrypoint.sh`
  - `polymarket-rust/polymarket-rust/src/wallet/keystore.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`
- **Required Tests**:
  - `secret_loaded_consistently_in_docker`
- **Acceptance**: Docker 侧 `WALLET_PASSWORD_FILE` 与 Rust 侧 `WALLET_PASSWORD` 读取链路统一，无不一致风险。

---

### 待 VERIFIED（代码已改，缺三重证据）

以下 9 项按新标准为 `FIXED`，需补 git diff + 测试 + 调用链证据后由非实施者判定 VERIFIED：

- P0-01 FastExecutor API Key / Warmup
- P0-03 RiskEngine pre_check 接入
- P0-04 FastExecutor 真实执行路径
- P0-05 幂等保护
- P0-06 post_check 基础实现（⚠️ 需验证真实成交路径调用）
- P0-07 TradingGate 状态机
- P0-08 StateSync 仓位接口
- P0-09 Position 对账逻辑

> 证据与验证结论见 `audits/round_04/`（待启动）。
