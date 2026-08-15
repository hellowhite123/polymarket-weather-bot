# Polymarket Rust 执行层 — 今日交付总结

## 日期：2026-08-08

---

## 一、新增模块（8 个）

### 1. Mempool Monitor (`src/fast/mempool.rs`，~620行，25个测试)
- **去重引擎**：OrderFingerprint + 滑动窗口(3s)
- **双层限频**：TokenBucket全局 + SlidingWindow per-token
- **Pending追踪**：PendingOrderTracker + TTL过期清理
- **防抢跑**：FrontRunDetector trait（可插拔）
- 接入 FastExecutor：check_before_submit() + on_submitted() 双钩子

### 2. TxLifecycle (`src/fast/tx_lifecycle.rs`，~430行，12个测试)
- **状态机**：Submitted→Accepted→Pending→Confirmed/Failed/Reverted/Stale
- **TxTracker**：HashMap<tx_hash, TxEntry> + tick() 本地stuck检测
- **tick_chain()**：通过RpcPool查询链上receipt自动推进状态
- **RBF支持**：calculate_rbf_gas_price(1.125x) + prepare_rbf_replacement
- **回调机制**：on_state_change() 注册状态变更通知

### 3. NonceManager (`src/fast/nonce_manager.rs`，~440行，17个测试)
- **NonceTracker**：onchain_nonce + pending_set + 缓存TTL=30s
- **NonceManager**：next_nonce() → register_tx() → confirm() → tick()
- **sync_from_chain()**：通过RpcPool查询eth_getTransactionCount
- **RBF重试**：gas×1.125加速，最多5次，stuck检测120s
- **紧急恢复**：sync(chian) / reset(new_nonce)

### 4. RpcPool (`src/fast/rpc_pool.rs`，~400行，13个测试)
- **5个Polygon公共RPC**：polygon-rpc.com, maticvigil, llamarpc, meowrpc, 1rpc.io
- **健康检查**：eth_blockNumber + 延迟测量
- **节点状态**：Healthy(<800ms) / Degraded(800ms-5s) / Dead(>5s)
- **故障切换**：Least-latency优先 + round-robin回退
- **自动恢复**：指数退避(10s→20s→40s→60s)
- **零额外依赖**：直接用reqwest+serde_json发JSON-RPC

### 5. RiskEngine (`src/fast/risk_engine.rs`，~850行，20个测试)
- **仓位硬限制**：单市场+全局暴露(USDC)
- **时间窗口风控**：结算前2h禁开Buy，Sell放行
- **日亏损熔断**：3级 Warn→Restricted→Halted
- **UMA争议监控**：OracleDisputeMonitor，偏差>5%预警
- **Gas费保护**：单笔上限500gwei
- **补充**：DrawdownTracker + ConcentrationChecker
- **双钩子**：pre_check() / post_check()

### 6. BackgroundDaemon (`src/fast/daemon.rs`，~150行，2个测试)
- **2s**：RpcPool.check_health_all()
- **5s**：TxTracker.tick_chain() 链上状态同步
- **15s**：TxTracker.tick() 本地stuck检测
- **30s**：NonceManager.sync_from_chain()
- **60s**：FastExecutor.sweep()
- CancellationToken优雅关闭

### 7. MarketEvents (`src/fast/market_events.rs`，~320行，4个测试)
- **4种事件**：Trade / OrderBookUpdate / OrderStateChange / Settlement
- **MarketEventPoller**：HTTP轮询CLOB API (trades/book/orders)
- **EventBus**：broadcast::channel(256) 事件分发
- **去重**：trade_id缓存去重

### 8. StateSync (`src/fast/state_sync.rs`，~350行，10个测试)
- **持仓对账**：本地vs CLOB API reconcile_positions()
- **订单对账**：pending orders vs /data/orders
- **断线恢复**：ConnectionState状态机 + 指数退避重连(1s→60s)
- **ReconnectPolicy**：最多10次重试

---

## 二、修改集成（5个文件）

| 文件 | 修改内容 |
|------|---------|
| `executor.rs` | 新增rpc_pool/nonce_mgr/tx_tracker字段 + with_full_guard() |
| `mod.rs` | 注册全部8个新模块 |
| `lib.rs` | 重导出30+新类型 |
| `enums.rs` | Side枚举加Hash derive |
| `signing_cache.rs` | parse_uint256十进制fallback修复 |

---

## 三、测试统计

| 阶段 | 测试数 |
|------|--------|
| 今日起始 | 80 |
| 今日新增 | +105 |
| **最终** | **185 passed, 0 failed** |

---

## 四、全链路架构

```
BackgroundDaemon (tokio)
├─ 2s  → RpcPool.health_check()         [5节点状态]
├─ 5s  → TxTracker.tick_chain()         [链上receipt同步]
├─ 15s → TxTracker.tick()               [本地stuck检测]
├─ 30s → NonceManager.sync_from_chain() [nonce同步]
└─ 60s → FastExecutor.sweep()           [过期清理]

FastExecutor.execute_fast()
├─ WalletUnlock
├─ TickLookup
├─ Nonce验证 (NonceManager)
├─ RiskEngine.pre_check()               ★ 风控预检
│   ├─ 熔断检查 Hablt→全拒 Restricted→只平仓
│   ├─ Gas费检查 >500gwei拒
│   ├─ 结算窗口检查 2h内禁Buy
│   ├─ 仓位硬限制 单市场+全局
│   └─ 集中度检查
├─ Build + Sign
├─ MempoolMonitor.check_before_submit()  ★ 去重+限频+防抢跑
├─ HTTP 提交
├─ MempoolMonitor.on_submitted()         ★ pending记录
├─ TxTracker.register()                  ★ 生命周期注册
└─ RiskEngine.post_check()               ★ 持仓+盈亏更新
```

---

## 五、文件清单

```
src/fast/
├── daemon.rs          (150行, 2个测试)
├── executor.rs        (集成修改, 3个测试)
├── hot_wallet.rs      (已有)
├── http_fast.rs       (已有)
├── latency.rs         (已有)
├── market_cache.rs    (已有)
├── market_events.rs   (320行, 4个测试) ★NEW
├── mempool.rs         (620行, 25个测试) ★NEW
├── mod.rs             (注册修改)
├── nonce_manager.rs   (440行, 17个测试) ★NEW
├── presign.rs         (已有)
├── risk_engine.rs     (850行, 20个测试) ★NEW
├── rpc_pool.rs        (400行, 13个测试) ★NEW
├── signing_cache.rs   (修复)
├── state_sync.rs      (350行, 10个测试) ★NEW
└── tx_lifecycle.rs    (430行, 12个测试) ★NEW

src/
├── lib.rs             (重导出修改)
└── types/enums.rs     (Side加Hash)
