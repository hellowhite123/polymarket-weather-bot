# REVIEW_CHECKLIST_ROUND_04.md

> 本文件供**独立审查 AI / Human** 使用，用于对 OpenCode（实施者）Round 04 产出做独立复核。
> 依据：`AGENTS.md`、`docs/CODE_REVIEW_STANDARD.md`（三重证据 + 六态状态机 + §4 Finding 格式）。
> **角色约束**：OpenCode 是本项目工程实施者，**不得自证 VERIFIED**；本清单只是待复核输入。

---

## 0. 审查输入快照（固定）

| 项 | 值 |
|----|----|
| 分支 | `p0-blockers-2026-09-13` |
| 代码快照 commit | `21fdcf8`（P1-035 补测试后） |
| 审查报告 | `audits/round_04/audit_opencode_round_04.md` |
| 候选任务 | `MASTER_TASKS.md` →「候选任务（Round 04 审查产出）」 |
| Rust 工具链 | `PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe`；`--target x86_64-pc-windows-msvc` |
| 项目根 | `C:\Users\32389\WorkBuddy\AI Weather`（junction）→ 实体 `D:\WorkBuddyData\AI Weather` |

**你的产出**：`audits/round_04/CONSENSUS_ROUND_04.md`（见 §4 模板）。

---

## 1. Part A — P1-035 修复（commit `21fdcf8`）判 VERIFIED

修复内容：三条 fill 路径对空/空白 `trade_id` fail-closed。

### A1 代码证据（git diff）

```bash
git show 21fdcf8 --stat
git show 21fdcf8
```

需确认三处守卫**真实存在且位置正确**（不是「加了函数没人调用」）：

| 入口 | 文件 | 关键行 | 期望语义 |
|------|------|--------|----------|
| 实时轮询 | `polymarket-rust/polymarket-rust/src/fast/market_events.rs` | ~285-291 | `poll_trades` 丢弃空/空白 `t.id` |
| 对账恢复计划 | `.../src/fast/state_sync.rs` | ~922-928 | `build_recovery_plan` 不生成空 id 的 `MarkOrderFilled` |
| 恢复应用 | `.../src/grpc/service.rs` | ~421-427 | `apply_recovery` 拒绝空 id 进 `on_market_trade` |

### A2 测试证据

```powershell
$env:PROTOC="C:/Users/32389/tools/protoc/bin/protoc.exe"
cargo test --target x86_64-pc-windows-msvc empty_trade_id
```

期望 3 passed：

- `fast::state_sync::tests::test_build_recovery_plan_skips_empty_trade_id`
- `grpc::service::tests::test_apply_recovery_rejects_empty_trade_id`
- `fast::market_events::tests::test_poll_trades_drops_empty_trade_id`

**鉴别力验证（实施者已做，请独立复现）**：把三处守卫条件临时置 `false`，上述 3 测试必须 FAILED；还原后转绿。
（复核建议：`git stash` 前先确认工作区干净，或仅在本地临时改动后 `git checkout`。）

全量回归：`cargo test --target x86_64-pc-windows-msvc` 期望 `317 + 4 + 18 + 1 doc` 全绿。

### A3 路径证据（调用链）

- `poll_trades`（实时）→ `MarketEvent::Trade` → `consume_trade_events`（`grpc/service.rs`）→ `on_market_trade`
- `build_recovery_plan` → `RecoveryPlan.actions` → `apply_recovery`（`service.rs:410-473`）→ `on_market_trade`
- 三条路径均须在到达 `RiskEngine::on_fill`（唯一入账点）**之前**完成拦截。

**待判**：`FIXED → VERIFIED` 或 `REJECTED`（并注明缺失证据）。

---

## 2. Part B — Round 04 Findings 复核（判 CONFIRMED / 定级）

对每条：核对代码位置与断言是否成立 → `CONFIRMED / DISPUTED / DEFERRED / REJECTED`，并按 `CODE_REVIEW_STANDARD.md` §2 裁定最终 Severity。

### P0（必须逐条复核）

| ID | 文件:行 | 需独立验证的断言 | 备注 |
|----|---------|------------------|------|
| P0-RISK-001 | `src/fast/risk_engine.rs:1049,1063` | `RiskEngine::post_check` 在生产代码零调用 → 日亏熔断永不触发；`report_pnl` 仅此可达 | 用 `rg post_check` 验证调用点 |
| P0-RISK-002 | `src/fast/risk_engine.rs:443-483,1115` | `set_settlement(s)` 生产零调用；未注册结算时间直接 `Ok(())`（fail-open） | 需确认「未注册」的期望处置 |
| P0-RISK-003 | `src/grpc/service.rs:718` | `check_and_reserve(..., 0)` gas 恒 0 → `risk_engine.rs:752` 上限检查失效 | 直接读该行 |
| P0-EXEC-004 | `src/fast/market_events.rs:268-277,438`；`src/bin/server.rs:303-335` | ① `poll_trades` 无 L2 头；② `token_ids` 恒空；③ `tick` 用 `if let Ok` 吞错 | **含外部事实**，见 §3 |
| P0-EXEC-005 | `src/grpc/service.rs:347-377`；`src/fast/market_events.rs:345-358` | `on_market_trade` 只用 `trade.side`，从不读 `trader_side` → Maker 成交方向可能反 | **含外部事实**，见 §3 |

### P1（建议逐条复核）

| ID | 文件:行 | 需独立验证的断言 |
|----|---------|------------------|
| P1-RISK-006 | `src/grpc/service.rs:915-960`；`risk_engine.rs:311` | 取消订单不调 `release_reserve`、不更新 OrderStore |
| P1-RISK-007 | `src/bin/server.rs:247-259`；`risk_engine.rs:372` | 后台对账 `sync_position` 清 reserved 但不 `rebuild_open_order_reserves` |
| P1-RISK-008 | `src/fast/risk_engine.rs:332-338` | `on_fill` 按 filled notional 冲抵 limit×qty 预留 → 价格改善残留 |
| P1-EXEC-009 | `src/grpc/service.rs:841-857`；`order_state.rs:362-366` | 未知结果停在 `Received`，被对账输入集排除 |
| P1-CONSIST-010 | `src/grpc/service.rs:398-399` vs `793-812`；`order_state.rs:112-136` | 响应绝对覆盖 + 事件增量双路写 OrderStore |
| P1-CONSIST-011 | `src/fast/state_sync.rs:116-123,437-527` | `PaginatedResponse` 无 `next_cursor`、从不翻页，首屏被当全集 |
| P1-DATA-012 | `src/fast/state_sync.rs:846-853` | `parse_json_f64` 非法/缺失静默 `0.0`（实时路径 `parse_f64` 要求 Err） |
| P1-SEC-013 | `src/clob/signing/hmac_sig.rs:35-37` | secret 解码失败回退原始 ASCII 字节（掩盖认证根因） |
| P1-EXEC-014 | `src/fast/mempool.rs:519,549,626-632` | 全局容量用 `orders.len()`；终态条目不被 `sweep` 清除（**生产未调 `confirm` 故潜伏**） |

> P2 明细（12 项）见审查报告第四、五节；建议批量 `DEFERRED`，仅对「接入生产前必须修」项（executor 双路记账、nonce 状态机）保留关注。

---

## 3. 需外部事实核验（非代码可证，请独立查官方文档/实测）

1. **`/data/trades` 是否强制 L2 认证** → 决定 `P0-EXEC-004` 是否 P0。
2. **Polymarket trade 的 `side` 视角**（taker 视角 vs 本方视角）→ 决定 `P0-EXEC-005` 是 P0 还是防御性 P1。
3. （可选）CLOB `/data/trades` 过滤参数是 `maker_address`/`taker`/`maker` 还是 `user`（`state_sync.rs:485`）。

---

## 4. Part C — 候选任务授权

`MASTER_TASKS.md` 候选：`G1`（风控三闸）、`G2`（成交链）、`G3`（Maker 方向）、`ENV-001`（pytest 门禁）。

- 对 `G1/G2/G3`：CONSENSUS 判 `CONFIRMED` 后方可将 `Status` 由「候选」改 `TODO` 并授权实施。
- `G3` 须先满足 §3 第 2 条。
- `ENV-001`：非资金风险，可独立验证（`pip install -r requirements-dev.txt` 后 `python -m pytest -q` 应 0 failed / 0 error）。

---

## 5. CONSENSUS_ROUND_04.md 建议模板

```markdown
| Finding | OpenCode | 独立审查方 | Final Severity | Decision | Reason | Required Action |
|---|---|---|---|---|---|---|
| P0-RISK-001 | P0 | ? | ? | CONFIRMED/DISPUTED/DEFERRED/REJECTED | ? | 进入 MASTER_TASKS G1 |
| P0-RISK-002 | P0 | ? | ? | | | |
| P0-RISK-003 | P0 | ? | ? | | | |
| P0-EXEC-004 | P0 | ? | ? | | | |
| P0-EXEC-005 | P0 | ? | ? | | | 需外部事实 |
| P1-... | P1 | ? | ? | | | |
| P1-035（21fdcf8） | FIXED | ? | — | VERIFIED/REJECTED | 三重证据是否齐备 | 更新 BUG_TRACKER |
```

> 任何 P0 未 `CONFIRMED` 且闭环前 = **LIVE NO-GO**（`CODE_REVIEW_STANDARD.md` §2）。
