# DCR-001 — B2 (P1-009) 持久化/重启恢复语义冲突

- **状态**：`APPROVED — 方案 A（2026-10-04，Human 批准）`
- **触发**：`docs/INSTRUCTION_TO_OPENCODE_ROUND05_B_IMPLEMENTATION.md` §B2 vs `docs/design/ROUND05_B_DESIGN_DRAFTS.md`（Design V2）B2 §1
- **触发规则**：实施指令 §0 L21 / §B2 Stop condition L194（「durable persistence 未冻结 → 停止并提交 DCR」）

## 1. 受影响 B 项
B2 / P1-009 Unknown Order Reconciliation。

## 2. 确切的冲突/遗漏
- 冻结设计：`restart recovery not supported`（OrderStore 纯内存，无持久化；Allowed Files 无 persistence 文件）。
- 实施指令要求持久化 + Required Test「Durable state survives restart and continues reconciliation」。
- 两者对**持久化语义**与**重启恢复承诺**直接矛盾；指令 §1 Forbidden 又禁止「改持久化语义」。

## 3. 当前代码证据
- `polymarket-rust/polymarket-rust/src/fast/order_state.rs`：`OrderStore` 为内存 `RwLock<HashMap>`，无落盘/回放。
- `order_state.rs:438+` `pending_exchange_order_ids()` 仅收 `Received` 且 `exchange_order_id` 存在者；`Received` 且无 exchange id（in-doubt）被对账候选排除。
- `grpc/service.rs:952-954` unknown outcome 保留 `Received`（不终态）。
- `grpc/service.rs:273-274` `snapshot_pending_exchange_order_ids()` 供 StateSync 使用。

## 4. 建议的语义变更（二选一，需审批）
- **方案 A（推荐，最小）**：维持冻结语义「不支持跨重启」，**删除/降级**指令 B2 的 durable persistence 要求，将「Durable state survives restart」标为 `DEFERRED`（需另立持久化专项）。B2 只实现 in-doubt 表示 + 对账候选 + TTL 不释放 + 匹配优先级 + 进程内幂等。
- **方案 B（扩张）**：新建持久化专项（durable journal / SQLite WAL），冻结 journal 记录结构、action identity、启动回放顺序、correction channel，再实现 B2。范围与风险显著扩大。

## 5. 风险/下行
- 选 A：重启后 in-doubt 订单丢失（与设计一致，但指令的 restart 测试无法满足）。
- 选 B：引入持久化组件、启动回放与 schema 迁移，需重新设计/审计，工期与资金风险上升。

## 6. 迁移/重启影响
- A：无额外迁移；重启即冷启动（沿用现状）。
- B：需定 journal 格式、版本、回放幂等、磁盘故障路径。

## 7. 所需测试
- A：in-doubt 不进 READY；唯一命中→Submitted；ambiguous→unresolved；TTL 不释放 reserve；重复 trade/cancel no-op；`restart` 测试标 `DEFERRED/skip`。
- B：外加 durable 恢复后继续对账、回放幂等、损坏 journal fail-closed。

## 8. 回滚方案
- 任一方案均只动 order_state/service/server/state_sync（+B 的持久化文件）；回滚 = revert 对应提交，恢复内存态。

## 9. 将变更的文件
- A：`fast/order_state.rs`、`grpc/service.rs`、`bin/server.rs`、`fast/state_sync.rs`（无持久化文件）。
- B：外加持久化新文件 + 可能的 `proto/polymarket.proto`。

## 10. 批准结果
**已批准方案 A**（2026-10-04）：维持冻结语义「不支持跨重启」；实施 in-doubt 对账候选 + 匹配优先级 + TTL 不释放 + 进程内幂等；「Durable state survives restart」测试标 `DEFERRED`；**不新增持久化文件**。B2 恢复实施。
