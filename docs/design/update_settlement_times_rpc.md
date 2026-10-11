# 设计方案：UpdateSettlementTimes RPC（P0-RISK-002 结算窗口）— Draft v3

- **状态**：**Draft v3** — 已按第二轮整改要求（P0-DESIGN-001~004 + P1）修订，**待独立审查**。未通过前不得实施。
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G1-002、`BUG_TRACKER.md` P0-RISK-002、`docs/design/CONSENSUS_DESIGN_ROUND_01.md`
- **依赖接口变更**：是（proto 新增 RPC）。
- **证据基线**：`file:line` 基于 commit `8f22902`，**未经独立审查确认**。
- **本阶段仅设计**；提交后 **STOP**，不进入代码实施。

---

## 0. V2 → V3 修改摘要（对应整改要求）

| 整改项 | 问题 | V3 解决 |
|--------|------|---------|
| **P0-DESIGN-001** | Unknown fail-closed 与「禁改 `TimeWindowGuard` 判定逻辑」冲突 | 明确 fail-closed 发生在 **`place_order` 内的 `settlement_gate`（`TimeWindowGuard` 之外）**；仅新增「registry→TimeWindowGuard 数据替换」的最小数据方法，**不改窗口计算**（§2/§3/§11） |
| **P0-DESIGN-002** | STALE 下已注册 token 的 Buy 行为未冻结 | 冻结为 **STALE ⇒ 所有 Buy 拒绝、Sell 允许**（§4） |
| **P0-DESIGN-003** | Freshness 只看 transport 未看 snapshot | 定义 **transport_age + snapshot_age 双窗口**；缺 snapshot_taken_at / 未来时间 / clock skew 超限 ⇒ invalid（§5） |
| **P0-DESIGN-004** | `replace_all` 语义自相矛盾 | 统一：`true` = 原子全量替换（缺失即删）；`false` = 原子 upsert；禁止 clear+insert（§6/§7） |
| **P1** | Human Acceptance 参数未冻结 | 提供冻结表 + 推荐默认值（§13） |
| **P1** | 无 G1-001 确认时 registry 无界增长 | 增加**增长保护**（上限 + 冷归档），不删除未确认 settlement 的 token（§10） |

---

## 1. Root Cause（现状与证据）

结算前「冻结窗口禁开新仓」**已实现但两端未接通**：

1. `TimeWindowGuard::pre_check`（`fast/risk_engine.rs:456-483`）逻辑完整；注册入口 `set_settlement(s)`（`:443-454` / `RiskEngine::set_settlement(s)` `:1115-1121`）**无生产调用**。
2. `RiskStateInternal::to_proto` 写死 `settlement_times: Vec::new()`（`grpc/service.rs:224`）。
3. Python `risk_gate._check_settlement_window`（`risk_gate.py:80-105`）依赖 Rust 回传的 `settlement_times`，恒空 ⇒ 同样失效。
4. 无 Python→Rust 推送通道（`proto/polymarket.proto:243-272` 仅 `GetRiskState`/`StreamRiskStateUpdates`）。
5. 数据源存在：`market_discovery/models.py:76` 的 `end_date_utc`。

**并发现设计冲突**：`TimeWindowGuard::pre_check` 对**未注册** token 返回 `Ok`（`:466-482`），即 Unknown ⇒ 放行；若不改该判定，则「Unknown 拒绝」必须落在**其它 production gate**。

---

## 2. Production Call Chain（唯一、可验证）

**冻结：Unknown / STALE 的 fail-closed 发生在 `place_order` 的 `settlement_gate`，位于 `TimeWindowGuard` 之外。**

```text
Python producer (market_discovery end_date_utc)
   │  gRPC UpdateSettlementTimes
   ▼
ExecutionServiceImpl::update_settlement_times
   │  validate → build new snapshot → atomic publish (Arc)
   │  → registry.replace_settlements(...)  // 仅“数据替换”，不改窗口计算
   ▼
SettlementRegistry (Arc<Snapshot>)
   ├──────────────► settlement_gate.check(token_id, side)   [NEW]
   │                    │  presence / validity / freshness / STALE
   │                    ▼
   │              place_order():  gate 先于 risk；Buy 拒绝时直接返回 Rejected
   │
   └──────────────► TimeWindowGuard (registry 数据，窗口计算不变)

place_order 顺序：
  ensure_ready()
    → direction_to_side()
    → claim_or_retry()
    → 【NEW】settlement_gate.check(token_id, side)      ← Unknown/STALE fail-closed
    → gas_gwei_for_risk()
    → RiskEngine.check_and_reserve(...)
          → check_gates()
              → TimeWindowGuard.pre_check()             ← 既有窗口逻辑（注册 token）
    → fast path / execute
```

- **唯一性**：Unknown/无效/STALE 只由 `settlement_gate` 决定；窗口时长只由 `TimeWindowGuard` 决定。
- **单一数据源**：gate 与 `TimeWindowGuard` 使用**同一 registry 快照**；发布时一并刷新 `TimeWindowGuard` 数据（§7）。
- **可验证**：§12 测试走真实 `place_order` 路径断言。

---

## 3. Unknown Settlement 语义（冻结）

对进入 `settlement_gate` 的 token：

| registry 状态 | Buy | Sell |
|---------------|-----|------|
| 包含 token（且 FRESH） | 继续 → `TimeWindowGuard` 正常窗口判断 | 继续（窗口检查天然放行 Sell） |
| **不包含 token** | **Reject** | **Allow** |
| registry invalid / NEVER_SYNCED / STALE | **Reject** | **Allow** |

- 「不包含」判定基于**同一次决策的单一快照版本**（§7）。
- 仅改变 Unknown 的处置；**不得改变**已注册 token 的窗口阈值与计算逻辑。
- 拒绝时返回 `ExecutionStatus::Rejected`，`error_message` 标注 `unknown_settlement`（便于观测）。

**冲突解决声明**：本方案**不修改** `TimeWindowGuard::pre_check` 的判定逻辑；`risk_engine.rs` 若需改动，仅限「registry 数据替换」这类**数据方法**（§11）。

---

## 4. STALE 语义（冻结，P0-DESIGN-002）

**基线（推荐，冻结）**：

```text
NEVER_SYNCED : Buy → Reject ; Sell → Allow
FRESH        : Known → 正常窗口判断 ; Unknown → Reject Buy / Allow Sell
STALE        : Buy → Reject ; Sell → Allow          ← 冻结：STALE 下所有 Buy 停止
PUSH_FAILED within freshness : 继续使用最后有效 snapshot（按 FRESH 判定）
PUSH_FAILED and freshness expired : → STALE
```

理由：旧 snapshot 可能已过时，无法证明「已知 token 仍在窗口外」；一旦数据不新鲜，最保守做法是**停止全部 Buy**。

> 若审查方要求保留「STALE + known token 仍可 Buy」，须作为 Human Acceptance 例外项并说明理由，不得由实施者自行决定。

---

## 5. Freshness 定义（transport + snapshot，P0-DESIGN-003）

### 5.1 双年龄

```text
transport_age = now - last_success_at
snapshot_age  = now - snapshot_taken_at
```

### 5.2 FRESH 判定

```text
FRESH ⇔  transport_age  <= transport_freshness_window
     AND snapshot_age   <= snapshot_freshness_window
     AND snapshot 合法（见 5.3）
否则 STALE
```

### 5.3 异常处理（全部 fail-closed）

| 情况 | 处理 |
|------|------|
| `snapshot_taken_at` 缺失 | **invalid**（不得自动视为 now）→ 视为 NEVER_SYNCED/STALE |
| `snapshot_taken_at` 在未来 | **reject snapshot** → STALE（不得改本地时间“修正”） |
| clock skew（`snapshot_taken_at` 相对 `last_success_at` 偏差）超容差 | **invalid** → STALE |
| RPC 成功但 snapshot 已旧 | 仍 STALE |
| RPC 失败但仍在 freshness 内 | 保持 FRESH（用最后有效快照） |

### 5.4 状态机

```text
NEVER_SYNCED ──(valid snapshot)──► FRESH ──(freshness 过期)──► STALE
FRESH ──(push fail, 未过期)──► FRESH(计数++)  ──(过期)──► STALE
STALE ──(新的有效且新鲜的 snapshot)──► FRESH
```

- 连续 RPC failure **不是独立交易状态**；最终交易状态由 **snapshot 有效性 + snapshot_age + transport_age** 决定。
- `max_consecutive_failures` 可作为**提前降级触发器**（→ STALE），但不得与 freshness 语义矛盾。

---

## 6. `replace_all` 语义（冻结，P0-DESIGN-004）

| 模式 | 语义 | 结果 |
|------|------|------|
| `replace_all=true` | 原子**全量替换** | `new_registry = 本次请求的合法条目全集`；旧 registry 中未出现的 token **删除**；invalid 条目 **reject 且不插入** |
| `replace_all=false` | 原子 **upsert** | `new_registry = old_registry + 合法 upserts`（invalid 不插入） |

两者均：**构造新 HashMap → 校验 → 一次性发布**；**禁止** `clear(); insert(A); insert(B); ...`。

---

## 7. Atomic Snapshot 设计（P0-DESIGN-004 / §5）

### 7.1 数据结构

```rust
struct SettlementSnapshot {
    times: HashMap<String, u64>,        // token_id -> 预计结算 unix 秒
    snapshot_taken_at: Option<SystemTime>,
}

struct SettlementRegistry {
    snapshot: RwLock<Arc<SettlementSnapshot>>,   // 不可变快照 + Arc 交换
    last_success_at: RwLock<Option<Instant>>,
    consecutive_failures: AtomicU64,
    // 配置：transport/snapshot freshness window、clock skew 容差（见 §13）
}
```

### 7.2 Writer

```text
build new HashMap (按 replace_all 语义)
  → validate 全部条目（token/时间/snapshot_taken_at/skew）
  → acquire write lock
  → replace Arc<SettlementSnapshot>
  → 同步刷新 TimeWindowGuard 数据（同一份 times）
  → update last_success_at / reset failure counter
  → release
```

### 7.3 Reader（关键）

```text
acquire read lock
  → clone Arc<SettlementSnapshot>
  → release read lock
  → 用该不可变快照完成“整次决策”
```

- **单次 `settlement_gate.check`（乃至整个 `place_order` 风控）必须使用同一快照版本**；不得「lookup A → 快照变化 → lookup B」。
- 若一次决策需多次查询，必须基于同一 `Arc`。

### 7.4 与 `TimeWindowGuard` 一致

- 发布快照时，将同一 `times` 刷入 `TimeWindowGuard`（`replace_settlements`，替换语义）。
- 该操作是**数据替换**，**不改变** `pre_check` 的窗口计算。
- 若审查方判定可接受 `Arc<SettlementRegistry>` 直接注入 `TimeWindowGuard`（免二次写入），可替代，但属更大改动，需另行批准。

---

## 8. RPC Response 语义（冻结）

```proto
message UpdateSettlementTimesRequest {
  repeated SettlementTimeEntry settlement_times = 1;
  bool replace_all = 2;
  string snapshot_taken_at = 3;   // RFC3339，必填；缺失→整请求 invalid
}
message UpdateSettlementTimesResponse {
  bool success = 1;               // 请求级：是否被正常处理
  int32 accepted = 2;             // 条目级
  int32 rejected = 3;             // 条目级
  string message = 4;
  string snapshot_taken_at = 5;   // 服务端当前快照时间（回显）
  bool registry_fresh = 6;        // 处理后 registry 是否 FRESH
}
```

- `success=true` = 请求已正常处理，**不代表**全部条目接受；`success=true, accepted=1, rejected=1` 合法。
- `success=false` = 请求级失败（如缺 `snapshot_taken_at`、内部错误），**registry 不变**（无部分变更）。
- `accepted + rejected = 请求条目数`。

---

## 9. 错误处理与 Python 侧（P1）

- **Python 客户端**（`strategy_engine/grpc_bridge/client.py`）新增 `update_settlement_times(entries, replace_all, snapshot_taken_at)`，并**显式解析** `success/accepted/rejected/registry_fresh`：
  - `success=true && rejected=0` → snapshot 接受；
  - `success=true && rejected>0` → **partial acceptance**，必须 `warn/error` 可观测，并参与 freshness/同步状态；
  - `success=false` → 视为推送失败（参与 §5 失败计数）。
- 推送失败/部分拒绝是否触发交易降级：见 §13 待批项，默认纳入 freshness 与降级判定。

---

## 10. Cleanup 生命周期与增长保护（P1）

### 10.1 生命周期（不推断实际结算）

```text
ACTIVE → SETTLEMENT_WINDOW → EXPECTED_SETTLEMENT_PASSED
                                   │（G1-001 的“实际结算”信号）
                                   ▼
                            ACTUALLY_SETTLED
                                   │ cleanup_grace
                                   ▼
                            CLEANUP_ELIGIBLE → delete
```

- **禁止** `now > expected_settlement_time ⇒ delete`。
- `ACTUALLY_SETTLED` 仅由 **G1-001 的实际结算事件**驱动；G1-002 不自行推断。

### 10.2 增长保护（G1-001 未完成时）

- **不得**为防增长而删除未确认 settlement 的 token。
- 措施：
  1. `max_registry_entries`（上限）：达到上限时**拒绝新增大批量**并告警，而非删除既有；
  2. **冷归档**：将 `EXPECTED_SETTLEMENT_PASSED` 且长期未确认的条目移入归档存储（保留可查，不参与窗口判定），活跃集合保持有界；
  3. 归档条目在 G1-001 确认结算后进入 CLEANUP_ELIGIBLE。
- 归档不得改变 fail-closed：未确认条目一旦回到活跃/命中查询，仍按 STALE/unknown 语义处理。

---

## 11. Allowed / Forbidden Scope（冻结）

**Allowed（建议，待批）**
- `proto/polymarket.proto`（新增 message/字段/RPC）
- `grpc/service.rs`（`settlement_gate` + handler + 回传组装）
- `bin/server.rs`（freshness 定时评估 / 归档接线）
- `strategy_engine/grpc_bridge/client.py`、`converter.py`、`market_discovery`（推送与快照时间）
- `fast/risk_engine.rs` —— **仅限**：
  - `TimeWindowGuard` 数据替换方法 `replace_settlements`（替换语义，**非窗口计算**）；
  - settlement registry 只读快照 getter（若需要）；
  - 与 Unknown fail-closed 集成直接相关的最小只读辅助。

**Forbidden**
- 修改 `TimeWindowGuard` 窗口阈值 / 窗口计算规则；
- 修改 Risk Model threshold / position limit / 集中度 / Gas / 熔断；
- 修改 strategy / trading semantics / funds-control；
- 修改 PnL / settlement accounting；
- 将 G1-002（预计结算时间）与 G1-001（实际结算/PnL）合并；
- `clear()+insert()` 式非原子发布。

---

## 12. Tests（含真实 production-path）

**Production-path（必须走 `place_order`，非仅 helper）**
1. `test_unknown_settlement_buy_rejected_on_order_path`：NEVER_SYNCED/未知 token → `place_order` 返回 Rejected，`error_message` 含 `unknown_settlement`。
2. `test_unknown_settlement_sell_allowed_on_order_path`：未知 token 的 Sell → 允许（进入窗口检查后放行）。
3. `test_stale_registry_rejects_known_token_buy`：STALE 下已知 token Buy → Rejected。
4. `test_registered_token_preserves_existing_window_behavior`：FRESH + 已注册 + 窗口外 → 正常放行；窗口内 Buy → 按既有 `TimeWindowGuard` 拒绝（**证明窗口逻辑未变**）。
5. `test_never_synced_rejects_buy`。
6. `test_fresh_registry_allows_known_token_when_outside_window`。

**RPC / 原子性**
7. `test_update_settlement_times_partial_reject`：A 合法 + B 非法 → `success=true, accepted=1, rejected=1`；A 存在、B 不存在。
8. `test_update_settlement_times_missing_taken_at_fails`：缺 `snapshot_taken_at` → `success=false`，registry 不变。
9. `test_settlement_snapshot_atomic_replace`：并发 reader×N + writer×M；reader ∈ {OLD, NEW}，**绝不出现 {} / 部分集 / 混合集**。
10. `test_single_decision_uses_single_snapshot`：一次 `settlement_gate.check` 期间发生替换，仍只用单版本。
11. `test_fresh_to_stale_transition_affects_order_path`：fresh→stale 后真实 `place_order` 立即按 STALE 拒绝 Buy。

**鉴别力**：短路 atomic swap（改 clear+insert）→ 测试 9 必 FAILED；短路未知门 → 测试 1/3/5 必 FAILED。

---

## 13. Human Acceptance（必须冻结，P1）

| 项目 | V3 推荐基线 | 状态 |
|------|-------------|------|
| Unknown Buy | Reject（fail-closed） | **待 Human 批准** |
| Unknown Sell | Allow | **待 Human 批准** |
| STALE Known Buy | Reject | **待 Human 批准** |
| STALE Sell | Allow | **待 Human 批准** |
| NEVER_SYNCED Buy | Reject | **待 Human 批准** |
| `transport_freshness_window` | 建议 = 2× 生产者推送间隔（如推送 10min → 20min） | **待 Human 指定** |
| `snapshot_freshness_window` | 建议 1h（不超过预计结算时间粒度的一半） | **待 Human 指定** |
| `max_consecutive_failures` | 建议 3（作为提前降级触发器） | **待 Human 指定** |
| `cleanup_grace` | 建议 24h | **待 Human 指定** |
| clock skew tolerance | 建议 5min | **待 Human 指定** |
| `max_registry_entries` | 建议 10000（超限拒绝新增+告警，不删除） | **待 Human 指定** |
| Python partial rejection 是否触发降级 | 建议纳入 freshness/降级 | **待 Human 指定** |
| STALE 是否全停 Buy（vs 仅已知 token） | 基线：全停 | **待 Human 批准** |

> 上述数值与语义**必须由 Human 冻结**后方可实施；实施者不得自行取值。

---

## 14. Rollback

- 功能开关：`SETTLEMENT_GATE_ENABLED`（默认可设 off/observe）。
- 关闭时：`settlement_gate` 直接放行（回退到当前行为），但**保留** registry 与观测，不影响 `TimeWindowGuard` 既有逻辑。
- proto 变更为纯新增，回滚不需改协议。

---

## 15. G1-001 / G1-002 边界（冻结）

| | G1-002（本文） | G1-001 |
|---|---|---|
| 数据 | **预计**结算时间 `end_date_utc` | **实际**结算事件 + outcome |
| 用途 | 临近结算禁开仓 | realized PnL → DailyLoss |
| 共享 | 仅 token 生命周期基础设施 | 同 |

**禁止**：`expected_settlement_time → 假设实际结算`；`registry cleanup → 假设 PnL settlement`；`settlement registry → 第二本 PnL 账`。

---

## 16. 结论 / 停止条件

V3 已解决 4 项 P0（gate 冲突 / STALE 语义 / 双 freshness / replace_all）与 2 项 P1（Acceptance 冻结 / 增长保护），并将 §15 要求的 15 项内容全部纳入。

**本阶段停止**：不改生产代码。提交物：
1. 本 `update_settlement_times_rpc.md`（Draft v3）；
2. 修改摘要（§0）；
3. Human Acceptance 表（§13）；
4. 预计修改文件列表（§11 Allowed）；
5. Production call chain（§2）；
6. Test plan（§12）。

等待独立审查通过 + Human 批准后方可拆分 `MASTER_TASKS` 实施。
