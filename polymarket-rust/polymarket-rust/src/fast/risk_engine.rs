//! 风控系统 (RiskEngine)
//!
//! Polymarket 天气预测市场量化交易专用风控。覆盖路线图全部 5 项需求 + 补充能力。
//!
//! ## 路线图需求覆盖
//!
//! | # | 需求 | 实现 |
//! |---|------|------|
//! | 1 | 仓位硬限制 | `PositionLimits`: max_per_market / max_global_exposure |
//! | 2 | 时间窗口风控 | `TimeWindowGuard`: 结算前 2h 禁开新仓，只允许平仓 |
//! | 3 | 每日亏损限制 | `DailyLossLimit`: 3 级熔断 Warn→Limit→Halt |
//! | 4 | 预言机争议监控 | `OracleDisputeMonitor`: UMA 结算 vs 气象数据源偏差检测 |
//! | 5 | Gas 费保护 | `GasFeeProtector`: 单笔 Gas 上限，高拥堵时拒绝交易 |
//!
//! ## 补充能力
//!
//! - `DrawdownTracker`: 实时最大回撤 (peak-to-trough)
//! - `ConcentrationChecker`: 单一市场集中度 (portfolio %)
//! - `RiskEngine`: 整合所有检查器，pre_check / post_check / monitor 三阶段
//!
//! ## 集成点
//!
//! 挂在 `FastExecutor.execute_fast()` 中, 与 MempoolMonitor 并行检查:
//!
//! ```text
//! execute_fast()
//!   ├─ WalletUnlock
//!   ├─ TickLookup
//!   ├─ ★ RiskEngine.pre_check()  ← 新增
//!   ├─ Build + Sign
//!   ├─ Mempool Hook A
//!   ├─ HTTP
//!   ├─ Mempool Hook B
//!   └─ ★ RiskEngine.post_check() ← 新增
//! ```

use crate::clob::error::{ClobError, ClobResult};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing;

// ============================================================================
// 常量
// ============================================================================

/// 结算前禁止开仓窗口 (2 小时)
pub const SETTLEMENT_FREEZE_WINDOW: Duration = Duration::from_secs(2 * 3600);

/// 默认单市场最大仓位 (USDC)
pub const DEFAULT_MAX_POSITION_PER_MARKET: f64 = 500.0;

/// 默认全局最大暴露 (USDC)
pub const DEFAULT_MAX_GLOBAL_EXPOSURE: f64 = 2000.0;

/// 默认单市场集中度上限 (portfolio %)
pub const DEFAULT_MAX_CONCENTRATION: f64 = 0.40; // 40%

/// 默认日亏损熔断阈值 (USDC)
pub const DEFAULT_DAILY_LOSS_WARN: f64 = 100.0;
pub const DEFAULT_DAILY_LOSS_LIMIT: f64 = 250.0;
pub const DEFAULT_DAILY_LOSS_HALT: f64 = 500.0;

/// UMA 预言机偏差预警阈值 (价格差 > 5%)
pub const DEFAULT_ORACLE_DEVIATION_THRESHOLD: f64 = 0.05;

/// Polygon 默认 Gas 上限 (gwei)
pub const DEFAULT_MAX_GAS_GWEI: u64 = 500;

/// 默认最大回撤告警阈值
pub const DEFAULT_MAX_DRAWDOWN_WARN: f64 = 0.10; // 10%

// ============================================================================
// 熔断级别
// ============================================================================

/// 熔断级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BreachLevel {
    /// 正常
    Normal = 0,
    /// 警告：记录日志，继续交易
    Warning = 1,
    /// 限制：禁止开新仓，允许平仓
    Restricted = 2,
    /// 熔断：全部交易暂停
    Halted = 3,
}

impl std::fmt::Display for BreachLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BreachLevel::Normal => write!(f, "normal"),
            BreachLevel::Warning => write!(f, "warning"),
            BreachLevel::Restricted => write!(f, "restricted"),
            BreachLevel::Halted => write!(f, "halted"),
        }
    }
}

// ============================================================================
// RiskConfig
// ============================================================================

/// 风控配置
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// 单市场最大仓位 (USDC)
    pub max_position_per_market: f64,
    /// 全局最大暴露 (USDC)
    pub max_global_exposure: f64,
    /// 单市场集中度上限 (0~1)
    pub max_concentration: f64,
    /// 日亏损三级阈值 (USDC)
    pub daily_loss_warn: f64,
    pub daily_loss_limit: f64,
    pub daily_loss_halt: f64,
    /// 最大回撤告警阈值 (0~1)
    pub max_drawdown_warn: f64,
    /// 结算前冻结窗口
    pub settlement_freeze_window: Duration,
    /// UMA 预言机偏差阈值 (0~1)
    pub oracle_deviation_threshold: f64,
    /// Gas 上限 (gwei)
    pub max_gas_gwei: u64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_per_market: DEFAULT_MAX_POSITION_PER_MARKET,
            max_global_exposure: DEFAULT_MAX_GLOBAL_EXPOSURE,
            max_concentration: DEFAULT_MAX_CONCENTRATION,
            daily_loss_warn: DEFAULT_DAILY_LOSS_WARN,
            daily_loss_limit: DEFAULT_DAILY_LOSS_LIMIT,
            daily_loss_halt: DEFAULT_DAILY_LOSS_HALT,
            max_drawdown_warn: DEFAULT_MAX_DRAWDOWN_WARN,
            settlement_freeze_window: SETTLEMENT_FREEZE_WINDOW,
            oracle_deviation_threshold: DEFAULT_ORACLE_DEVIATION_THRESHOLD,
            max_gas_gwei: DEFAULT_MAX_GAS_GWEI,
        }
    }
}

// ============================================================================
// PositionLimits — 仓位硬限制
// ============================================================================

/// 仓位硬限制检查器
///
/// 路线图 #1: 单市场最大仓位 + 全局最大暴露
pub struct PositionLimits {
    config: RiskConfig,
    /// 当前持仓 {token_id: (quantity, notional_usdc)}
    positions: RwLock<HashMap<String, (f64, f64)>>,
}

impl PositionLimits {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            positions: RwLock::new(HashMap::new()),
        }
    }

    /// 预检：新开仓是否超限
    ///
    /// `side`: Buy = 增加暴露, Sell = 减少暴露
    /// `notional`: 名义价值 (USDC)
    pub fn pre_check(
        &self,
        token_id: &str,
        side: crate::types::enums::Side,
        notional: f64,
    ) -> ClobResult<()> {
        let positions = self.positions.read().unwrap();
        let current_pos = positions.get(token_id).map(|(_, v)| *v).unwrap_or(0.0);

        if matches!(side, crate::types::enums::Side::Buy) {
            // 新增仓位: 检查单市场上限
            let new_market_pos = current_pos + notional;
            if new_market_pos > self.config.max_position_per_market {
                return Err(ClobError::OrderFailed(format!(
                    "仓位硬限制: {} 市场仓位 {:.2} > 上限 {:.2} USDC",
                    token_id, new_market_pos, self.config.max_position_per_market
                )));
            }

            // 检查全局暴露
            let current_global: f64 = positions.values().map(|(_, v)| *v).sum();
            let new_global = current_global + notional;
            if new_global > self.config.max_global_exposure {
                return Err(ClobError::OrderFailed(format!(
                    "全局暴露限制: {:.2} > 上限 {:.2} USDC",
                    new_global, self.config.max_global_exposure
                )));
            }
        }
        Ok(())
    }

    /// 后检：更新持仓
    pub fn post_check(
        &self,
        token_id: &str,
        side: crate::types::enums::Side,
        qty: f64,
        notional: f64,
    ) {
        let mut positions = self.positions.write().unwrap();
        let entry = positions.entry(token_id.to_string()).or_insert((0.0, 0.0));
        match side {
            crate::types::enums::Side::Buy => {
                entry.0 += qty;
                entry.1 += notional;
            }
            crate::types::enums::Side::Sell => {
                entry.0 = (entry.0 - qty).max(0.0);
                entry.1 = (entry.1 - notional).max(0.0);
            }
        }
    }

    /// 当前全局暴露
    pub fn global_exposure(&self) -> f64 {
        self.positions
            .read()
            .unwrap()
            .values()
            .map(|(_, v)| *v)
            .sum()
    }

    /// 获取指定市场仓位
    pub fn market_position(&self, token_id: &str) -> Option<(f64, f64)> {
        self.positions.read().unwrap().get(token_id).copied()
    }
    /// 返回当前所有市场的仓位数量快照。
    ///
    /// key: token_id
    /// value: token quantity
    pub fn snapshot_positions(&self) -> HashMap<String, f64> {
        self.positions
            .read()
            .unwrap()
            .iter()
            .map(|(token_id, (quantity, _notional_usdc))| (token_id.clone(), *quantity))
            .collect()
    }
}

// ============================================================================
// TimeWindowGuard — 时间窗口风控
// ============================================================================

/// 结算时间窗口守卫
///
/// 路线图 #2: 事件结算前 2 小时自动禁止新开仓。
///
/// Polymarket 预测市场在结算前流动性急剧下降，
/// 价格可能被操纵，新开仓风险极高。
/// 此窗口内只允许平仓（Sell），禁止建仓（Buy）。
///
/// 每个市场的结算时间需要外部注入（从 Polymarket API 获取 `endTime`）。
pub struct TimeWindowGuard {
    freeze_window: Duration,
    /// {token_id: settlement_unix_timestamp}
    settlements: RwLock<HashMap<String, u64>>,
}

impl TimeWindowGuard {
    pub fn new(freeze_window: Duration) -> Self {
        Self {
            freeze_window,
            settlements: RwLock::new(HashMap::new()),
        }
    }

    /// 注册市场的结算时间
    pub fn set_settlement(&self, token_id: &str, unix_timestamp: u64) {
        self.settlements
            .write()
            .unwrap()
            .insert(token_id.to_string(), unix_timestamp);
    }

    /// 批量注册结算时间
    pub fn set_settlements(&self, map: HashMap<String, u64>) {
        let mut s = self.settlements.write().unwrap();
        s.extend(map);
    }

    /// 检查是否在结算冻结窗口内
    ///
    /// 返回 Err 表示在窗口内且是开仓方向（Buy），应拒绝。
    /// 返回 Ok 表示安全或方向是平仓（Sell）。
    pub fn pre_check(&self, token_id: &str, side: crate::types::enums::Side) -> ClobResult<()> {
        // Sell 方向（平仓）始终允许
        if matches!(side, crate::types::enums::Side::Sell) {
            return Ok(());
        }

        let settlements = self.settlements.read().unwrap();
        if let Some(&settlement_ts) = settlements.get(token_id) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let time_until = settlement_ts.saturating_sub(now);

            if time_until <= self.freeze_window.as_secs() {
                let mins = time_until / 60;
                return Err(ClobError::OrderFailed(format!(
                    "结算窗口保护: {} 距结算剩余 {} 分钟，禁止新开仓",
                    token_id, mins
                )));
            }
        }
        Ok(())
    }

    /// 列出即将结算的市场
    pub fn upcoming_settlements(&self) -> Vec<(String, u64)> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let threshold = now + self.freeze_window.as_secs();
        self.settlements
            .read()
            .unwrap()
            .iter()
            .filter(|(_, &ts)| ts <= threshold)
            .map(|(tid, ts)| (tid.clone(), *ts))
            .collect()
    }
}

// ============================================================================
// DailyLossLimit — 每日亏损限制
// ============================================================================

/// 日亏损追踪与熔断
///
/// 路线图 #3: 达到阈值后自动切断交易权限。
/// 三级熔断: Warning(日志) → Restricted(禁开仓) → Halted(全部停止)
pub struct DailyLossLimit {
    warn: f64,
    limit: f64,
    halt: f64,
    /// 当日累计盈亏
    daily_pnl: RwLock<f64>,
    /// 当日峰值权益 (用于回撤计算)
    daily_peak: RwLock<f64>,
    /// 当前熔断级别
    breach_level: RwLock<BreachLevel>,
    /// 上次重置日 (YYYY-MM-DD)
    last_reset: RwLock<String>,
    /// 熔断日志
    breach_count: AtomicU64,
}

impl DailyLossLimit {
    pub fn new(config: &RiskConfig) -> Self {
        let today = date_util::today_utc();
        Self {
            warn: config.daily_loss_warn,
            limit: config.daily_loss_limit,
            halt: config.daily_loss_halt,
            daily_pnl: RwLock::new(0.0),
            daily_peak: RwLock::new(0.0),
            breach_level: RwLock::new(BreachLevel::Normal),
            last_reset: RwLock::new(today),
            breach_count: AtomicU64::new(0),
        }
    }

    /// 每日重置检测
    fn maybe_reset(&self) {
        let today = date_util::today_utc();
        let mut last = self.last_reset.write().unwrap();
        if *last != today {
            tracing::info!("risk_engine: 日亏损追踪重置 ({} → {})", *last, today);
            *self.daily_pnl.write().unwrap() = 0.0;
            *self.daily_peak.write().unwrap() = 0.0;
            *self.breach_level.write().unwrap() = BreachLevel::Normal;
            *last = today;
        }
    }

    /// 上报交易盈亏并检查熔断
    pub fn report_pnl(&self, pnl: f64) -> BreachLevel {
        self.maybe_reset();

        let mut daily = self.daily_pnl.write().unwrap();
        *daily += pnl;

        // 更新峰值
        let mut peak = self.daily_peak.write().unwrap();
        if *daily > *peak {
            *peak = *daily;
        }

        // 判断熔断级别
        let loss = -(*daily); // 负表示亏损
        let new_level = if loss >= self.halt {
            BreachLevel::Halted
        } else if loss >= self.limit {
            BreachLevel::Restricted
        } else if loss >= self.warn {
            BreachLevel::Warning
        } else {
            BreachLevel::Normal
        };

        let mut level = self.breach_level.write().unwrap();
        if new_level > *level {
            *level = new_level;
            self.breach_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                "risk_engine: 日亏损触发 {} 级熔断 (loss={:.2} USDC, 阈值 warn={} limit={} halt={})",
                new_level,
                loss,
                self.warn,
                self.limit,
                self.halt,
            );
        }

        *level
    }

    /// 获取当前熔断级别
    pub fn current_level(&self) -> BreachLevel {
        *self.breach_level.read().unwrap()
    }

    /// 当前日盈亏
    pub fn daily_pnl(&self) -> f64 {
        *self.daily_pnl.read().unwrap()
    }

    /// 日最大回撤 (从峰值)
    pub fn daily_drawdown(&self) -> f64 {
        let daily = *self.daily_pnl.read().unwrap();
        let peak = *self.daily_peak.read().unwrap();
        if peak <= 0.0 {
            return 0.0;
        }
        (peak - daily) / peak
    }
}

// ============================================================================
// OracleDisputeMonitor — UMA 预言机争议监控
// ============================================================================

/// 预言机结算争议检测结果
#[derive(Debug, Clone)]
pub struct OracleStatus {
    pub token_id: String,
    /// UMA 结算价格 (0~1 for binary markets)
    pub uma_price: Option<f64>,
    /// 气象数据源推断结果 (0~1)
    pub weather_price: Option<f64>,
    /// 价格偏差
    pub deviation: Option<f64>,
    /// 是否触发预警
    pub alert: bool,
    /// 建议：是否发起争议
    pub suggest_dispute: bool,
}

/// 预言机争议监控器
///
/// 路线图 #4: UMA 结算异常时自动预警并评估是否发起争议。
///
/// Polymarket 使用 UMA Optimistic Oracle 结算二元市场。
/// 如果 UMA 返回结果与气象数据源严重不符, 本项目可发起争议
/// 以纠正结算结果——这是量化交易在 Polymarket 上的独特优势。
pub struct OracleDisputeMonitor {
    deviation_threshold: f64,
    /// 缓存的气象推断结果 {token_id: expected_outcome}
    expected_outcomes: RwLock<HashMap<String, f64>>,
    /// UMA 结算结果 {token_id: (price, timestamp)}
    uma_results: RwLock<HashMap<String, (f64, u64)>>,
}

impl OracleDisputeMonitor {
    pub fn new(deviation_threshold: f64) -> Self {
        Self {
            deviation_threshold,
            expected_outcomes: RwLock::new(HashMap::new()),
            uma_results: RwLock::new(HashMap::new()),
        }
    }

    /// 注入气象数据源的预期结果 (由 Python 策略引擎推送)
    pub fn set_expected_outcome(&self, token_id: &str, expected: f64) {
        self.expected_outcomes
            .write()
            .unwrap()
            .insert(token_id.to_string(), expected);
    }

    /// 注入 UMA 结算结果 (由链上事件或 API 获取)
    pub fn set_uma_result(&self, token_id: &str, price: f64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.uma_results
            .write()
            .unwrap()
            .insert(token_id.to_string(), (price, now));
    }

    /// 检测单个市场的预言机偏差
    pub fn check(&self, token_id: &str) -> OracleStatus {
        let outcomes = self.expected_outcomes.read().unwrap();
        let uma = self.uma_results.read().unwrap();

        let expected = outcomes.get(token_id).copied();
        let uma_price = uma.get(token_id).map(|(p, _)| *p);

        let deviation = match (expected, uma_price) {
            (Some(e), Some(u)) if u > 0.0 || e > 0.0 => Some((e - u).abs()),
            _ => None,
        };

        let alert = deviation.unwrap_or(0.0) > self.deviation_threshold;
        let suggest_dispute = alert && expected.unwrap_or(0.0).abs() > 0.02;

        if alert {
            tracing::warn!(
                "risk_engine: 预言机偏差预警 {} deviation={:.4} expected={:.4} uma={:.4} suggest_dispute={}",
                token_id,
                deviation.unwrap_or(0.0),
                expected.unwrap_or(0.0),
                uma_price.unwrap_or(0.0),
                suggest_dispute,
            );
        }

        OracleStatus {
            token_id: token_id.to_string(),
            uma_price,
            weather_price: expected,
            deviation,
            alert,
            suggest_dispute,
        }
    }

    /// 检测所有已知市场的预言机偏差
    pub fn check_all(&self) -> Vec<OracleStatus> {
        let outcomes = self.expected_outcomes.read().unwrap();
        outcomes.keys().map(|tid| self.check(tid)).collect()
    }
}

// ============================================================================
// GasFeeProtector — Gas 费保护
// ============================================================================

/// Gas 费保护器
///
/// 路线图 #5: 单笔 Gas 上限，防止极端拥堵时的高额手续费。
///
/// Polygon 上 Gas 通常 < 100 gwei，但网络拥堵时可能飙升至数千 gwei。
/// 此保护器拒绝 Gas 超过阈值的交易。
pub struct GasFeeProtector {
    max_gas_gwei: AtomicU64,
    /// 统计：拒绝次数
    rejected: AtomicU64,
}

impl GasFeeProtector {
    pub fn new(max_gas_gwei: u64) -> Self {
        Self {
            max_gas_gwei: AtomicU64::new(max_gas_gwei),
            rejected: AtomicU64::new(0),
        }
    }

    /// 检查 Gas 价格是否在安全范围内
    ///
    /// 如果超过上限，返回 Err 并记录。
    pub fn pre_check(&self, gas_gwei: u64) -> ClobResult<()> {
        let max = self.max_gas_gwei.load(Ordering::Acquire);
        if gas_gwei > max {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(ClobError::OrderFailed(format!(
                "Gas 费保护: 当前 Gas {} gwei > 上限 {} gwei, 拒绝交易",
                gas_gwei, max
            )));
        }
        Ok(())
    }

    /// 更新上限（动态调整）
    pub fn set_max_gas(&self, gwei: u64) {
        self.max_gas_gwei.store(gwei, Ordering::Release);
    }

    /// 获取上限
    pub fn max_gas(&self) -> u64 {
        self.max_gas_gwei.load(Ordering::Acquire)
    }

    /// 拒绝次数
    pub fn rejected_count(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
}

// ============================================================================
// DrawdownTracker — 最大回撤追踪
// ============================================================================

/// 实时最大回撤追踪器
///
/// 补充能力: 追踪 portfolio 权益的峰值→谷值最大跌幅。
/// 当回撤超过阈值时发出告警。
pub struct DrawdownTracker {
    /// 历史最高权益
    peak_equity: RwLock<f64>,
    /// 当前最大回撤
    max_drawdown: RwLock<f64>,
    /// 告警阈值
    warn_threshold: f64,
}

impl DrawdownTracker {
    pub fn new(warn_threshold: f64) -> Self {
        Self {
            peak_equity: RwLock::new(0.0),
            max_drawdown: RwLock::new(0.0),
            warn_threshold,
        }
    }

    /// 更新权益并计算回撤
    pub fn update(&self, equity: f64) {
        let mut peak = self.peak_equity.write().unwrap();

        if equity > *peak {
            *peak = equity;
        }

        let drawdown = if *peak > 0.0 {
            (*peak - equity) / *peak
        } else {
            0.0
        };
        let mut max_dd = self.max_drawdown.write().unwrap();
        if drawdown > *max_dd {
            *max_dd = drawdown;
        }

        if drawdown > self.warn_threshold && drawdown > *max_dd {
            tracing::warn!(
                "risk_engine: 回撤告警 {:.2}% (max={:.2}%, peak={:.2})",
                drawdown * 100.0,
                *max_dd * 100.0,
                *peak,
            );
        }
    }

    pub fn current_drawdown(&self) -> f64 {
        *self.max_drawdown.read().unwrap()
    }

    pub fn peak_equity(&self) -> f64 {
        *self.peak_equity.read().unwrap()
    }
}

// ============================================================================
// ConcentrationChecker — 集中度检查
// ============================================================================

/// 单一市场集中度检查器
///
/// 补充能力: 确保没有单一市场占比超过 portfolio 的 X%
pub struct ConcentrationChecker {
    max_concentration: f64,
}

impl ConcentrationChecker {
    pub fn new(max_concentration: f64) -> Self {
        Self { max_concentration }
    }

    /// 检查单个市场是否集中度过高
    pub fn check(&self, market_exposure: f64, total_exposure: f64) -> ClobResult<()> {
        if total_exposure <= 0.0 {
            return Ok(());
        }
        let concentration = market_exposure / total_exposure;
        if concentration > self.max_concentration {
            return Err(ClobError::OrderFailed(format!(
                "集中度限制: {:.1}% > 上限 {:.1}%",
                concentration * 100.0,
                self.max_concentration * 100.0,
            )));
        }
        Ok(())
    }
}

// ============================================================================
// RiskEngine — 整合层
// ============================================================================

/// 风控引擎
///
/// 整合所有风控子模块，提供两个核心钩子：
/// - `pre_check()`: 下单前全面检查
/// - `post_check()`: 下单后更新状态
pub struct RiskEngine {
    config: RiskConfig,
    position_limits: PositionLimits,
    time_window: TimeWindowGuard,
    daily_loss: DailyLossLimit,
    oracle_monitor: OracleDisputeMonitor,
    gas_protector: GasFeeProtector,
    drawdown_tracker: DrawdownTracker,
    concentration_checker: ConcentrationChecker,
}

impl RiskEngine {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            position_limits: PositionLimits::new(config.clone()),
            time_window: TimeWindowGuard::new(config.settlement_freeze_window),
            daily_loss: DailyLossLimit::new(&config),
            oracle_monitor: OracleDisputeMonitor::new(config.oracle_deviation_threshold),
            gas_protector: GasFeeProtector::new(config.max_gas_gwei),
            drawdown_tracker: DrawdownTracker::new(config.max_drawdown_warn),
            concentration_checker: ConcentrationChecker::new(config.max_concentration),
            config,
        }
    }

    // ========================================================================
    // 钩子 A: 下单前预检 (pre_check)
    // ========================================================================

    /// 下单前全面风控检查
    ///
    /// 返回 `Ok(())` 表示可以下单，`Err` 表示被某个检查拦截。
    pub fn pre_check(
        &self,
        token_id: &str,
        side: crate::types::enums::Side,
        notional: f64,
        qty: f64,
        gas_gwei: u64,
    ) -> ClobResult<()> {
        // 0. 熔断检查 (最高优先级)
        let level = self.daily_loss.current_level();
        match level {
            BreachLevel::Halted => {
                return Err(ClobError::OrderFailed(
                    "风控熔断: 日亏损触及 HALT 阈值，全部交易暂停".to_string(),
                ));
            }
            BreachLevel::Restricted => {
                // Restricted 只允许平仓
                if matches!(side, crate::types::enums::Side::Buy) {
                    return Err(ClobError::OrderFailed(
                        "风控限制: 日亏损触及 LIMIT 阈值，仅允许平仓".to_string(),
                    ));
                }
            }
            BreachLevel::Warning => {
                tracing::warn!("risk_engine: 日亏损达到 WARNING 级别，继续交易但需关注");
            }
            BreachLevel::Normal => {}
        }

        // 1. Gas 费检查
        self.gas_protector.pre_check(gas_gwei)?;

        // 2. 时间窗口检查 (结算前 2h)
        self.time_window.pre_check(token_id, side)?;

        // 3. 仓位硬限制
        self.position_limits.pre_check(token_id, side, notional)?;

        // 4. 集中度检查 (仅在总暴露 > 2 倍单市场上限时生效，避免空仓第一笔被拦截)
        let market_pos = self
            .position_limits
            .market_position(token_id)
            .map(|(_, v)| v)
            .unwrap_or(0.0);
        let new_market = if matches!(side, crate::types::enums::Side::Buy) {
            market_pos + notional
        } else {
            (market_pos - notional).max(0.0)
        };
        let total = self.position_limits.global_exposure();
        let future_total = total + notional;
        // 仅当 portfolio 已经有一定规模时才检查集中度
        if future_total > self.config.max_position_per_market * 2.0 {
            self.concentration_checker.check(new_market, future_total)?;
        }

        Ok(())
    }

    // ========================================================================
    // 钩子 B: 下单后更新 (post_check)
    // ========================================================================

    /// 下单后更新风控状态
    ///
    /// `pnl`: 此笔交易的盈亏（正=盈利，负=亏损）
    pub fn post_check(
        &self,
        token_id: &str,
        side: crate::types::enums::Side,
        qty: f64,
        notional: f64,
        pnl: f64,
        equity: f64,
    ) {
        // 更新持仓
        self.position_limits
            .post_check(token_id, side, qty, notional);

        // 更新日盈亏和熔断
        let level = self.daily_loss.report_pnl(pnl);
        if level >= BreachLevel::Restricted {
            tracing::warn!(
                "risk_engine: 触发熔断 level={} pnl={:.2} daily={:.2}",
                level,
                pnl,
                self.daily_loss.daily_pnl(),
            );
        }

        // 更新回撤
        self.drawdown_tracker.update(equity);
    }
    // ========================================================================
    // 钩子 C: 真实成交后更新持仓
    // ========================================================================
    /// 真实成交后更新 RiskEngine 的仓位暴露。
    ///
    /// 与 post_check() 不同：
    /// - 只更新 PositionLimits
    /// - 不修改日 PnL
    /// - 不修改 Equity / Drawdown
    ///
    /// 用于实时成交路径，避免在缺少可靠 PnL / Equity 数据时
    /// 错误污染风控状态。
    pub fn on_fill(
        &self,
        token_id: &str,
        side: crate::types::enums::Side,
        qty: f64,
        notional: f64,
    ) {
        self.position_limits
            .post_check(token_id, side, qty, notional);
    }
    // ========================================================================
    // 辅助方法
    // ========================================================================

    /// 设置市场结算时间
    pub fn set_settlement(&self, token_id: &str, unix_ts: u64) {
        self.time_window.set_settlement(token_id, unix_ts);
    }

    /// 批量设置结算时间
    pub fn set_settlements(&self, settlements: HashMap<String, u64>) {
        self.time_window.set_settlements(settlements);
    }

    /// 注入气象预期结果 (由 Python 策略引擎推送)
    pub fn set_expected_outcome(&self, token_id: &str, expected: f64) {
        self.oracle_monitor.set_expected_outcome(token_id, expected);
    }

    /// 注入 UMA 结算结果
    pub fn set_uma_result(&self, token_id: &str, price: f64) {
        self.oracle_monitor.set_uma_result(token_id, price);
    }

    /// 检测预言机偏差
    pub fn check_oracle(&self, token_id: &str) -> OracleStatus {
        self.oracle_monitor.check(token_id)
    }

    /// 检查所有市场的预言机偏差
    pub fn check_oracle_all(&self) -> Vec<OracleStatus> {
        self.oracle_monitor.check_all()
    }

    /// 更新 Gas 上限
    pub fn set_max_gas(&self, gwei: u64) {
        self.gas_protector.set_max_gas(gwei);
    }

    /// 完整风控状态摘要
    pub fn snapshot(&self) -> RiskSnapshot {
        RiskSnapshot {
            breach_level: self.daily_loss.current_level(),
            daily_pnl: self.daily_loss.daily_pnl(),
            daily_drawdown: self.daily_loss.daily_drawdown(),
            global_exposure: self.position_limits.global_exposure(),
            max_drawdown: self.drawdown_tracker.current_drawdown(),
            peak_equity: self.drawdown_tracker.peak_equity(),
            gas_rejected: self.gas_protector.rejected_count(),
            upcoming_settlements: self.time_window.upcoming_settlements(),
            oracle_alerts: self
                .oracle_monitor
                .check_all()
                .into_iter()
                .filter(|s| s.alert)
                .count(),
        }
    }
}

/// 风控状态快照
#[derive(Debug, Clone)]
pub struct RiskSnapshot {
    pub breach_level: BreachLevel,
    pub daily_pnl: f64,
    pub daily_drawdown: f64,
    pub global_exposure: f64,
    pub max_drawdown: f64,
    pub peak_equity: f64,
    pub gas_rejected: u64,
    pub upcoming_settlements: Vec<(String, u64)>,
    pub oracle_alerts: usize,
}

// ============================================================================
// chrono dependency check — use a minimal date helper
// ============================================================================

mod date_util {
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Returns today's date as "YYYY-MM-DD" string (UTC)
    pub fn today_utc() -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let days = secs / 86400;
        // Days since 1970-01-01
        let (y, m, d) = civil_from_days(days as i64 + 719468);
        format!("{:04}-{:02}-{:02}", y, m, d)
    }

    fn civil_from_days(days: i64) -> (i64, u32, u32) {
        let z = days + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u32;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (y + (m as i64 <= 2) as i64, m, d)
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::enums::Side;

    fn make_engine() -> RiskEngine {
        RiskEngine::new(RiskConfig::default())
    }

    // ========== PositionLimits ==========

    #[test]
    fn test_position_limit_per_market() {
        let engine = make_engine();
        // 开仓 400 USDC (上限 500)
        assert!(engine
            .pre_check("market-1", Side::Buy, 400.0, 400.0, 50)
            .is_ok());
        engine.post_check("market-1", Side::Buy, 400.0, 400.0, 0.0, 10000.0);
        // 再开 200 → 600 > 500 上限
        assert!(engine
            .pre_check("market-1", Side::Buy, 200.0, 200.0, 50)
            .is_err());
    }

    #[test]
    fn test_position_limit_global() {
        let mut cfg = RiskConfig::default();
        cfg.max_position_per_market = 10000.0; // 放宽单市场限制
        cfg.max_global_exposure = 1000.0;
        let engine = RiskEngine::new(cfg);
        assert!(engine.pre_check("m1", Side::Buy, 600.0, 600.0, 50).is_ok());
        engine.post_check("m1", Side::Buy, 600.0, 600.0, 0.0, 10000.0);
        assert!(engine.pre_check("m2", Side::Buy, 500.0, 500.0, 50).is_err()); // 600+500 > 1000
    }

    #[test]
    fn test_sell_reduces_exposure() {
        let engine = make_engine();
        engine.post_check("m1", Side::Buy, 400.0, 400.0, 0.0, 10000.0);
        // Sell 应该始终通过
        assert!(engine.pre_check("m1", Side::Sell, 100.0, 100.0, 50).is_ok());
    }

    // ========== TimeWindowGuard ==========

    #[test]
    fn test_settlement_freeze_new_buy() {
        let guard = TimeWindowGuard::new(Duration::from_secs(3600)); // 1h
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 30 分钟后结算 → 在冻结窗口内
        guard.set_settlement("near", now + 1800);
        assert!(guard.pre_check("near", Side::Buy).is_err());
    }

    #[test]
    fn test_settlement_freeze_sell_ok() {
        let guard = TimeWindowGuard::new(Duration::from_secs(3600));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        guard.set_settlement("near", now + 600); // 10 分钟后结算
                                                 // Sell 始终允许
        assert!(guard.pre_check("near", Side::Sell).is_ok());
    }

    #[test]
    fn test_settlement_far_future_ok() {
        let guard = TimeWindowGuard::new(Duration::from_secs(3600));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        guard.set_settlement("far", now + 86400 * 7); // 7 天后
        assert!(guard.pre_check("far", Side::Buy).is_ok());
    }

    // ========== DailyLossLimit ==========

    #[test]
    fn test_daily_loss_normal() {
        let cfg = RiskConfig::default();
        let dll = DailyLossLimit::new(&cfg);
        let level = dll.report_pnl(-50.0); // 亏 50
        assert_eq!(level, BreachLevel::Normal);
    }

    #[test]
    fn test_daily_loss_warning() {
        let mut cfg = RiskConfig::default();
        cfg.daily_loss_warn = 50.0;
        let dll = DailyLossLimit::new(&cfg);
        let level = dll.report_pnl(-75.0);
        assert_eq!(level, BreachLevel::Warning);
    }

    #[test]
    fn test_daily_loss_halted() {
        let mut cfg = RiskConfig::default();
        cfg.daily_loss_halt = 100.0;
        let dll = DailyLossLimit::new(&cfg);
        let level = dll.report_pnl(-150.0);
        assert_eq!(level, BreachLevel::Halted);
    }

    #[test]
    fn test_daily_loss_progression() {
        let mut cfg = RiskConfig::default();
        cfg.daily_loss_warn = 50.0;
        cfg.daily_loss_limit = 150.0;
        cfg.daily_loss_halt = 300.0;
        let dll = DailyLossLimit::new(&cfg);
        assert_eq!(dll.report_pnl(-60.0), BreachLevel::Warning);
        assert_eq!(dll.report_pnl(-100.0), BreachLevel::Restricted);
        assert_eq!(dll.report_pnl(-200.0), BreachLevel::Halted);
    }

    // ========== GasFeeProtector ==========

    #[test]
    fn test_gas_ok() {
        let gp = GasFeeProtector::new(500);
        assert!(gp.pre_check(100).is_ok());
    }

    #[test]
    fn test_gas_blocked() {
        let gp = GasFeeProtector::new(500);
        assert!(gp.pre_check(800).is_err());
        assert_eq!(gp.rejected_count(), 1);
    }

    // ========== DrawdownTracker ==========

    #[test]
    fn test_drawdown_tracking() {
        let dt = DrawdownTracker::new(0.05);
        dt.update(10000.0); // peak=10000
        dt.update(9500.0); // dd=5%
        dt.update(9000.0); // dd=10%
        dt.update(10500.0); // new peak
        let dd = dt.current_drawdown();
        assert!(dd >= 0.05);
    }

    // ========== ConcentrationChecker ==========

    #[test]
    fn test_concentration_ok() {
        let cc = ConcentrationChecker::new(0.5);
        assert!(cc.check(400.0, 1000.0).is_ok()); // 40%
    }

    #[test]
    fn test_concentration_blocked() {
        let cc = ConcentrationChecker::new(0.3);
        assert!(cc.check(400.0, 1000.0).is_err()); // 40% > 30%
    }

    // ========== RiskEngine 集成 ==========

    #[test]
    fn test_engine_pre_check_all_pass() {
        let mut cfg = RiskConfig::default();
        cfg.max_position_per_market = 10000.0;
        let engine = RiskEngine::new(cfg);
        assert!(engine.pre_check("t", Side::Buy, 100.0, 100.0, 50).is_ok());
    }

    #[test]
    fn test_engine_pre_check_halted() {
        let mut cfg = RiskConfig::default();
        cfg.daily_loss_halt = 1.0; // 极低阈值，一亏就熔断
        let engine = RiskEngine::new(cfg);
        engine.post_check("t", Side::Buy, 100.0, 100.0, -50.0, 10000.0);
        assert!(engine.pre_check("t", Side::Buy, 100.0, 100.0, 50).is_err());
    }

    #[test]
    fn test_engine_snapshot() {
        let engine = make_engine();
        let snap = engine.snapshot();
        assert_eq!(snap.breach_level, BreachLevel::Normal);
        assert_eq!(snap.daily_pnl, 0.0);
    }

    #[test]
    fn test_oracle_status() {
        let engine = make_engine();
        engine.set_expected_outcome("t-1", 0.52);
        engine.set_uma_result("t-1", 0.50);
        let status = engine.check_oracle("t-1");
        assert!(!status.alert); // 0.02 偏差 < 0.05 阈值

        engine.set_uma_result("t-1", 0.40);
        let status2 = engine.check_oracle("t-1");
        assert!(status2.alert); // 0.12 > 0.05
        assert!(status2.suggest_dispute); // 大偏差 + expected > 0.02
    }

    #[test]
    fn test_date_util_today_format() {
        let today = date_util::today_utc();
        assert_eq!(today.len(), 10);
        assert!(today.chars().nth(4) == Some('-'));
        assert!(today.chars().nth(7) == Some('-'));
    }
}
