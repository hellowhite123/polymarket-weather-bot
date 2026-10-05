# Round 05 阶段 B 设计草案（B1–B5）— WAITING_AUTHORIZATION

> 依 `docs/INSTRUCTION_TO_OPENCODE_ROUND05_P1_REMEDIATION.md` 阶段 B：**只做设计，禁止改代码**。
> 每个 B 项状态：**WAITING_AUTHORIZATION**（获人工授权后方可实施）。
> 基线 commit：见本次交付报告。（实施者不自证 VERIFIED。）

---

## B1 — P1-008 Reserve Accounting（order-level）

### 现状调用链确认
- 预留：`RiskEngine::check_and_reserve` → `PositionLimits::check_and_reserve`（Buy 按 `notional = qty×limit` 计入 per-token `reserved`；Sell 不预留）。
- 冲抵：`on_market_trade` → `RiskEngine::on_fill` → `PositionLimits::post_check`：`Buy { moved = reserved.min(fill_notional); reserved -= moved; notional += exposure_notional }`（`fill_notional = fill_qty×fill_price`）。
- 取消：`service.rs::settle_cancel` 按 `remaining_size×limit_price` 释放（A1 已改为先转换成功再释放）。

### 问题
预留按 **limit** 入池、按 **fill 价** 出池 → 价格改善/部分成交残留 `(limit−fill)×qty`，且无 order 维度可精确清算。

### 候选设计
- **方案 A（推荐，最小）**：`post_check` 的**释放额**改为 `fill_delta_qty × order.limit_price`（限价从 `OrderStore.get_by_exchange_order_id` 反查；未知则回退 fill notional）；暴露仍用 `fill_qty×fill_price`。终态（全成交/取消/过期）残余清零。
- **方案 B（更强）**：`PositionLimits` 增 `reserves: HashMap<order_id, (token, reserved, released)>`，释放/取消按 order 精确清算，池化值由聚合派生。
- **禁止**：改 Risk Model 阈值 / 改 Python 侧 accounting。

### 状态转移（冻结）
`ACTIVE → (FILLED | CANCELLED | EXPIRED)`；每单累计释放额必须恰等于其 `limit×qty`（全成交）或 `limit×(filled+remaining)`（部分+终态）。

### 边界案例
A 全成交价优；B 部分成交；C 部分+取消；D 多笔 partial；E 限价未知回退。

### Required Tests
`test_reserve_release_on_price_improvement`、`test_reserve_release_partial_then_cancel`、`test_reserve_release_limit_unknown_fallback`、`test_reserve_release_concurrent_fills`（mutation：退回 fill-notional 释放必 FAILED）。

### Allowed / Forbidden
Allowed：`fast/risk_engine.rs`（仅记账，不改阈值）、`grpc/service.rs`（反查限价）。Forbidden：`risk_gate.py`、`RiskConfig` 阈值、strategy、funds-control、proto。

---

## B2 — P1-009 Unknown Order Reconciliation

### 现状
未知结果订单停留 `OrderStateStatus::Received`（无 exchange id），而 `pending_exchange_order_ids()` 排除 `Received` → 对账输入集看不到，预留无释放路径。

### 设计要点（待授权）
1. **状态模型**：新增显式 `UnknownOutcome`（或保留 `Received` 但为其建立独立「in-doubt」集合）；`Received` 不得永久滞留而无出口。
2. **request fingerprint**：下单请求 (token, side, price, size, client_order_id) 指纹，用于远端匹配。
3. **判定优先级**：exchange open orders（按 fingerprint） → `/data/trades` 历史 → （若平台支持）client order id 精确查询 → 超 TTL 则 unresolved。
4. **幂等**：restart/reconcile 重放不重复应用；已解决→正常状态推进；未解决→保持 unresolved。
5. **驱动 TradingGate**：存在 unresolved/in-doubt → `reconcile_complete(false,false)`（停新单，FAIL-CLOSED），预留保守持有。

### Required Tests
in-doubt → 不进入 READY；remote open order 命中 → 推进为 Submitted；未决 → 预留不释放；重启重放 no-op。

### Allowed（建议）
`fast/order_state.rs`、`grpc/service.rs`、`bin/server.rs`、`fast/state_sync.rs`（不改 `risk_engine.rs` 阈值）。可选 proto 观测字段（需单独批准）。

---

## B3 — P1-RISK-UNIT-001 Python/Rust Risk Authority（**关键 P1**）

### 现状（E4 证据）
- `order.quantity` = **shares**（`PositionSizer`: `base_capital×f/cost`）。
- Rust：`notional = order.quantity × target_price`（`service.rs`）与 `RiskConfig.max_position_per_market`（USDC）比较。
- Python `risk_gate.py`：`new_total = existing + order.quantity`（**shares**）直接与 `max_position_per_market` 比较 → **Shares/USDC 量纲混淆**。
- 双权威：Python `AGG_MAX_POSITION=1000`（`aggregator.py`）vs Rust `DEFAULT_MAX_POSITION_PER_MARKET=500`（`risk_engine.rs`）；proto 未携带该限制。

### 待冻结定义
1. **position 正式单位**：USDC notional（与 Rust 一致）。
2. **global exposure 正式单位**：USDC notional。
3. **notional 公式（关键）**：
   - `BUY_YES`: `notional = qty × yes_price`
   - `BUY_NO`: `notional = qty × no_price = qty × (1 − yes_price)`
   - **禁止**用 `qty × target_price` 一刀切（BUY_NO 成本是 `1−yes_price`）。
4. **`position_notionals` 写入/回写链**：Python `RiskState.position_notionals`（已存在字段）如何从成交/结算回填，并用于 `risk_gate` 的 notional 比较。
5. **最终 authority**：**Rust RiskEngine 为唯一风险权威**；Python `risk_gate` 降级为 **local projection/advisory**（不得形成第三套）。
6. **配置同步**：`AGG_MAX_POSITION`/`AGG_MAX_GLOBAL_EXPOSURE` → Rust `RiskConfig`（env 或 proto 传递）单一来源；`AGG_*` 与 Rust 默认值必须一致化（当前 1000 vs 500 分歧）。
7. **proto**：是否需要新增配置/限制字段（供 Python↔Rust 同步）。

### Allowed（建议，待授权）
`strategy_engine/aggregator/risk_gate.py`、`strategy_engine/aggregator/models.py`、`aggregator.py`/`config`（配置同步）、`polymarket-rust .../risk_engine.rs`、`bin/server.rs`、`proto/polymarket.proto`（如需要）。Forbidden：改交易方向语义、cold wallet、EIP-712 标准实现。

### Required Tests
BUY_YES/BUY_NO notional 各一；shares≠USDC 场景不再误拒；Python/Rust 对同一订单风险判定一致；配置单一来源断言。

---

## B4 — P1-010 Snapshot/Event Authority

### 设计要点（待授权）
- 明确 **snapshot（下单响应/poll）** 与 **event（成交事件）** 谁拥有 filled 的最终 authority。
- 引入 **sequence/version 或 timestamp** 规则；**filled_size 是否允许回退**（建议：单调不减；恢复/对账修正走独立通道）。
- **reconciliation correction 覆盖正常事件**的优先级与幂等。
- **禁止**只加 `max()` 声称解决（掩盖 authority 缺失）。
- restart/reconcile 后不得重复应用事件。

Allowed（建议）：`grpc/service.rs`、`fast/order_state.rs`、`fast/state_sync.rs`。

---

## B5 — R5-005 Zeroization source-to-signing trace

### 待输出（默认不改密码学实现）
追踪 `encrypted keystore → decrypt(Zeroizing) → WalletManager::unlock → LocalWallet/SecretKey → sign → lock/drop` 全生命周期：
- 检查 `SecretKey` 是否把私钥再次复制到**非 Zeroizing** 缓冲；
- `Arc<LocalWallet>` 各 clone 的生命周期与 drop；
- hot_wallet 的 lock/TTL 清缓存行为。
发现明确缺陷且获授权后再改。

Allowed（建议）：`wallet/keystore.rs`、`wallet/manager.rs`、`fast/hot_wallet.rs`、`fast/signing_cache.rs`。

---

## 阶段 C — 不改代码项

- **C1 R5-009**：维持 **BLOCKED**（环境无 Docker）。验收命令见 `docs/DEPLOY_HEALTHCHECK_RUNBOOK.md` §5：
  `docker compose config` / `docker compose up -d --build` / `docker compose ps` /
  `docker inspect --format='{{json .State.Health}}' ai-weather-app` / 停止 market_discovery 后等待 ≥3 次 healthcheck 确认 `unhealthy`。
- **C2 文档状态**：实施者**不得**将 Finding 置 VERIFIED/CLOSED；仅报告 changed files / tests / limitations / 建议审查点。

> 以上 B1–B5 全部 **WAITING_AUTHORIZATION**：请人工逐项授权后再实施。
