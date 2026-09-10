# BUG_TRACKER.md — 现有 BUG 清单

**数据源**：GPT 修复进度清单（2026-08-26，16 项）＋ 8/25 第一版 P0 清单（14 项）＋ `docs/CODE_REVIEW_STANDARD.md` ＋ `MASTER_TASKS.md`
**状态口径**：按六态状态机，GPT 的「✅已修 / 基本完成」仅等于 `FIXED`（代码已改、未验证），须补三重证据（git diff + 测试 + 调用链）才 `VERIFIED`。

---

## 一、总览

| 级别 | 数量 | GPT 状态分布 | 按标准的状态映射 |
|------|------|-------------|-----------------|
| P0 | 7 | 6 已修 + 1 基本完成 | **7 项全部 VERIFIED（09-09 闭环）** |
| P1 | 10 | 4 待修 + 5 待验证 + 1 遗留 | **10 项全部完成（09-10 闭环）** |
| **合计** | **17** | | |

**验证进展**：
- `VERIFIED`：旧 P0-07/P0-11（TradingGate）+ P0-003（RiskEngine 超仓）+ P0-004（Order Dedup 并发）+ P0-007（full_reconcile 启动）+ P0-001（FastExecutor 初始化，09-08）+ P0-005（StateSync L2 认证）+ P0-006（StateSync JSON Decode）+ **P0-002（Docker Secret，09-09）**
- **P0 全部 7 项 VERIFIED（09-09）**
- **P1-001~010 全部完成**：P1-001~009（09-07）+ P1-010 claim 占位失败清理（09-10）

---

## 二、P0 清单（7 项）

| 新编号 | 问题 | GPT 状态 | 标准映射 | 对应旧清单 | 需补证据 |
|--------|------|---------|---------|-----------|---------|
| P0-001 | FastExecutor 初始化 | ✅ 已修 | **VERIFIED** | P0-01（对应） | 调用链逐层验证 + 4 个离线 mock 测试全绿 |
| P0-002 | Docker Secret | ✅ 已修 | **VERIFIED** | P0-02 | `load_wallet_password()` 优先读 secret 文件回退 env + 4 个测试全绿 |
| P0-003 | RiskEngine pre-check | ✅ 已修 | **VERIFIED** | P0-03（对应） | 超仓拒单测试已有 + 集成测试补 3 个 |
| P0-004 | Order Dedup | ✅ 已修 | **VERIFIED** | P0-05（对应） | 并发穿透已修复（原子 claim）+ 并发测试转绿 |
| P0-005 | StateSync API Key | ✅ 已修 | **VERIFIED** | 对应 08-24/25 的 L2 认证 401 修复 | create_l2_headers→HMAC 调用链 + 官方测试向量通过 |
| P0-006 | StateSync JSON Decode | ✅ 已修 | **VERIFIED** | 对应 08-25 PaginatedResponse 修复 | PaginatedResponse 双格式解析 + 6 个测试全绿 |
| P0-007 | StateSync reconciliation | ✅ 基本完成 | **VERIFIED** | P0-09 + 旧 P0-10 | full_reconcile 启动测试已补（reconcile_complete） |

**✅ P0-007 代码 + 测试已闭环**（08-26）：
1. `full_reconcile()` 已接入启动流程（`server.rs:138-141`）；
2. 对账结果驱动 TradingGate：`in_sync` 才切 `Ready`，否则保持 `Reconciling`；
3. 决策逻辑抽为 `TradingGate::reconcile_complete()`，补 4 个单测 + 5 个集成测试全过。

**✅ P0-001 代码 + 测试已闭环**（09-08）：
1. 调用链逐层核对：`server.rs:110/112/139` → `FastExecutor::new()` → `init_api_key()`(executor.rs:292) → `create_or_derive_api_key()`(client.rs:205) → `set_api_creds()`(client.rs:87) → `prewarm_domains()`(signing_cache.rs:131) → `warmup()`(http_fast.rs:127) → `hot.prewarm()`(hot_wallet.rs:135) → `set_fast_executor()`(service.rs:309)，全部落点真实；
2. FAIL-CLOSED：`new()`/`init_api_key()`/`health_check()` 任一失败 → `halt` + `return Err`，进程在 `serve()` 前退出；下单入口 `place_order` 首行 `ensure_ready()` 兜底；
3. 补 4 个离线 mock 测试（`tests/p0_production_validation/fast_executor_init.rs`，wiremock）：400→derive 回退 / 500 传播 / 成功四步预热顺序 / 失败不预热。全绿。

**✅ P0-005 代码 + 测试已闭环**（09-09）：
1. 调用链：`state_sync.rs:337 create_l2_headers` → `auth.rs:62 build_poly_hmac_signature` → `hmac_sig.rs:21` HMAC-SHA256（secret 用 `URL_SAFE` 带 padding 解码，输出同样带 padding）；
2. 官方测试向量对齐：`test_hmac_matches_official_vector`（secret=`AAA…A=`，签名=`4gJVbox-R6XlDK4nlaicig0_ANVL1qdcahiL8CXfXLM=`）通过，`cargo test hmac` 2/2 绿。

**✅ P0-006 代码 + 测试已闭环**（09-09）：
1. 调用链：`state_sync.rs:307/358` 先按 `PaginatedResponse` 包装对象解析，失败兜底顶层数组，保留 body 前 200 字符便于诊断；
2. 6 个双格式解析测试（`state_sync.rs` tests 内 `test_paginated_*`）全绿，`cargo test state_sync` 15/15。

**✅ P0-002 代码 + 测试已闭环**（09-09，Human 授权后修复）：
1. 根因：`docker-compose.yml:81` 设 `WALLET_PASSWORD_FILE=/run/secrets/wallet_password`，但原 `server.rs:42` 只读 `WALLET_PASSWORD`，Docker 部署密码恒空 → fail-closed 起不来；
2. 修复：`server.rs` 新增 `load_wallet_password()` —— 优先读 `WALLET_PASSWORD_FILE` 文件并 trim，未配置/读失败回退 `WALLET_PASSWORD`，两者空返回空串（上层 `!password.is_empty()` 保持 fail-closed）；
3. 测试：抽纯函数 `resolve_password(file_content, env_password)`，补 4 个测试（文件优先/回退 env/空白文件回退/双空 fail-closed）全绿；
4. `MASTER_TASKS.md` P0-02 已把 `server.rs` 加入 Allowed Files。

---

## 三、P1 清单（10 项，全部完成 09-10）

| 新编号 | 问题 | 处置 | 落地 |
|--------|------|------|------|
| P1-001 | API Key Fail Gate | ✅ 已实现 | server.rs API Key 失败 → halt + return；TradingGate halt 优先（reconcile 不覆盖） |
| P1-002 | Network Health Check | ✅ 已实现 | ClobClient.health_check()（GET /time）+ server.rs 启动健康检查 FAIL-CLOSED |
| P1-003 | Retry Backoff | ✅ 已实现 | ClobClient.get_with_retry() 指数退避（0.5s→1s→2s，5xx/网络错误重试） |
| P1-004 | API Key 流程优化 | ✅ 已实现 | create_or_derive 仅 400「已存在」时回退派生，网络/认证错误直接返回 |
| P1-005 | Order Lifecycle | ✅ 已补 | OrderState 补 mark_cancelled/rejected/expired + 4 个生命周期测试 |
| P1-006 | Fill 同步 | ✅ 已实现 | ExecutionServiceImpl.on_market_trade()：异步成交 → RiskEngine.on_fill + FillLedger + OrderStore |
| P1-007 | State 恢复机制 | ✅ 已实现 | server.rs 后台周期对账（每 30s full_reconcile）+ 差异告警 |
| P1-008 | Settlement Observation | ✅ 已补 | market_events.mark_settled 回填盈亏测试（settle_by_token 链路） |
| P1-009 | Ensemble Calibration | ✅ 已补 | test_ensemble_fusion.py 5 个测试（Brier/权重/贝叶斯收敛/CI） |
| P1-010 | Order Dedup claim 占位失败清理（P0-004 遗留） | ✅ 已实现 | OrderStore.mark_rejected + claim_or_retry；place_order 6 失败路径清理占位；4 测试全绿 |

**测试结果（09-07）**：Rust 241 单元测试 + 9 集成测试全绿；Python 5 测试全过。

> 已知限制：P1-006 的 `on_market_trade` 已实现但 `MarketEventPoller` 尚未接入启动流程（需 L2 认证的 `/data/trades` 轮询驱动），异步成交的端到端触发留待 Shadow Trading 阶段验证。

---

## 四、与 8/25 旧清单对照（变化追踪）

| 类型 | 项 | 说明 |
|------|-----|------|
| ✅ 对应 | P0-001/002/003/004/005/006 | 与旧 P0-01/02/03/05 等一一对应 |
| 🔻 降级 | P0-06、P0-12、P0-13、P0-14 → P1-005/006/007 | post_check、pending orders、后台循环从 P0 降到 P1 |
| ➕ 新增 | P1-001/002/003/004/008/009 | 6 项旧清单未覆盖 |
| ✅ 已验证 | 旧 P0-07（TradingGate 状态机）、P0-11（TradingGate 进 READY） | 代码闭环 + 补 7 测试全过 → **VERIFIED** |
| ✅ 已确认 | 旧 P0-10（full_reconcile 接入） | 已接入启动流程（server.rs:138-141），见 P0-007 |
| ✅ 已确认 | 旧 P0-04（FastExecutor 真实执行路径） | 已实现于 FastExecutor 15 子模块（185 tests），place_order 全链路代码闭环；真实 CLOB 下单端到端留待实盘/Shadow Trading |

---

## 五、关键风险提示 🔴

1. ~~TradingGate 相关项疑似整体消失~~ → **✅ 已验证完成**：旧 P0-07（状态机）+ P0-11（进 READY）代码闭环，补 7 个测试全过（总计 229 tests），达 VERIFIED。GPT 新清单未列是因其已判定完成，非遗漏。
2. **P0 降级到 P1 理由已记录**：post_check 成交闭环、pending orders、后台对账循环从 P0 降到 P1，理由 = 逻辑已实现且有单测，但真实成交触发依赖 L2 `/data/trades` 轮询，sandbox 无外网无法端到端验证 → 推迟到 Shadow Trading 阶段。⚠️ 资金安全相关项降级仍待 Human 显式确认归档（记录见 `MASTER_TASKS.md`「P0 → P1 降级记录」）。
3. **「已修」≠「已验证」**：P0 全部 7 项（001~007）已 VERIFIED（09-09）；P1 9 项已完成（09-07）。

---

## 六、下一步

1. 向 GPT 确认两点：P0 降级 P1 的理由、P0-04（FastExecutor 真实执行路径）去向。（TradingGate 两项已确认 VERIFIED，无需再问。）
2. 编号体系已变（P0-01 → P0-001），`MASTER_TASKS.md` 需对齐重排（待确认后执行）。
3. 授权后启动 Round 4 验证：对 16 项逐项核 git diff + 测试 + 调用链，产出 `audits/round_04/` 报告。
4. **P1 项（P1-001~009）明日进行**；P0-004 遗留「claim 占位失败清理」记 P1 一并处理。
