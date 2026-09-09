# 代码审查标准与流程（Code Review Standard & Process）

**项目**：Polymarket 天气预测市场量化交易系统（Python 策略大脑 + Rust 执行手脚）
**制定者**：火眼眼（CodeReviewExpert）
**版本**：1.0
**日期**：2026-08-25
**上游依据**：`E:\AI_CODE_REVIEW_PROTOCOL.md` v1.0（多 AI 协作审计与开发协议）

---

## 0. 文档定位

协议 v1.0 回答的是 **"谁审、谁做、什么红线"**（角色分工与授权边界）。
本文档回答的是 **"怎么审、审什么、什么才算过"**（代码级标准与流程）。

两者关系：

```text
AI_CODE_REVIEW_PROTOCOL.md v1.0   —— 宏观框架（角色 / 红线 / Release Gate）
        │
        └──> 本文档 —— 代码级落地细则（维度 / 判据 / 流程 / 验收门禁）
```

**本文档不改变协议 v1.0 的任何角色与红线**，只把它变成可执行、可验证的操作标准。

---

## 1. 审查目标与五维标准

一次合格的代码审查，必须覆盖五个维度。每个维度在本项目有专属检查点（非通用模板）：

### 1.1 Correctness（正确性）

> 代码是否做了它应该做的事，且在边界条件下不崩溃。

| # | 检查点 | 本项目典型反例 |
|---|--------|---------------|
| C1 | 交易语义是否正确 | BUY_NO 被实现成 SELL_YES（YES/NO 是独立 outcome token） |
| C2 | 状态机迁移是否完整 | TradingGate 卡在 Reconciling，永远进不了 Ready |
| C3 | 幂等性是否成立 | 同一 order_id 并发提交导致重复下单 |
| C4 | 数值精度/单位一致 | quantity 与 notional 混用（P0-08 正是此问题） |
| C5 | 边界条件 | 空订单簿、0 持仓、非法 body、超时、重启恢复 |

### 1.2 Security（安全性）

> 是否存在可被攻击或误用导致资金损失、私钥泄露的路径。

| # | 检查点 | 本项目典型反例 |
|---|--------|---------------|
| S1 | 私钥/密码的加载与存储 | keystore.json 被提交进 git；密码硬编码 |
| S2 | Secret 与配置一致性 | Docker 读 `WALLET_PASSWORD_FILE`、Rust 读 `WALLET_PASSWORD`（P0-02） |
| S3 | 签名/HMAC 正确性 | base64 变体用错（URL_SAFE_NO_PAD）导致 401 |
| S4 | 能否绕过风控门 | 绕过 TradingGate、绕过 pre_check 直接下单 |
| S5 | 输入校验 | gRPC 入参未校验导致越权/越界 |

### 1.3 Maintainability（可维护性）

> 半年后另一个人（或另一个 AI）能否看懂并安全修改。

| # | 检查点 |
|---|--------|
| M1 | 命名与结构清晰，模块职责单一 |
| M2 | 关键决策有注释/文档，尤其"为什么这么做" |
| M3 | 无重复代码（可提取而未提取的公共逻辑） |
| M4 | 错误信息带上下文（原始 body 片段、参数值） |
| M5 | 与 `architecture/` 真源一致，无 Architecture Drift |

### 1.4 Performance（性能）

> 是否存在明显的性能瓶颈（本项目延迟预算 100ms，99% 在网络层）。

| # | 检查点 |
|---|--------|
| P1 | 热路径无阻塞调用（冷解锁、网络 I/O 不得进签名热路径） |
| P2 | 无 N+1 查询、无不必要的重复分配/序列化 |
| P3 | 预签名/缓存机制正确命中（signing_cache、hot_wallet） |
| P4 | 后台任务频率合理，不拖垮主循环 |

### 1.5 Testing（测试）

> 重要路径是否有测试覆盖，测试是否验证了"行为"而非"实现细节"。

| # | 检查点 |
|---|--------|
| T1 | 每个修复都带对应测试（"修复必须可证伪"） |
| T2 | 测试覆盖边界条件，不止 happy path |
| T3 | 测试断言行为，不断言内部实现 |
| T4 | 关键路径有集成/回归测试（启动对账、风控门、成交回填） |

---

## 2. Severity 分级标准（代码级判据）

协议 v1.0 §11 定义了 P0/P1/P2 的宏观含义，这里给出**可直接套用的判据**。

### P0 — Critical（实盘前必须解决，任何 P0 未闭环 = LIVE NO-GO）

**命中任一即 P0：**

- 可能导致**资金损失**：重复下单、错误 token 路由、错误仓位、错误结算目标
- 可能导致**私钥/Secret 泄露**
- 可能导致**风控失效**：TradingGate 可绕过、pre_check 可跳过
- **对账失效**：无法发现本地与链上仓位不一致
- **Lookahead Bias / 数据穿越**（策略层）
- **模块存在但未接入生产执行路径**（存在 ≠ 生效）

### P1 — High（小资金实盘前必须解决）

- 直接影响策略正确性、回测可信度、执行质量、生产稳定性
- 错误处理缺失但不会立即导致资金损失
- 缺少关键路径测试

### P2 — Medium（可后续迭代）

- 技术债：可维护性、性能优化、代码整洁、命名

### 判据冲突时的裁决原则

涉及 **资金安全 / 安全 / 并发 / 结算** 的 Finding，**采用最保守、可证实的结论**，禁止简单多数投票（协议 §13）。

---

## 3. 修复状态定义（本标准的灵魂）

GPT 的 P0 清单暴露了一个系统性问题：**"✅ 已修复" ≠ 真的修好了**。P0-06 `post_check()` 标"函数已存在，⚠️需验证真实路径调用"，P0-13 标"⏳待验证"——这说明"加了函数/加了调用"被当成了"修复完成"。

本系统强制采用**六态修复状态机**，任何修复必须走完才能算 CLOSED：

```text
TODO ──> IN_PROGRESS ──> FIXED ──> VERIFIED ──> CLOSED
                            │
                            └──> REJECTED（代码审查不通过，打回）
```

| 状态 | 含义 | 进入条件 |
|------|------|---------|
| TODO | 已识别，未动工 | Finding 已确认 |
| IN_PROGRESS | 实施中 | MASTER_TASKS 已授权，代码已改 |
| FIXED | 代码已改，**未验证** | 代码 diff 存在 |
| VERIFIED | 三重证据齐备 | 见下方 |
| CLOSED | 已合入并通过 Human Review | Release Gate 通过 |
| REJECTED | 审查不通过，打回 IN_PROGRESS | 任一证据缺失或测试失败 |

### "VERIFIED" 的三重证据（缺一不可）

一项修复只有在**同时满足**以下三条时，才允许从 FIXED → VERIFIED：

1. **代码证据**：`git diff` 显示改动真实存在，且改动位置正确（不是"加了函数但没人调用"）。
2. **测试证据**：有针对该修复的测试，且**测试通过**（Rust `cargo test` / Python `pytest`）。
3. **路径证据**：调用链完整可追踪。例：`FastExecutor Fill → OrderStore → RiskEngine.post_check()` 必须逐环有据，不能停在"函数存在"。

**红线**：任何 AI（包括实施者）**不得自行宣告 VERIFIED**。VERIFIED 由**独立的审查方**（审查 AI 或 Human）基于三重证据确认后写入 CONSENSUS。

---

## 4. Finding 标准格式

沿用协议 §10，补两个字段（`Status`、`Verified By`）：

```markdown
# Finding

ID:            P0-EXEC-001
Severity:      P0
Category:      Execution | Security | Risk | Reconciliation | Strategy | Quant | DevOps
File:          polymarket-rust/polymarket-rust/src/grpc/service.rs
Line:          300-350
Finding:       这里发生了什么（一句话）。
Evidence:      源码证据（引用代码片段）。
Risk:          可能造成什么后果。
Proposed Fix:  建议如何修复。
Required Test: 必须新增什么测试（测试名 + 断言内容）。
Confidence:    HIGH | MEDIUM | LOW
Status:        TODO | IN_PROGRESS | FIXED | VERIFIED | CLOSED | REJECTED
Verified By:   （VERIFIED 时才填，审查方 + 日期）
```

---

## 5. Round 审查流程

每个审查轮次（Round）严格执行以下顺序，禁止跳步：

```text
[1] 冻结审查输入
        │  固定 commit / 代码快照 + 固定文档版本
        ▼
[2] 独立审计（GPT / Gemini / DeepSeek / KIMI 各自独立读同一版本）
        │  产出各自的 audit_*.md，互不参考
        ▼
[3] Consensus / Judge
        │  逐条 Finding 判定 CONFIRMED / DISPUTED / DEFERRED / REJECTED
        │  定最终 Severity（冲突按 §2 裁决原则）
        ▼
[4] 生成 MASTER_TASKS.md
        │  仅 CONFIRMED 项进入；每项含 Goal / Allowed / Forbidden / Required Tests / Acceptance
        ▼
[5] Work Buddy 实施（唯一实施者，一次只做一个 Task Group）
        │
        ▼
[6] 强制验收（见 §6）
        │  cargo test / fmt / clippy / pytest / Docker
        ▼
[7] 独立验证（三重证据 → VERIFIED，写入 CONSENSUS）
        │
        ▼
[8] Human Review（关键代码人工审查）
        │
        ▼
[9] 下一轮 Round（或 Release Gate）
```

**关键规则**：
- 步骤 [2] 的四家 AI **独立并行**，避免过早互相影响（协议 §9）。
- 步骤 [6] 的验收由实施者跑，但步骤 [7] 的 VERIFIED 判定**必须由非实施者完成**。
- 任何一轮出现 P0 未闭环，**不进入 Release Gate**。

---

## 6. 强制验收门禁（Gate）

### 6.1 Rust（硬性，全部通过才算数）

```bash
cargo test --all
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

> 当前已知：clippy 有 51 个**历史遗留** lint 债（non_snake_case、unused import、map_or 等）。规则：
> - 本次改动**不得新增**任何 lint 问题；
> - 历史 lint 债单独建档跟踪，逐轮消化，不阻塞 P0 修复，但**不得用"反正有历史债"为由新增 lint**。

### 6.2 Python（硬性）

```bash
python -m pytest -q    # 0 errors, 0 failures
```

同时核对 protobuf / grpcio / grpcio-tools 三版本兼容（协议 §18）。

### 6.3 Docker（硬性）

验证：secret loading、container startup、health check、network isolation、gRPC exposure、graceful shutdown、restart recovery。

**注意**（协议 §19）：Docker 启动成功 ≠ 交易系统 READY，必须继续通过 Startup Reconciliation + Trading Gate + Risk Sync。

### 6.4 交易安全门（Human 把关，AI 不得放行）

以下任一项，**必须 Human 人工确认**才能进入下一阶段：
- 真实资金上线、资金规模增加
- 关闭 Risk Gate / Reconciliation
- 绕过 Startup Reconciliation

---

## 7. 目录与文件规范

协议 v1.0 定义了以下基础设施，当前项目尚未落地，本流程要求补齐：

```text
项目根目录/
├── audits/
│   ├── round_01/  audit_gpt_round_01.md  audit_gemini_...  audit_quant_...  audit_security_...  CONSENSUS_ROUND_01.md
│   ├── round_02/  ...
│   └── round_N/   ...
├── MASTER_TASKS.md       # 唯一工程修改任务来源
├── DECISIONS.md          # 正式决策日志（已决策问题不得重复讨论）
├── architecture/         # 架构真源
│   ├── SYSTEM_ARCHITECTURE.md
│   ├── TRADING_STATE_MACHINE.md
│   ├── RISK_MODEL.md
│   ├── SETTLEMENT_MODEL.md
│   ├── PROBABILITY_MODEL.md
│   └── EXECUTION_MODEL.md
└── docs/                 # 本文件所在目录
```

**命名约定**：
- Finding ID：`{Severity}-{Category缩写}-{三位序号}`，如 `P0-EXEC-001`
- 分支：`fix/p0-a-trading-gate`、`fix/p0-b-fast-executor`（协议 §15）
- Commit：`fix(execution): add atomic order claim` 等语义化前缀

---

## 8. 模板

### 8.1 MASTER_TASKS.md 条目模板

```markdown
## P0-A Trading Gate READY 切换

Status:         TODO
Owner:          Work Buddy
Allowed Files:  - polymarket-rust/polymarket-rust/src/bin/server.rs
                - polymarket-rust/polymarket-rust/src/fast/trading_gate.rs
Forbidden:      - strategy_engine/
                - polymarket-rust/polymarket-rust/src/fast/risk_engine.rs
Required Tests: - startup_reconciliation_failure_halts
                - trading_gate_blocks_submit_when_not_ready
Acceptance:     只有 READY 状态才能提交真实订单；StateSync 失败时闸门保持关闭。
```

### 8.2 CONSENSUS 条目模板

```markdown
| Finding | GPT | Gemini | DeepSeek | KIMI | Final Severity | Decision | Reason | Required Action |
|---|---|---|---|---|---|---|---|---|
| P0-EXEC-001 | P0 | P0 | P1 | P0 | P0 | CONFIRMED | 资金安全+并发 | 进入 MASTER_TASKS |
```

Decision 仅允许：`CONFIRMED / DISPUTED / DEFERRED / REJECTED`。

### 8.3 Decision Log 条目模板

```markdown
# DEC-0042
Decision: BUY_NO 使用 no_token_id + Side::Buy
Reason:   YES 与 NO 是独立 outcome token
Rejected: BUY_NO = SELL_YES
Approved: Human + GPT + Gemini
```

---

## 9. 针对 GPT P0 清单的首轮审查动作

GPT 的《P0 Bug 修复状态总表》共 14 项，其中 9 项标"✅ 已修复"、5 项标"⏳ 待修复/待验证"。**按本标准，9 项"已修复"最多算 FIXED，一律不能算 VERIFIED**，需要逐项走三重证据验证。

### 9.1 待 VERIFIED 的 9 项（原标"已修复"）

| ID | 原状态 | 本标准判定 | 需补的证据 |
|----|--------|-----------|-----------|
| P0-01 FastExecutor API Key/Warmup | ✅ | FIXED | init_api_key 调用链 + 启动日志 + 测试 |
| P0-03 RiskEngine pre_check 接入 | ✅ | FIXED | 下单入口强制调用 + 超仓被拒测试 |
| P0-04 FastExecutor 真实执行路径 | ✅ | FIXED | gRPC→ExecutionService→FastExecutor 链路测试 |
| P0-05 幂等保护 | ✅ | FIXED | 并发重复下单被去重测试 |
| P0-06 post_check 基础实现 | ✅（⚠️） | FIXED | **真实成交路径调用验证**（当前缺失） |
| P0-07 TradingGate 状态机 | ✅ | FIXED | 状态迁移测试 |
| P0-08 StateSync 仓位接口 | ✅ | FIXED | /data/positions 解析测试（08-25 已加） |
| P0-09 Position 对账逻辑 | ✅ | FIXED | reconcile 差异检测测试 |
| P0-10~14 | ⏳ | TODO | 见下 |

### 9.2 第一优先级（上线前必须，对应 GPT 清单 §五）

1. **P0-10** StateSync `full_reconcile()` 接入启动流程
2. **P0-11** TradingGate 进入 READY（`ensure_ready()` 放行）
3. **P0-13** 成交回报连接 `RiskEngine.post_check()`（fill→position→risk 闭环）

### 9.3 第二优先级

4. **P0-12** Pending Orders 同步（OrderStore → StateSync）
5. **P0-14** StateSync 后台循环（30s 周期）
6. **P0-02** Docker Secret 最终确认（`WALLET_PASSWORD_FILE` vs `WALLET_PASSWORD`）

---

## 10. 核心原则（一句话总结）

```text
多 AI 审计的价值 = 发现不同专业盲区，而非同时改代码
单一实施者 = 修改路径稳定
三重证据 = 声称"修好了"必须可证实、可追溯、可独立验证
Human Review = 金融关键风险不得自动放行
任何 P0 未闭环 = LIVE NO-GO
```

---

*本文档是流程标准，不替代协议 v1.0；若两者冲突，以协议 v1.0 的角色与红线为准，本文件按需修订。*
