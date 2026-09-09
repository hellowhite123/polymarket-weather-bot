# EXECUTION_MODEL.md

**架构真源** · 版本 v0.1 · 2026-08-25 · 待随审计完善

## 1. 核心执行链路（协议 §23）

```text
Python gRPC OrderIntent
      │
      v
Trading Gate  （ensure_ready 闸门）
      │
      v
Atomic Idempotency  （order_id 去重 / 幂等）
      │
      v
Risk pre_check
      │
      v
Order Submit  →  CLOB
      │
      v
Reconciler / StateSync
      │
      v
Actual Fill
      │
      v
Position / FillLedger
      │
      v
RiskEngine.post_check
      │
      v
Trading Gate
```

## 2. FastExecutor 亚秒级执行层（15 子模块）

| 模块 | 职责 |
|------|------|
| hot_wallet | 热钱包，冷解锁 PBKDF2 21.7ms，热命中 0.2μs |
| signing_cache | 签名缓存 |
| market_cache | 市场缓存 |
| http_fast | 快速 HTTP 客户端 |
| latency | 延迟监控 |
| presign | 预签名池（take 0.7μs） |
| executor | 执行主路径 |
| mempool | 去重引擎 + 双层限频 + pending 追踪 + 防抢跑 |
| tx_lifecycle | TxState 状态机 + TxTracker 轮询 + RBF + 链上 receipt |
| nonce_manager | NonceTracker + StuckTx 检测 + RBF 重试 + 链上 nonce 同步 |
| rpc_pool | 5 节点 RPC 池 + 健康检查 + 故障切换 + LLB |
| risk_engine | 仓位硬限制 + 时间窗口 + 3 级熔断 + UMA 争议 + Gas 保护 |
| daemon | 六阶段周期维护（2s/5s/15s/30s/60s） |
| market_events | CLOB HTTP 轮询（成交/订单簿/结算） |
| state_sync | 本地账簿与链上对账 + 断线恢复 |

另有 `uma_monitor`（UMA OO 事件监听）与 `fill_records`（FillLedger 账簿）。

## 3. 签名与认证

- **L1**：EIP-712 ClobAuth 签名（4 字段 domain），创建/派生 API Key。
- **L2**：HMAC-SHA256（消息 `{timestamp}{METHOD}{path}{body}`），secret 用 URL_SAFE base64 带 padding 解码。
- 钱包：AES-256-GCM 加密 keystore，`chain_id = 137`。

## 4. 延迟基准（2026-08-21 实测）

- 热路径本地 ≈ 43μs；EIP-712 完整签名 36μs（keccak 1.3μs + secp256k1）。
- 本地合计 < 1.5ms，占 100ms 预算 ~1%；99% 延迟在网络层。
- bench：`examples/latency_bench.rs` + `latency_bench_py.py`；报告 `latency_benchmark_report.md`。

## 5. 权威状态共享

`Position / Risk State / Fill Ledger / Order Store` 必须共享权威状态，BUY/SELL 可异步执行但不得各持副本。

## 6. 待补充（v0.1 → v1.0）

- [ ] 私有 mempool / RPC 部署拓扑（网络层优化方向）
- [ ] 批量预签的吞吐上限
- [ ] 断线恢复的具体状态回放策略
