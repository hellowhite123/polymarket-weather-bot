# AGENTS.md — Polymarket 天气预测市场量化交易系统

> 本文件由 OpenCode 始终自动加载，用于约束 AI 工程实施者的行为。WorkBuddy 的
> `.workbuddy/` 项目记忆与技能保留并存，本文件是其跨工具的行为契约摘要。

## 项目定位
专注「现实世界气象数据 → 链上预测市场定价偏差」的量化交易系统，非通用加密货币交易机器人。
- **Python 策略大脑**：asyncio + aiohttp + pandas（策略引擎在 `strategy_engine/`）
- **Rust 执行手脚**：tokio + ethers-rs + tonic（执行层在 `polymarket-rust/`）
- **双层通信**：Python(gRPC client) ↔ Rust(gRPC server)，支持 UDS / TCP

## 协作协议与角色（最高优先级）
- **伯森先生 = Human 监督者（总闸）**：资金开关、关键代码人工审查、Release 批准。
- **工程实施者（AI） = 唯一工程实施者**：按 `MASTER_TASKS.md` 修 bug + 跑测试验收；
  **不擅自**改交易语义 / Risk Model / Strategy / 资金控制逻辑 / 大规模重构。
- 工作流：Bug → 进 `MASTER_TASKS.md`（Task ID / Goal / Allowed / Forbidden / Required Tests / Acceptance）
  → 修 → 验收（Rust `cargo test` / Python `pytest`）→ `git diff` → Human Review。
- 状态机：`TODO → IN_PROGRESS → FIXED → VERIFIED → CLOSED`，`VERIFIED` 由**非实施者**判定。

## 红线（无 MASTER_TASKS.md 明确授权不得做）
- 改交易语义 / 改 Risk Model / 改 Strategy / 加重大架构 / 大规模重构 / 改资金控制逻辑。
- 任何不可逆或资金相关操作前必须显式授权（等你授权）。

## 验证门禁（每次改动后必跑）
- **Rust 编译**：`cargo check --target x86_64-pc-windows-msvc`
  （需 `PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe`；protoc v25.1）
- **Rust 测试**：`cargo test --target x86_64-pc-windows-msvc`（历史基线 337 passed / 0 failed）
- **Python venv**：`C:\Users\32389\.workbuddy\binaries\python\envs\default\Scripts\python.exe`
- **Python 依赖**：`python -m pip install -r requirements-dev.txt`（含 pytest / pytest-asyncio）
- **Python 测试**：`python -m pytest -q`（基线 103 passed / 0 failed / 0 error）
  - `test_grpc.py` 的 async 用例已纳入 pytest（TST-001：改用 session 级 event loop 解决 grpc.aio 挂起）；脚本 runner `python test_grpc.py` 亦全绿。
- 验收：Rust 编译 0 error；Python `pytest` 全绿；改动前先 `git status` 确认无在途未提交工作。

## 关键工程约定
- **FAIL-CLOSED 哲学**：身份/数据异常时宁可漏记或拒绝，不可重复记账或错误放行。
  （例：空 `trade_id` 不得进入 `on_market_trade`，由 `poll_trades` / StateSync / `apply_recovery` 三处拦截）
- **成交幂等**：同一 `trade_id` 仅入账一次，由 `FillLedger::claim_trade` 单点负责；
  `on_market_trade` 是唯一调用 `RiskEngine::on_fill` 的路径。
- **风控两阶段**：预检（熔断 / 结算 / 亏损）→ 仓位 → 后检（仓位 / 暴露）。
- 滑点：`|成交价 - 下单价| / 下单价`（始终 ≥0）；手续费 `fee_rate_bps` 默认 0。
- Welford 在线算法滚动 μ/σ（O(1) 内存）；IQR 离群点检测仅用于温度（降水零膨胀，不适用）。

## Polymarket CLOB 认证（切勿用错）
- **L1 EIP-712 domain**：4 字段结构，含 `address verifyingContract`（verifyingContract 编码为 32 字节 0）。
- **L2 HMAC-SHA256**：消息 = `{timestamp}{METHOD}{path}{body}`；secret 与签名均用
  **URL-safe base64 带 padding**（`URL_SAFE`，非 `URL_SAFE_NO_PAD`），否则 401。
- **链与资产**：`chain_id = 137`（Polygon PoS 主网），结算资产 USDC；钱包文件 `**/keystore.json` 严禁提交。

## 文件结构权威（改动前先读对应真源）
- `architecture/`：架构真源（6 份 .md）
- `MASTER_TASKS.md`：唯一工程修改任务来源（代码改动授权依据）
- `BUG_TRACKER.md`：bug 权威登记（P0/P1 状态机）
- `DECISIONS.md`：正式决策日志（`DEC-XXXX` 已批准，`C-XXX` 技术约定）
- `audits/`：多 AI 审计存档（round_X/ + DECISIONS + architecture/）
- `E:\AI_CODE_REVIEW_PROTOCOL.md`：多 AI 协作审计/开发协议 v1.0

## 安全规则
- 绝不提交：`**/keystore.json`、`.env`、`wallets/`、`*.key`（`.gitignore` 已覆盖）。
- 真实 API key / 钱包密码只走环境变量或 keystore，绝不写进代码或本文件。
- `.workbuddy/` 是项目记忆与技能目录（已被 gitignore），**不要删除**。
- 本 AGENTS.md 可提交；若含任何真实密钥则立即移除。
