# DECISIONS.md

**正式决策日志。** 已正式决策的问题，任何 AI 不得重复讨论，除非出现新证据（协议 §21）。

---

## 一、已批准决策（Approved）

### DEC-0001 — BUY_NO 的 Token 路由

- **Decision**: `BUY_NO` 使用 `no_token_id` + `Side::Buy`。
- **Reason**: YES 与 NO 是独立的 outcome token，各按自有 token_id 下单。
- **Rejected**: `BUY_NO = SELL_YES`。
- **Approved**: Human + GPT + Gemini

### DEC-0002 — gRPC 方向映射

- **Decision**: `BUY_YES → Side::Buy`，`BUY_NO → Side::Sell`。
- **Reason**: Rust 执行层 `Side` 枚举与 CLOB 下单语义对齐。
- **Approved**: 项目技术约定（执行层实现）

### DEC-0003 — 链与资产

- **Decision**: `chain_id = 137`（Polygon PoS），结算资产为 USDC。
- **Reason**: Polymarket 仅接受 Polygon PoS 主网 USDC，资金存 Profile（Gnosis Safe），签名由 EOA 完成。
- **Approved**: 平台约束

### DEC-0004 — Polymarket base64 编码变体

- **Decision**: 一律使用 **URL-safe base64 且带 padding**（`URL_SAFE`，非 `URL_SAFE_NO_PAD`）。
- **Reason**: 官方 rs-clob-client 的 L1/L2 签名密钥解码、签名输出编码均遵循此约定；用错变体导致 401。
- **Approved**: 官方 SDK 对齐 + 官方测试向量验证

### DEC-0005 — L1 EIP-712 domain 结构

- **Decision**: domain 类型串使用带 `address verifyingContract` 的 **4 字段** 结构。
- **Reason**: 与 alloy/官方 SDK 的 domain separator 编码严格一致，3 字段版本导致 400。
- **Approved**: 官方 SDK 对齐

### DEC-0006 — Kelly 仓位系数

- **Decision**: `b = (1-price)/price`（BUY_YES），`kelly_fraction = 0.25`。
- **Reason**: 防方差爆炸，保留安全边际。
- **Approved**: 策略层约定

---

## 二、技术约定（已生效，待正式批准归档）

以下为项目现行技术约定，已在代码中落地，建议经 Human 确认后升格为正式决策：

| ID | 约定 | 说明 |
|----|------|------|
| C-001 | 滑点定义 | `|成交价 - 下单价| / 下单价`（始终 ≥0）；手续费 `fee_rate_bps` 默认 0 |
| C-002 | Welford 在线算法 | 滚动 μ/σ，O(1) 内存 |
| C-003 | IQR 离群点检测 | 仅用于温度；降水是零膨胀分布，不适用 IQR |
| C-004 | 临近修正时间权重 | `1 - hours_to_deadline/window`，距截止 <0.5h 置信度衰减 |
| C-005 | 临近修正信号有效期 | 30 分钟（vs 预报偏差策略 5 分钟） |
| C-006 | 历史统计混合权重 | `max(0.15, min(0.85, 1 - lead_hours/200))`，信号有效期 1 小时 |
| C-007 | 风控两阶段 | 预检（熔断/结算/亏损）→ 仓位 → 后检（仓位/暴露） |
| C-008 | 仓位硬限制 | 时间窗口 2h + 3 级熔断 + UMA 争议 + Gas 保护 |
| C-009 | Python pb2 包内导入 | 重新生成后 `import polymarket_pb2` 改为 `from . import polymarket_pb2` |

---

## 决策编号规则

`DEC-XXXX` 为正式批准决策；`C-XXX` 为技术约定（候选）。新决策按顺序递增编号，记录 Decision / Reason / Rejected / Approved 四要素。
