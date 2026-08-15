//! 亚秒级执行层 (fast path)
//!
//! 把「信号 → 成交」的**本地计算**压进 100ms 预算，剩余全部留给网络往返。
//!
//! ## 模块划分
//!
//! - [`hot_wallet`] —— 热钱包缓存，消除每次下单的 PBKDF2（100~300ms 主瓶颈）
//! - [`signing_cache`] —— EIP-712 域分隔符预计算 + 零堆分配 struct hash（并修复了一处签名正确性 Bug）
//! - [`market_cache`] —— 市场静态元数据（tickSize / negRisk）缓存，消除额外 `GET /tick-size` 往返
//! - [`http_fast`] —— HTTP/2 连接池调优 + TLS 预热
//! - [`latency`] —— 分阶段延迟埋点（P50/P95/P99）与超标告警
//! - [`presign`] —— 预签名订单池，把签名彻底移出关键路径
//! - [`executor`] —— [`FastExecutor`]：整合上述组件，提供亚百毫秒 `execute_fast()` 接口
//! - [`mempool`] —— Mempool Monitor：去重、限频、pending 追踪、防抢跑
//! - [`tx_lifecycle`] —— TxLifecycle：交易广播到链上确认的完整生命周期管理
//! - [`nonce_manager`] —— NonceManager：nonce 追踪、stuck 检测、RBF 费用替换
//! - [`rpc_pool`] —— RpcPool：3-5 个 Polygon RPC 节点自动切换、健康检查、故障恢复
//! - [`daemon`] —— BackgroundDaemon：整合所有子模块的后台周期性维护任务
//! - [`risk_engine`] —— RiskEngine：仓位限制 + 时间窗口 + 日亏损熔断 + UMA争议 + Gas保护
//! - [`market_events`] —— MarketEvents：实时市场事件抓取（成交/订单簿/结算）
//! - [`state_sync`] —— StateSync：本地账簿与链上定期对账，断线自动恢复
//! - [`fill_records`] —— FillLedger：成交记录账簿（下单/成交/结算完整生命周期字段）

pub mod daemon;
pub mod executor;
pub mod fill_records;
pub mod hot_wallet;
pub mod http_fast;
pub mod latency;
pub mod market_cache;
pub mod market_events;
pub mod mempool;
pub mod nonce_manager;
pub mod order_state;
pub mod presign;
pub mod rpc_pool;
pub mod state_sync;
pub mod signing_cache;
pub mod tx_lifecycle;
pub mod risk_engine;
pub mod uma_monitor;
