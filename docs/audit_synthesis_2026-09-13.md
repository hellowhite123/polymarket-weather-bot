# 审计意见综合（GPT × Gemini）— 2026-09-13

> 来源：
> 1. GPT — `POLYMARKET_WEATHER_FINAL_CODE_AUDIT.md`（本地文件，编号 P0-003~015 / P1-017~040 / P2-001~010）
> 2. Gemini — 资料库《Polymarket 量化交易系统代码审查修改建议汇总》（`/space/d/Ihaiu2nNjEPdmIAyv17eEK`，按文件组织，⚠️=正确性/安全，💡=优化）
>
> 说明：「修改级别」= P0（实盘阻塞/资金安全）> P1（生命周期/可靠性）> P2（性能/精度/测试）；「修改时间」= 预计修复工时（人天，1 人天≈8h）。

---

## 一、综合去重清单

### P0 — 实盘阻塞（9 项）

| 问题 | GPT | Gemini | 工时 |
|------|-----|--------|------|
| Trade 归属 attribution（不能假设 taker_order_id=用户订单） | P0-013 | — | 2 |
| MarketEventPoller → ExecutionService 未接线 | P0-014 | — | 2 |
| Fill 幂等 / 重复记账（统一 fill_id/trade_id） | P0-015 | — | 2 |
| full_reconcile 只有检测、没有恢复 | P0-007B | — | 2 |
| Risk check-and-reserve 非原子（check_and_reserve） | P0-008 | — | 1 |
| Nonce 并发分配（原子递增） | P0-010 | ⚠️ executor | 1 |
| Nonce 确认语义（confirmed/allocated/pending 状态机） | P0-011 | — | 1 |
| Mempool capacity TOCTOU（try_submit 原子化） | P0-012 | — | 0.75 |
| Risk 输入校验 NaN/Inf/负值（is_finite + >0） | P0-009 | — | 0.5 |

**P0 小计 ≈ 12.25 人天**

### P1 — 生命周期 / 可靠性（约 25 项）

| 问题 | GPT | Gemini | 工时 |
|------|-----|--------|------|
| 一个订单多个 partial fills（FillLedger 改 fill_id 主键） | P1-026 | — | 1 |
| Fill recovery cursor（重启/断线恢复） | P1-032 | — | 1 |
| OrderStateChange 无生产 consumer | P1-029 | — | 1 |
| open-order 消失分类（filled/cancelled/expired） | P1-040 | — | 1 |
| order_flow `tokio::Mutex` 串行瓶颈（并发发单被锁死） | — | ⚠️ 最高 | 1 |
| Python gRPC 断线重试（指数退避） | — | ⚠️ 最高 | 1 |
| 预签名池线程安全（take 原子 + 失败不放回） | — | ⚠️ | 0.75 |
| PendingCancel 状态（撤单并发冲突） | — | ⚠️ | 0.75 |
| Trade / Order cursor pagination | P1-027 / P1-028 | — | 0.75 |
| parse_units 静默失败（极端小数返回"0"） | P2-008 | ⚠️ clob | 0.5 |
| 跨链重放保护（EIP-712 绑定 chain.id） | P2-010 | ⚠️ clob | 0.5 |
| 浮点累积误差（幽灵残留 1e-13 → remove） | — | ⚠️ risk | 0.5 |
| proto price/quantity 用 double（防 float 截断） | — | ⚠️ py | 0.5 |
| update_fill 绝对值 vs 增量语义 | — | ⚠️ order_state | 0.5 |
| poll_open_orders L2 认证未统一 | P1-030 | — | 0.5 |
| FillRecord 生命周期模型冲突 | P1-033 | — | 0.5 |
| settlement 幂等（重复结算覆盖） | P1-034 | — | 0.5 |
| settlement 结果校验（仅 0/1） | P1-035 | — | 0.5 |
| invalid side 静默返回 PnL=0 | P1-036 | — | 0.5 |
| Drawdown warning 判断顺序 | P1-037 | — | 0.5 |
| settlement metadata 缺失 fail-open | P1-038 | — | 0.5 |
| Trade metadata 字段不足 | P1-039 | — | 0.5 |
| Mempool dedup reservation 过早 | P1-017 | — | 0.5 |
| Trade status 未充分利用 | P1-031 | — | 0.5 |

**P1 小计 ≈ 15.75 人天**

### P2 — 性能 / 精度 / 测试（约 14 项）

| 问题 | GPT | Gemini | 工时 |
|------|-----|--------|------|
| OrderStore HashMap → DashMap 分片锁 | P2-001 | 💡 | 1 |
| RiskStateInternal tokio::Mutex → std RwLock | P2-002 | 💡 | 0.5 |
| RiskEngine O(N) exposure → O(1) global_exposure | P2-003 | ⚠️ risk | 0.5 |
| Nested RwLock 重构 | P2-004 | — | 0.5 |
| Batch order 受控并发 | P2-005 | — | 0.5 |
| f64 → Decimal / 定点（长期） | P2-006 | — | 长期 |
| CLOB rounding boundary tests | P2-007 | — | 0.5 |
| HTTP 429 限频 / 令牌桶 | P2-009 | 💡 clob | 0.5 |
| EIP-712 chain-id/domain/签名回归测试 | P2-010 | — | 0.5 |
| side String → 枚举（去堆分配） | — | 💡 | 0.5 |
| token_id Arc<str> | — | 💡 | 0.5 |
| Mempool Monitor 内存泄漏（LRU/sweep） | — | 💡 executor | 0.5 |
| NTP 时钟漂移容差（122 分钟） | — | 💡 risk | 0.5 |
| 进程守护化（cron / while 循环） | — | 💡 py | 0.5 |

**P2 小计 ≈ 7 人天（不含 Decimal 长期项）**

---

## 二、关键判断

### 1. 两份意见高度互补
- GPT 主攻**正确性/资金安全**（Trade→Fill→Risk→StateSync 状态一致性链），Gemini 主攻**并发/性能/可靠性**（锁竞争、Nonce、预签名池、断线重试、内存泄漏）。
- 合并后覆盖更全，无重大冲突。

### 2. 双方独立确认（可信度最高，优先处理）
| 问题 | 结论 |
|------|------|
| Nonce 并发分配 | GPT P0-010 + Gemini ⚠️ → 提级 P0 |
| parse_units 静默失败 | GPT P2-008 + Gemini ⚠️ → 提级 P1 |
| O(N) exposure | GPT P2-003 + Gemini ⚠️ → 确认为性能项 |
| DashMap / RwLock 降级 | 双方 💡 → P2 |
| EIP-712 chain-id 重放保护 | 双方 → P1 需验证 |
| 429 限频 | 双方 → P2 |

### 3. 矛盾点（已核实）
- **claim_or_retry**：GPT 判 P0-004 ✅ 已完成；Gemini 称 service.rs 仍用「get+create 两步」。
- 核实结论：**Gemini 审计的是旧代码**。本项目 09-10 已修复（P1-010，commit `dac0976`，`claim_or_retry` 已原子化并接入 service.rs）。**该条可关闭。**

### 4. Gemini 独有且 GPT 遗漏的增量项
- `order_flow` tokio::Mutex 串行瓶颈（并发发单被排队锁死）— 延迟敏感，实盘前应处理。
- Python gRPC 断线重试（Rust 幂等已就绪，重试安全）— 信号可靠性。
- 预签名池失败不放回、Mempool Monitor 内存泄漏。

---

## 三、修复顺序与实盘时间线

按资金安全 + 状态一致性依赖（整合 GPT 五阶段 + Gemini 增量）：

| 阶段 | 内容 | 累计工时 |
|------|------|---------|
| ① Trade 一致性链 | P0-013 归属 → P0-014 接线 → P0-015 幂等 | 6 |
| ② 资金并发安全 | P0-008 reserve → P0-009 校验 → P0-010/011 Nonce → P0-012 Mempool | 4.25 |
| ③ 状态恢复 | P0-007B 恢复 + P1-026/032/040 fill/recovery | 6 |
| ④ 可靠性（Gemini 增量） | 断线重试、串行瓶颈、预签名池、parse_units、重放保护 | 4.25 |
| ⑤ P1 收尾 + P2 优化 | 其余 P1 + P2 | 约 14.5 |

**结论**：P0 全部闭环约 **12.25 人天**；达到「可进入离线回放 → paper → shadow」门禁约 **22 人天**（P0 + 关键 P1/可靠性）；全量（含 P2）约 **35 人天**。

> 实盘 Go/No-Go 底线（两份意见一致）：Trade 正确归属、事件真正接线、一笔成交只记一次、一单支持多笔 partial fill、risk check+reserve 原子、NaN/Inf/负值全拒、Nonce 并发安全、Mempool 容量原子、StateSync 能恢复真实 fill、重启不重复记账、settlement 不重复结算、关键路径有集成测试。
