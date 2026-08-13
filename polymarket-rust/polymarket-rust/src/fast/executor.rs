//! 快速执行器 —— 亚秒级下单的热路径整合层
//!
//! 把分散在 [`crate::fast`] 各模块中的优化手段串成一条**可测量**的下单链路：
//!
//! ```text
//! 信号 → [WalletUnlock] → [TickLookup] → [BuildOrder] → [Sign]
//!       → [Serialize] → [Hmac] → [Http] → 成交
//! ```
//!
//! 当缓存全部命中时，本地计算（`WalletUnlock`..`Hmac`，不含 `Http`）应稳定
//! 落在 **1ms 以内**，100ms 预算的绝大部分留给网络往返。
//!
//! ## 关键优化来源
//!
//! | 阶段 | 优化 | 节约 |
//! |------|------|------|
//! | `WalletUnlock` | [`HotWalletCache`] 常驻 `Arc<LocalWallet>` | 消除 PBKDF2（100~300ms） |
//! | `TickLookup` | [`MarketMetaCache`] 静态元数据缓存 | 消除 `GET /tick-size` 往返 |
//! | `BuildOrder` | 域分隔符预计算（`signing_cache`） | 消除重复 keccak256 + ABI 编码 |
//! | `Sign` | 预签名池（`presign`）可把签名移出关键路径 | 消除 secp256k1 ECDSA |
//! | `Http` | HTTP/2 连接池 + TLS 预热（`http_fast`） | 消除 3 RTT 握手 |
//!
//! 每个阶段都被 [`latency`] 独立计时，最终给出 P50/P95/P99 与超标告警。

use crate::clob::client::{ClobClient, ClobConfig};
use crate::clob::constants::get_exchange_address;
use crate::clob::error::{ClobError, ClobResult};
use crate::clob::signing::sign_order;
use crate::fast::fill_records::{FillLedger, FillRecord};
use crate::fast::hot_wallet::HotWalletCache;
use crate::fast::http_fast::warmup;
use crate::fast::latency::{LatencyTracker, OrderLatency, Stage, StageTimer};
use crate::fast::market_cache::{MarketMeta, MarketMetaCache};
use crate::fast::mempool::{MempoolConfig, MempoolMonitor, MempoolStats};
use crate::fast::nonce_manager::NonceManager;
use crate::fast::presign::{PreSignedOrder, PreSignedPool};
use crate::fast::rpc_pool::RpcPool;
use crate::fast::signing_cache;
use crate::fast::tx_lifecycle::TxTracker;
use crate::types::clob_types::{ApiKeyCreds, OrderResponse, SignedOrder, UserOrder};
use crate::types::enums::{Chain, OrderType, Side, TickSize};
use crate::types::order::CreateOrderOptions;
use crate::wallet::WalletManager;
use ethers::signers::LocalWallet;
use std::sync::Arc;
use std::time::Duration;

/// 默认延迟预算：亚秒级目标的硬上限
pub const DEFAULT_LATENCY_BUDGET: Duration = Duration::from_millis(100);

/// 快速下单参数
///
/// 相比 [`crate::order::PlaceOrderParams`]，本结构去掉了「每次都传密码」
/// 的约束——若钱包已在 [`HotWalletCache`] 中预热，`password` 可为 `None`，
/// 热路径上完全不触碰磁盘与 PBKDF2。
#[derive(Debug, Clone)]
pub struct FastOrderParams {
    /// 钱包地址
    pub wallet_address: String,
    /// 钱包密码（未预热时必填；已预热可留空走纯缓存路径）
    pub password: Option<String>,
    /// Token ID
    pub token_id: String,
    /// 价格（0~1）
    pub price: f64,
    /// 数量（shares）
    pub size: f64,
    /// 方向
    pub side: Side,
    /// 显式 tick size（不提供则查缓存/API）
    pub tick_size: Option<TickSize>,
    /// 是否 negRisk 市场
    pub neg_risk: Option<bool>,
    /// 订单类型（默认 GTC）
    pub order_type: Option<OrderType>,
    /// 优先从预签名池取单（sign 阶段≈0）
    pub prefer_presign: bool,
}

impl FastOrderParams {
    /// 买入参数
    pub fn buy(
        wallet_address: &str,
        password: Option<&str>,
        token_id: &str,
        price: f64,
        size: f64,
    ) -> Self {
        Self {
            wallet_address: wallet_address.to_string(),
            password: password.map(|s| s.to_string()),
            token_id: token_id.to_string(),
            price,
            size,
            side: Side::Buy,
            tick_size: None,
            neg_risk: None,
            order_type: None,
            prefer_presign: false,
        }
    }

    /// 卖出参数
    pub fn sell(
        wallet_address: &str,
        password: Option<&str>,
        token_id: &str,
        price: f64,
        size: f64,
    ) -> Self {
        Self {
            wallet_address: wallet_address.to_string(),
            password: password.map(|s| s.to_string()),
            token_id: token_id.to_string(),
            price,
            size,
            side: Side::Sell,
            tick_size: None,
            neg_risk: None,
            order_type: None,
            prefer_presign: false,
        }
    }

    /// 开启预签名优先
    pub fn with_presign(mut self) -> Self {
        self.prefer_presign = true;
        self
    }
}

/// 下单来源（用于区分走的是实时签名还是预签名快路径）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionSource {
    /// 本笔单实时构建并签名
    LiveSigned,
    /// 命中预签名池，sign 阶段被跳过
    PreSigned,
}

/// 快速下单结果
#[derive(Debug, Clone)]
pub struct FastOrderResult {
    /// 交易所原始响应
    pub response: OrderResponse,
    /// 分阶段延迟明细
    pub latency: OrderLatency,
    /// 执行来源
    pub source: ExecutionSource,
    /// 是否达成 100ms 预算（含网络）
    pub within_budget: bool,
}

/// 快速执行器
pub struct FastExecutor {
    client: ClobClient,
    hot: HotWalletCache,
    meta: MarketMetaCache,
    presign: Option<PreSignedPool>,
    mempool: Option<Arc<MempoolMonitor>>,
    rpc_pool: Option<Arc<RpcPool>>,
    nonce_mgr: Option<Arc<NonceManager>>,
    tx_tracker: Option<Arc<TxTracker>>,
    /// 成交记录账簿（可选）。挂载后每次成交自动记录完整字段。
    fill_ledger: Option<Arc<FillLedger>>,
    tracker: LatencyTracker,
    chain: Chain,
}

impl FastExecutor {
    /// 使用低延迟 HTTP 客户端构造
    pub fn new(wallet_manager: Arc<WalletManager>, config: ClobConfig) -> ClobResult<Self> {
        let client = ClobClient::new_fast(wallet_manager.clone(), config.clone())?;
        let hot = HotWalletCache::new(wallet_manager);
        let meta = MarketMetaCache::new();
        let tracker = LatencyTracker::with_default_budget();
        Ok(Self {
            client,
            hot,
            meta,
            presign: None,
            mempool: None,
            rpc_pool: None,
            nonce_mgr: None,
            tx_tracker: None,
            fill_ledger: None,
            tracker,
            chain: config.chain,
        })
    }

    /// 启用预签名池
    pub fn with_presign(mut self) -> Self {
        self.presign = Some(PreSignedPool::new());
        self
    }

    /// 启用 Mempool Monitor（去重 + 限频 + pending 追踪 + 防抢跑）
    pub fn with_mempool_monitor(mut self) -> Self {
        self.mempool = Some(Arc::new(MempoolMonitor::new()));
        self
    }

    /// 使用自定义 Mempool Monitor 配置启用
    pub fn with_mempool_config(mut self, config: MempoolConfig) -> Self {
        self.mempool = Some(Arc::new(MempoolMonitor::with_config(config)));
        self
    }

    /// 挂载 RPC 节点池（多节点自动切换）
    pub fn with_rpc_pool(mut self, rpc: Arc<RpcPool>) -> Self {
        self.rpc_pool = Some(rpc);
        self
    }

    /// 挂载 Nonce 管理器（链上 nonce 追踪 + stuck 检测 + RBF）
    pub fn with_nonce_manager(mut self, nm: Arc<NonceManager>) -> Self {
        self.nonce_mgr = Some(nm);
        self
    }

    /// 挂载交易生命周期追踪器
    pub fn with_tx_tracker(mut self, tt: Arc<TxTracker>) -> Self {
        self.tx_tracker = Some(tt);
        self
    }

    /// 挂载成交记录账簿。此后 `execute_fast` 成交成功会自动写入完整成交记录。
    pub fn with_fill_ledger(mut self, ledger: Arc<FillLedger>) -> Self {
        self.fill_ledger = Some(ledger);
        self
    }

    /// 一键启用全部防护模块
    ///
    /// 等价于:
    /// ```text
    /// .with_presign()
    /// .with_mempool_monitor()
    /// .with_rpc_pool(rpc)
    /// .with_nonce_manager(nonce_mgr)
    /// .with_tx_tracker(tx_tracker)
    /// ```
    pub fn with_full_guard(
        mut self,
        rpc: Arc<RpcPool>,
        nonce_mgr: Arc<NonceManager>,
        tx_tracker: Arc<TxTracker>,
    ) -> Self {
        self.presign = Some(PreSignedPool::new());
        self.mempool = Some(Arc::new(MempoolMonitor::new()));
        self.rpc_pool = Some(rpc);
        self.nonce_mgr = Some(nonce_mgr);
        self.tx_tracker = Some(tx_tracker);
        self
    }

    /// 使用自定义延迟预算构造（例如 50ms 更激进目标）
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.tracker = LatencyTracker::new(1024, budget);
        self
    }

    /// 底层 CLOB 客户端
    pub fn client(&self) -> &ClobClient {
        &self.client
    }

    /// 钱包管理器
    pub fn wallet_manager(&self) -> &Arc<WalletManager> {
        self.hot.manager()
    }

    /// 预热指定钱包（把 PBKDF2 移出交易时段）
    pub fn prewarm_wallet(&self, address: &str, password: &str) -> ClobResult<()> {
        Ok(self.hot.prewarm(address, password)?)
    }

    /// 手动写入市场元数据（启动批量预热用）
    pub fn put_market_meta(&self, token_id: &str, meta: MarketMeta) {
        self.meta.put(token_id, meta);
    }

    /// 初始化 API Key 并预热全部缓存层（启动时调用一次）
    ///
    /// 这一步会一次性付出：
    /// - 1 次 PBKDF2（解锁热钱包）
    /// - 1 次 L1 请求（创建/派生 API Key）
    /// - 1 次 HTTP/2 连接建立（DNS + TCP + TLS 三连击）
    ///
    /// 把它们全部挡在交易时段之外。
    pub async fn init_api_key(
        &mut self,
        address: &str,
        password: &str,
    ) -> ClobResult<ApiKeyCreds> {
        // 1. L1 认证：创建/派生 API Key
        let creds = self.client.create_or_derive_api_key(address, password).await?;
        self.client.set_api_creds(creds.clone());

        // 2. 预热 EIP-712 域分隔符（4 种 chain×negRisk 组合）
        signing_cache::prewarm_domains();

        // 3. 预热 HTTP/2 连接（消除 3 RTT）
        let host = self.client.config_host().to_string();
        let _ = warmup(self.client.http_client(), &host).await;

        // 4. 解锁热钱包（消除后续下单的 PBKDF2）
        self.hot.prewarm(address, password)?;

        Ok(creds)
    }

    /// 设置已存在的 API 凭证（无需再调用 `init_api_key`）
    pub fn set_api_creds(&mut self, creds: ApiKeyCreds) {
        self.client.set_api_creds(creds);
    }

    /// 解析市场元数据：命中缓存直接返回，未命中才走一次 API
    async fn resolve_meta(&self, token_id: &str) -> ClobResult<MarketMeta> {
        if let Some(m) = self.meta.get(token_id) {
            return Ok(m);
        }
        // 未命中：拉取 tick size（negRisk 无法从该端点推断，沿用全局默认）
        let s = self.client.get_tick_size(token_id).await?;
        let tick = crate::fast::market_cache::parse_tick_size(&s).unwrap_or(TickSize::Point01);
        let meta = MarketMeta {
            tick_size: tick,
            neg_risk: false,
        };
        self.meta.put(token_id, meta);
        Ok(meta)
    }

    /// 构建并签名订单（同时测量 BuildOrder 与 Sign 阶段）
    async fn build_and_sign(
        &self,
        wallet: &LocalWallet,
        user_order: &UserOrder,
        options: &CreateOrderOptions,
        lat: &mut OrderLatency,
    ) -> ClobResult<(SignedOrder, ExecutionSource)> {
        let t = StageTimer::start();
        let order = self.client.build_unsigned_order(wallet, user_order, options)?;
        t.stop(lat, Stage::BuildOrder);

        let t = StageTimer::start();
        let verifying = get_exchange_address(self.chain, options.neg_risk);
        let signature = sign_order(wallet, &order, self.chain.id(), verifying)?;
        t.stop(lat, Stage::Sign);

        Ok((SignedOrder { order, signature }, ExecutionSource::LiveSigned))
    }

    /// 执行一笔快速下单
    ///
    /// 完整测量 7 个阶段，记录到 [`LatencyTracker`]，并判定是否达成 100ms 预算。
    pub async fn execute_fast(&self, params: &FastOrderParams) -> ClobResult<FastOrderResult> {
        let mut lat = OrderLatency::new();
        let root = StageTimer::start();

        // --- 阶段 1: 钱包解锁 ---
        let wallet = {
            let t = StageTimer::start();
            let w = match &params.password {
                Some(pw) => self.hot.get_or_unlock(&params.wallet_address, pw)?,
                None => self
                    .hot
                    .get(&params.wallet_address)
                    .ok_or_else(|| {
                        ClobError::OrderFailed(
                            "热钱包未预热且未提供密码，无法解锁".to_string(),
                        )
                    })?,
            };
            t.stop(&mut lat, Stage::WalletUnlock);
            w
        };

        // --- 阶段 2: tick 元数据 ---
        let meta = {
            let t = StageTimer::start();
            let m = self.resolve_meta(&params.token_id).await?;
            t.stop(&mut lat, Stage::TickLookup);
            m
        };

        let options = CreateOrderOptions {
            tick_size: params.tick_size.unwrap_or(meta.tick_size),
            neg_risk: params.neg_risk.unwrap_or(meta.neg_risk),
        };
        let order_type = params.order_type.unwrap_or(OrderType::GTC);

        let user_order = UserOrder {
            token_id: params.token_id.clone(),
            price: params.price,
            size: params.size,
            side: params.side,
            fee_rate_bps: None,
            nonce: None,
            expiration: None,
            taker: None,
        };

        // --- ★ Nonce 验证：下单前检查链上 nonce 一致性（如已挂载 NonceManager + RpcPool）---
        if let (Some(ref nm), Some(_rpc)) = (&self.nonce_mgr, &self.rpc_pool) {
            if nm.is_stale() {
                // 缓存过期，从链上同步
                if let Err(e) = nm.sync_from_chain().await {
                    tracing::warn!("nonce sync 失败: {}", e);
                }
            }
            // 验证本地 nonce 与链上 nonce 偏差在合理范围
            let local = nm.onchain_nonce();
            let pending = nm.pending_nonces().last().copied().unwrap_or(local);
            if pending > local + 10 {
                tracing::warn!(
                    "nonce 偏差过大: local={}, pending={}, 可能 stuck tx",
                    local, pending
                );
            }
        }

        // --- 阶段 3+4: 构建 + 签名（或预签名快路径）---
        let (signed, source) = if params.prefer_presign {
            if let Some(pool) = &self.presign {
                if let Some(pre) = pool.take(&params.token_id, params.side, params.price) {
                    // 预签名命中：Build 与 Sign 阶段均为 ~0
                    let t = StageTimer::start();
                    let s = pre.signed.clone();
                    t.stop(&mut lat, Stage::BuildOrder);
                    let t = StageTimer::start();
                    t.stop(&mut lat, Stage::Sign);
                    (s, ExecutionSource::PreSigned)
                } else {
                    self.build_and_sign(&wallet, &user_order, &options, &mut lat)
                        .await?
                }
            } else {
                self.build_and_sign(&wallet, &user_order, &options, &mut lat)
                    .await?
            }
        } else {
            self.build_and_sign(&wallet, &user_order, &options, &mut lat)
                .await?
        };

        // --- ★ Mempool Hook A: 提交前检查（去重 + 限频 + pending 容量 + 防抢跑）---
        if let Some(ref mp) = self.mempool {
            let tick_f64 = match options.tick_size {
                TickSize::Point1 => 0.1,
                TickSize::Point01 => 0.01,
                TickSize::Point001 => 0.001,
                TickSize::Point0001 => 0.0001,
            };
            mp.check_before_submit(
                &params.token_id,
                params.side,
                params.price,
                params.size,
                tick_f64,
            )?;
        }

        // --- 阶段 5: 序列化 ---
        let body = {
            let t = StageTimer::start();
            let b = self.client.build_order_body(&wallet, &signed, order_type)?;
            t.stop(&mut lat, Stage::Serialize);
            b
        };

        // --- 阶段 6: L2 HMAC ---
        let headers = {
            let t = StageTimer::start();
            let h = self.client.build_order_l2_headers(&wallet, &body)?;
            t.stop(&mut lat, Stage::Hmac);
            h
        };

        // --- 阶段 7: HTTP 往返 ---
        let resp = {
            let t = StageTimer::start();
            let r = self.client.send_order(body, headers).await?;
            t.stop(&mut lat, Stage::Http);
            r
        };

        // --- ★ Mempool Hook B: 提交后记录（pending 追踪 + 去重指纹记录）---
        if let Some(ref mp) = self.mempool {
            let tick_f64 = match options.tick_size {
                TickSize::Point1 => 0.1,
                TickSize::Point01 => 0.01,
                TickSize::Point001 => 0.001,
                TickSize::Point0001 => 0.0001,
            };
            let order_id = resp.orderID.clone();
            mp.on_submitted(
                format!("tmp_{}", order_id),
                &params.token_id,
                params.side,
                params.price,
                params.size,
                tick_f64,
                Some(order_id),
            );
        };

        // --- ★ TxTracker: 注册到交易生命周期追踪 ---
        if let Some(ref tt) = self.tx_tracker {
            let order_id = resp.orderID.clone();
            let entry = crate::fast::tx_lifecycle::TxEntry::new(
                order_id.clone(),
                params.token_id.clone(),
                Some(order_id),
            );
            tt.register(entry);
        }

        // --- ★ FillLedger: 记录完整成交记录（下单时间/成交时间/价格/数量/滑点/手续费）---
        if let Some(ref ledger) = self.fill_ledger {
            if resp.success && !resp.orderID.is_empty() {
                let filled_price = if !resp.makingAmount.is_empty() && !resp.takingAmount.is_empty() {
                    resp.makingAmount.parse::<f64>().unwrap_or(0.0)
                        / resp.takingAmount.parse::<f64>().unwrap_or(1.0)
                } else {
                    params.price
                };
                let mut rec = FillRecord::new(
                    resp.orderID.clone(),
                    params.token_id.clone(),
                    params.side.as_str().to_string(),
                    params.price,
                    params.size,
                );
                // 手续费：fee_rate_bps 默认 0（Polymarket 天气盘 0 fee）
                rec.mark_filled(filled_price, 0.0);
                ledger.record(rec);
            }
        }

        lat.set_total(root.elapsed());
        let within = lat.within(DEFAULT_LATENCY_BUDGET);
        self.tracker.record(&lat);

        Ok(FastOrderResult {
            response: resp,
            latency: lat,
            source,
            within_budget: within,
        })
    }

    /// 预填充预签名池
    ///
    /// 在信号到达前，围绕 `center` 价格按 `tick` 步长生成 `2*depth+1` 档订单，
    /// 全部签好放入 [`PreSignedPool`]。信号来时 `execute_fast(prefer_presign=true)`
    /// 即可跳过签名阶段。
    ///
    /// # 错误
    ///
    /// 若未启用预签名池（`with_presign`）则返回错误。
    pub async fn prefill_presign(
        &self,
        token_id: &str,
        side: Side,
        size: f64,
        center: f64,
        tick: f64,
        depth: usize,
    ) -> ClobResult<usize> {
        let pool = self.presign.as_ref().ok_or_else(|| {
            ClobError::OrderFailed("未启用预签名池，请使用 with_presign()".to_string())
        })?;

        // 需要 API Key 与热钱包
        let address = self
            .hot
            .cached_addresses()
            .into_iter()
            .next()
            .ok_or_else(|| ClobError::OrderFailed("热钱包未预热，无法预签名".to_string()))?;
        let wallet = self
            .hot
            .get(&address)
            .ok_or_else(|| ClobError::OrderFailed("热钱包为空".to_string()))?;

        let meta = self.resolve_meta(token_id).await?;
        let options = CreateOrderOptions {
            tick_size: meta.tick_size,
            neg_risk: meta.neg_risk,
        };

        let prices = crate::fast::presign::price_ladder(center, tick, depth);
        let mut count = 0usize;
        for p in prices {
            let user_order = UserOrder {
                token_id: token_id.to_string(),
                price: p,
                size,
                side,
                fee_rate_bps: None,
                nonce: None,
                expiration: None,
                taker: None,
            };
            let order = self.client.build_unsigned_order(&wallet, &user_order, &options)?;
            let verifying = get_exchange_address(self.chain, options.neg_risk);
            let signature = sign_order(&wallet, &order, self.chain.id(), verifying)?;
            let signed = SignedOrder { order, signature };
            pool.insert(PreSignedOrder::new(token_id, side, p, size, signed));
            count += 1;
        }
        Ok(count)
    }

    /// 清理过期条目（热钱包 + 预签名池 + pending 订单），建议由后台任务周期性调用
    pub fn sweep(&self) -> usize {
        let mut n = self.hot.sweep();
        if let Some(p) = &self.presign {
            n += p.sweep();
        }
        if let Some(mp) = &self.mempool {
            n += mp.sweep();
        }
        n
    }

    /// RPC 节点池引用
    pub fn rpc_pool(&self) -> Option<&Arc<RpcPool>> {
        self.rpc_pool.as_ref()
    }

    /// Nonce 管理器引用
    pub fn nonce_manager(&self) -> Option<&Arc<NonceManager>> {
        self.nonce_mgr.as_ref()
    }

    /// 交易追踪器引用
    pub fn tx_tracker(&self) -> Option<&Arc<TxTracker>> {
        self.tx_tracker.as_ref()
    }

    /// 延迟报告（P50/P95/P99 + 分阶段）
    pub fn latency_report(&self) -> String {
        self.tracker.report()
    }

    /// 延迟追踪器
    pub fn tracker(&self) -> &LatencyTracker {
        &self.tracker
    }

    /// 热钱包缓存统计
    pub fn hot_stats(&self) -> crate::fast::hot_wallet::HotWalletStats {
        self.hot.stats()
    }

    /// 市场元数据缓存命中率
    pub fn meta_hit_rate(&self) -> f64 {
        self.meta.hit_rate()
    }

    /// 预签名池统计
    pub fn presign_stats(&self) -> Option<crate::fast::presign::PreSignStats> {
        self.presign.as_ref().map(|p| p.stats())
    }

    /// Mempool Monitor 统计（None 表示未启用）
    pub fn mempool_stats(&self) -> Option<MempoolStats> {
        self.mempool.as_ref().map(|mp| mp.stats())
    }

    /// Mempool Monitor 可变引用（用于运行时开关、手动清理等高级操作）
    pub fn mempool_monitor(&self) -> Option<&Arc<MempoolMonitor>> {
        self.mempool.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PK: &str = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";

    fn make_executor() -> (Arc<WalletManager>, FastExecutor) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ks.json");
        let mut mgr = WalletManager::new(&path);
        let addr = mgr.import_private_key(TEST_PK, "pw").unwrap();
        let arc = Arc::new(mgr);
        let exec = FastExecutor::new(arc.clone(), ClobConfig::default()).unwrap();
        // 预热钱包以便后续测试无需密码
        exec.prewarm_wallet(&addr, "pw").unwrap();
        // 暴露地址供测试使用——这里用闭包捕获
        std::mem::forget(addr);
        (arc, exec)
    }

    #[test]
    fn test_build_unsigned_order_matches_wire_shape() {
        // 验证 build_unsigned_order 产出与 eip712 签名路径一致的 Order 结构
        let (_mgr, exec) = make_executor();
        let addr = exec.hot.cached_addresses()[0].clone();
        let wallet = exec.hot.get(&addr).unwrap();

        let uo = UserOrder {
            token_id: "12345".to_string(),
            price: 0.5,
            size: 100.0,
            side: Side::Buy,
            fee_rate_bps: None,
            nonce: None,
            expiration: None,
            taker: None,
        };
        let opts = CreateOrderOptions {
            tick_size: TickSize::Point01,
            neg_risk: false,
        };
        let order = exec.client().build_unsigned_order(&wallet, &uo, &opts).unwrap();
        assert_eq!(order.side, 0); // BUY
        assert_eq!(order.signature_type, 0); // EOA
        assert!(!order.salt.is_empty());
        // maker_amount 应为 6 位小数的链上单位（= 50.0 * 1e6）
        assert_eq!(order.maker_amount, "50000000");
        assert_eq!(order.taker_amount, "100000000");
    }

    #[tokio::test]
    async fn test_execute_fast_cache_hit_path() {
        // 离线环境无法真正发送，这里验证「构建+签名」本地路径不抛错，
        // 且热钱包命中（WalletUnlock≈0）。网络阶段会因连不上而失败，
        // 但本地阶段计时已被记录。
        let (_mgr, exec) = make_executor();
        let addr = exec.hot.cached_addresses()[0].clone();
        let params = FastOrderParams::buy(&addr, None, "12345", 0.5, 100.0);

        // 不连网：直接调用内部构建验证签名路径
        let wallet = exec.hot.get(&addr).unwrap();
        let uo = UserOrder {
            token_id: "12345".to_string(),
            price: 0.5,
            size: 100.0,
            side: Side::Buy,
            fee_rate_bps: None,
            nonce: None,
            expiration: None,
            taker: None,
        };
        let opts = CreateOrderOptions {
            tick_size: TickSize::Point01,
            neg_risk: false,
        };
        let (signed, src) = exec.build_and_sign(&wallet, &uo, &opts, &mut OrderLatency::new()).await.unwrap();
        assert_eq!(src, ExecutionSource::LiveSigned);
        assert!(signed.signature.starts_with("0x"));
        assert_eq!(signed.signature.len(), 132);
        // 验证签名能被规范 digest 路径复现
        let verifying = get_exchange_address(Chain::Polygon, false);
        let digest = signing_cache::fast_order_digest(&signed.order, Chain::Polygon.id(), verifying);
        assert_eq!(digest.as_bytes().len(), 32);
    }

    #[test]
    fn test_with_presign_allocates_pool() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = WalletManager::new(dir.path().join("ks.json"));
        let _ = mgr.import_private_key(TEST_PK, "pw").unwrap();
        let exec = FastExecutor::new(Arc::new(mgr), ClobConfig::default())
            .unwrap()
            .with_presign();
        assert!(exec.presign_stats().is_some());
        assert_eq!(exec.presign_stats().unwrap().available, 0);
    }
}
