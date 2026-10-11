# TRADING_STATE_MACHINE.md

**架构真源** · 版本 v0.1 · 2026-08-25 · 待随审计完善

## 1. 启动状态机

```text
STARTING
   │
   v
INITIALIZING
   │
   v
Wallet Loaded          （私钥解锁 / hot_wallet 预热）
   │
   v
API Key Initialized    （L1 create_or_derive + L2 凭据就绪）
   │
   v
RiskEngine Ready       （风控状态加载）
   │
   v
FastExecutor Warmup    （EIP-712 domain + HTTP + hot wallet 预热）
   │
   v
RECONCILING            （StateSync.full_reconcile 对账）
   │
   v
READY                  （TradingGateState::Ready，允许交易）
```

## 2. TradingGate 状态

| 状态 | 含义 | 放行交易 |
|------|------|---------|
| `Initializing` | 系统初始化中 | ❌ |
| `Reconciling` | 启动对账中 | ❌ |
| `Ready` | 对账通过 | ✅ |

- `ensure_ready()` 在非 Ready 状态必须阻止订单提交。
- 对账失败 → 闸门保持关闭（不允许交易）。

## 3. 当前缺口（对应 MASTER_TASKS）

- P0-10：`full_reconcile()` 尚未接入启动流程（`RECONCILING` 阶段无正式调用）。
- P0-11：对账通过后尚未切换到 `TradingGateState::Ready`。

## 4. 关键约束

- `Startup Reconciliation` 不可绕过（Human 把关）。
- 重启后必须回到 `RECONCILING` 重新对账，禁止携带旧状态直接进入 READY。

## 5. 待补充（v0.1 → v1.0）

- [ ] 异常路径：对账失败 / 超时 / 部分成功的状态迁移
- [ ] 运行中从 Ready 降级回 Reconciling 的触发条件
