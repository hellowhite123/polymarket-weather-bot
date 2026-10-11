# SYSTEM_ARCHITECTURE.md

**架构真源** · 版本 v0.1 · 2026-08-25 · 待随审计完善

> 若代码与本文冲突，视为 Architecture Drift，必须提出 Finding（协议 §22）。

## 1. 顶层架构

双层架构：Python 策略大脑 + Rust 执行手脚，gRPC 通信。

```text
┌─────────────────────────────── Python ───────────────────────────────┐
│ weather_data_ingestion  多源气象数据接入（NOAA/彩云/ECMWF/JMA）        │
│ strategy_engine         策略引擎（预报偏差/多源/临近/历史统计）          │
│                         + 数据融合 FusionEngine + 信号聚合 + 回测      │
│ market_discovery        新盘扫描 + MarketRegistry 注册表              │
│ gui                      PyQt6 仪表盘                                 │
└───────────────────────────────┬──────────────────────────────────────┘
                                │ gRPC（UDS/TCP，Python=client）
                                ▼
┌─────────────────────────────── Rust ─────────────────────────────────┐
│ grpc/service             tonic server（8 个 RPC，含 GetFills）        │
│ TradingGate              交易闸门（Initializing/Reconciling/Ready）   │
│ FastExecutor             亚秒级执行层（15 子模块）                     │
│ RiskEngine               风控（pre_check/post_check）                 │
│ StateSync                本地账簿与链上对账                            │
│ wallet                   钱包管理（AES-256-GCM + EIP-712 + L1/L2）    │
└───────────────────────────────┬──────────────────────────────────────┘
                                │ HTTPS
                                ▼
                     Polymarket CLOB + Polygon RPC
```

## 2. Python 栈

- `asyncio + aiohttp + pandas`，策略引擎在 `strategy_engine/`。
- 数据融合 `FusionEngine`：Brier Score 追踪 + 衰减加权 + 95% CI。
- `OrderSizeLimits`：下单金额上限按总资产五档动态调整（每单 $1~$100）。

## 3. Rust 栈

- `tokio + ethers-rs + tonic`，执行层在 `polymarket-rust/`。
- 热路径本地总耗时 ≈ 43μs（build 5 + sign 36 + serialize 1.3 + hmac 0.8）；gRPC 本地往返 ~1.1ms（TCP loopback，生产 Linux UDS 更低）。
- 99% 延迟在网络层（CLOB HTTP + RPC RTT），优化方向在网络层。

## 4. 通信契约

- `proto/polymarket.proto`：Python↔Rust 接口契约（8 个 RPC）。
- Python 为 client（`grpc.aio`），Rust 为 server（`tonic`）。

## 5. 待补充（v0.1 → v1.0）

- [ ] 各模块间数据流时序图
- [ ] 部署拓扑（Docker 容器边界、网络隔离）
- [ ] 故障域与恢复路径
