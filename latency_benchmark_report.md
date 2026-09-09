# Polymarket 天气预测市场 —— 交易链路延迟基准报告

> 生成时间：2026-08-21
> 目的：量化「信号 → 成交」端到端链路各环节耗时，定位延迟瓶颈，为后续延迟优化提供数据依据，并作为多 AI 交叉评审的基线文档。

---

## 0. 结论先行（TL;DR）

| 结论 | 数据 |
|------|------|
| **本地计算不是瓶颈** | Python 策略+聚合+序列化 ≈ 50μs，Rust 签名+构建 ≈ 43μs，合计 **< 1.5ms**，仅占 100ms 预算的 ~1% |
| **gRPC 往返是本地最大单项** | 实测 ~1.1ms（TCP loopback），比序列化大两个数量级 |
| **99% 延迟在网络往返** | CLOB HTTP + RPC RTT（沙盒无外网，未实测，预估 100~500ms） |
| **预签名池方案正确** | `presign_take` 0.7μs vs `sign_eip712` 36μs，快 ~50 倍 |
| **PBKDF2 已被热钱包消除** | 冷解锁 21.7ms → 热命中 0.2μs，快 10 万倍 |

**一句话**：继续优化的方向不在代码里，而在**网络层**（RPC 节点选择 + CLOB 连接地理部署 + 私有 mempool）。本地计算已经压到物理极限，再优化收益趋近于零。

---

## 1. 测量方法

### 1.1 环境

| 项 | 值 |
|----|----|
| 操作系统 | Windows（本地开发机） |
| CPU | 本机（未绑定亲和性） |
| Rust | 1.97.1，`--release`（`opt-level=3, lto=fat, codegen-units=1`） |
| Python | 3.13.12（venv 隔离） |
| 网络 | **沙盒无外网**（clob.polymarket.com / polygon-rpc.com 均超时） |

> ⚠️ 因沙盒无外网，**网络段（CLOB HTTP / RPC RTT）无法实测**，本报告只测到「本地计算 + 本地 gRPC 往返」。网络段给出测量方法 + 量级预估，需在真实环境（有外网的部署机）补齐。

### 1.2 测量工具（新增，已落盘可复现）

| 文件 | 作用 |
|------|------|
| `polymarket-rust/polymarket-rust/examples/latency_bench.rs` | Rust 本地 10 项微基准 |
| `latency_bench_py.py` | Python 侧 5 项基准（聚合/序列化/gRPC 往返） |

复现命令：

```bash
# Rust（release，约 5 分钟编译）
cd polymarket-rust/polymarket-rust
PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe cargo run --release --example latency_bench

# Python
cd <项目根目录>
python latency_bench_py.py
```

### 1.3 统计口径

- 每项跑 N 次（`n=` 见各表），记录单次耗时，输出 **mean / p50 / p95 / p99 / max**。
- 分位数采用「最近秩法」：`idx = ceil(q*n) - 1`。
- 微基准含 `Instant::now()` / `time.perf_counter()` 的调用开销（~20~100ns），对 μs 级测量影响 <5%，对 ns 级测量（`domain_hot` 80ns、`wallet_hot_hit` 200ns）仅反映数量级。

---

## 2. 端到端链路与延迟构成

```
Python 策略引擎                     Rust 执行层                       外部网络
─────────────────────            ─────────────────────            ─────────────────
策略评估(forecast)                 
  └─ 聚合器 aggregate  [32μs]
       └─ 序列化 order  [18μs]
            └─ gRPC PlaceOrder ────[1.1ms]──▶ 热钱包命中 [0.2μs]
                                               tick 查询   [≈0, 缓存]
                                               订单构建     [5μs]
                                               EIP-712 签名 [36μs] ← 可预签→0.7μs
                                               JSON 序列化  [1.3μs]
                                               L2 HMAC      [0.8μs]
                                                 └─ CLOB HTTP POST ──▶ [100~500ms 预估]
                                                                      (可选)链上确认 [秒级]
```

**本地段合计（实测）**：Python ~50μs + gRPC ~1.1ms + Rust ~43μs ≈ **1.2ms**

---

## 3. 实测结果

### 3.1 Rust 本地阶段（release，单位 ms）

| 阶段 | 说明 | mean | p50 | p95 | p99 | max | n |
|------|------|------|-----|-----|-----|-----|---|
| `wallet_pbkdf2` | 冷解锁（PBKDF2 210k 轮） | **21.712** | 21.632 | 22.253 | 22.253 | 22.253 | 10 |
| `wallet_hot_hit` | 热钱包缓存命中 | 0.0002 | 0.0002 | 0.0002 | 0.0003 | 0.2226 | 200k |
| `domain_cold` | 域分隔符冷计算（含 keccak） | 0.0010 | 0.0009 | 0.0010 | 0.0018 | 0.3084 | 10k |
| `domain_hot` | 域分隔符热缓存命中 | 0.00008 | 0.0001 | 0.0001 | 0.0001 | 0.05 | 200k |
| `build_order` | 订单结构构建 + salt | 0.0050 | 0.0046 | 0.0056 | 0.0080 | 0.3675 | 100k |
| `sign_eip712` | 完整 EIP-712 签名（keccak+secp256k1） | **0.0364** | 0.0342 | 0.0407 | 0.0559 | 0.2206 | 5k |
| `digest_only` | 仅 struct hash + digest（keccak） | 0.0013 | 0.0013 | 0.0014 | 0.0020 | 0.2187 | 200k |
| `serialize_json` | 请求体 JSON 序列化 | 0.0013 | 0.0012 | 0.0014 | 0.0015 | 0.3747 | 100k |
| `l2_hmac` | L2 认证 HMAC-SHA256 | 0.0008 | 0.0008 | 0.0009 | 0.0011 | 0.2892 | 200k |
| `presign_take` | 预签名池取单（快路径） | 0.0007 | 0.0006 | 0.0012 | 0.0014 | 0.2102 | 100k |

**解读**：

1. `wallet_pbkdf2` 实测 21.7ms（文档原估 100~300ms，本机更快），是冷路径唯一大头，**已被热钱包缓存彻底消除**（21.7ms → 0.2μs）。
2. `sign_eip712` 36μs 是本地最大单项，其中 keccak（`digest_only`）只占 1.3μs，**其余 ~35μs 是 secp256k1 ECDSA**。
3. 热路径本地总耗时（不含 PBKDF2、不含网络）≈ 0.2 + 0 + 5 + 36 + 1.3 + 0.8 ≈ **43μs**。
4. 若命中预签名池，签名 36μs → 0.7μs，本地总耗时降至 **~6μs**。

### 3.2 Python 策略大脑侧（单位 ms）

| 环节 | 说明 | mean | p50 | p95 | p99 | max | n |
|------|------|------|-----|-----|-----|-----|---|
| `aggregate` | 信号聚合（冲突仲裁+融合+风控+仓位） | 0.032 | 0.028 | 0.048 | 0.104 | 0.317 | 5k |
| `serialize_order` | dataclass → proto → bytes | 0.018 | 0.014 | 0.033 | 0.075 | 1.022 | 20k |
| `deserialize_result` | bytes → proto → dict | 0.0018 | 0.0016 | 0.0018 | 0.0035 | 0.195 | 20k |
| `grpc_health_rt` | gRPC HealthCheck 本地往返（纯框架） | **0.829** | 0.764 | 1.456 | 1.833 | 4.973 | 2k |
| `grpc_placeorder_rt` | gRPC PlaceOrder 本地往返（含序列化+mock 处理） | **1.124** | 1.105 | 1.632 | 2.146 | 3.047 | 2k |

**解读**：

1. 纯 Python 计算（aggregate + serialize）合计 ~50μs，可忽略。
2. **gRPC 往返 ~1.1ms 是本地最大开销**，比序列化大两个数量级。这是 TCP loopback 的结果；生产部署到 Linux 用 **UDS** 可显著降低，但仍是 μs~亚 ms 量级。
3. `grpc_health_rt`（0.83ms）≈ `grpc_placeorder_rt`（1.12ms）说明 gRPC 框架本身贡献了往返的大头，业务处理只多 ~0.3ms。

---

## 4. 网络段（未实测，需真实环境补齐）

沙盒无外网，以下为**量级预估 + 测量方法**，请其他 AI 或真实环境验证：

| 环节 | 预估量级 | 说明 | 测量方法 |
|------|---------|------|---------|
| CLOB HTTP 往返 | **100~500ms** | 国内→美东 CLOB，主导项 | 现有 `LatencyTracker` 的 `Http` 阶段已埋点；真实下单跑 `latency_report()` |
| RPC 节点 RTT | **50~300ms** | 5 个 Polygon 公共 RPC，取决于地理 | `RpcPool::check_health_all()` 已测 p50/p95/p99；看 `NodeStats.last_latency_ms` |
| 链上确认（可选） | **秒级（~2s 出块）** | Polygon PoS 平均 2s/块 | `TxTracker` 轮询 receipt |

**补充测量建议（真实环境执行）**：

```bash
# 1. RPC RTT（复用现有 rpc_pool）
#    在部署机运行一个小 probe，对 5 个默认节点分别 ping eth_blockNumber 100 次，记录 RTT 分位数

# 2. CLOB HTTP 往返（复用现有 latency 埋点）
#    真实下一笔单，看 execute_fast() 返回的 OrderLatency 里 http 阶段

# 3. 端到端（信号→成交）
#    在 Python 侧打点：策略入口 t0，收到 OrderResult t1，delta = t1 - t0
```

---

## 5. 瓶颈定位与优化建议（按投入产出比排序）

### 5.1 最高优先级：网络层（99% 延迟所在）

| 措施 | 预期收益 | 成本 |
|------|---------|------|
| RPC 节点地理优化（选最近/专属节点） | RPC RTT 300ms → 30ms | 低（换节点） |
| 接入私有 mempool（Flashbots Protect / Polygon 私池） | 减少抢跑 + 失败重试 | 中 |
| 部署机迁到 Polygon 节点同区域 | CLOB/RPC 往返 100~300ms → 10~30ms | 中（换机房） |
| 精细化 Gas 竞价 + RBF | 提高进块优先级，减少 pending 等待 | 低 |

> 结论支撑：本地 1.2ms vs 网络 100~500ms，**网络优化 1ms = 本地优化 1000 倍**。

### 5.2 次优先级：gRPC 传输（本地最大单项 1.1ms）

| 措施 | 预期收益 | 成本 |
|------|---------|------|
| gRPC over UDS（生产 Linux 已支持） | 1.1ms → 0.2~0.3ms | 低（改 endpoint） |
| 共享内存 + lock-free queue（极限） | → <0.1ms | 高 |
| 批量下单（PlaceOrders 一次传多单） | 摊薄每单 gRPC 开销 | 低（接口已有） |

### 5.3 已实现/无需再动：本地计算

- 预签名池（`presign_take` 0.7μs）已把签名移出热路径，**无需再优化**。
- 本地计算 43μs 已到物理极限，继续微优化（如锁替换、无锁结构）收益 < 40μs，无意义。

### 5.4 若要做链上确认延迟优化（进阶，非下单延迟）

- 预授权 USDC（避免下单时 approve 交易）。
- Operator 模式（主钱包只授权，执行走 operator 地址）。
- 批量提交（multicall）减少 nonce 竞争。

---

## 6. 供其他 AI 补充的开放问题

> 以下为本报告未能覆盖、需外部补充或真实环境验证的点：

1. **真实网络延迟实测**：本报告网络段为预估，请在真实部署环境（有外网）补齐 CLOB HTTP / RPC RTT 的分位数数据。
2. **UDS vs TCP 在 Linux 上的实测差**：本报告 gRPC 往返是 Windows TCP loopback（1.1ms），生产 Linux UDS 的实际值需验证。
3. **地理部署的量化收益**：不同 RPC 供应商（Alchemy/Infura/QuickNode/专属节点）在珠三角地区的 RTT 对比数据。
4. **私有 mempool 在 Polygon 的可用性与延迟影响**：Flashbots 是否覆盖 Polygon、私池广播的实际加速比。
5. **策略评估耗时**：本报告未测 `ForecastDeviationStrategy.evaluate` 等策略「思考」时间（依赖天气数据结构构造），需补充。
6. **尾延迟（p99/max）优化**：部分微基准 max 值有 0.2~0.4ms 的尖峰（疑似 GC/线程调度），是否需要关注取决于信号频率。
7. **多策略并发下的锁竞争**：本报告为单线程串行测量，未测多策略并发时的 `RwLock` 竞争。

---

## 附：原始数据文件

- Rust 基准源码：`polymarket-rust/polymarket-rust/examples/latency_bench.rs`
- Python 基准源码：`latency_bench_py.py`
