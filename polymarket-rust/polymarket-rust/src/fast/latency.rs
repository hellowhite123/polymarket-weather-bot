//! 延迟埋点 —— 分阶段计时与分位数统计
//!
//! 「全流程 < 100ms」这种目标，不测就是空话。本模块把下单链路切成可观测
//! 的阶段，逐笔记录纳秒级耗时，并给出 P50 / P95 / P99。
//!
//! ```text
//! ┌──────────┬──────────┬───────┬──────┬──────┬───────────┬──────┐
//! │ unlock   │ tick     │ build │ sign │ hmac │ serialize │ http │
//! └──────────┴──────────┴───────┴──────┴──────┴───────────┴──────┘
//!  热钱包后    缓存后
//!  ≈0         ≈0         ~µs     ~µs    ~µs    ~µs         主要成本
//! ```
//!
//! 优化完成后，本地计算部分应在 **1ms 以内**，剩余预算全部留给网络往返。
//! 因此若 P99 超标，看 `http` 阶段即可定位是网络问题还是代码问题。

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// 下单链路的阶段划分
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// 钱包解锁（热钱包命中时≈0）
    WalletUnlock,
    /// tickSize 查询（缓存命中时≈0）
    TickLookup,
    /// 订单金额换算与结构构建
    BuildOrder,
    /// EIP-712 签名（keccak256 + secp256k1）
    Sign,
    /// L2 认证 HMAC-SHA256
    Hmac,
    /// 请求体 JSON 序列化
    Serialize,
    /// HTTP 往返
    Http,
}

impl Stage {
    /// 所有阶段（用于遍历输出）
    pub const ALL: [Stage; 7] = [
        Stage::WalletUnlock,
        Stage::TickLookup,
        Stage::BuildOrder,
        Stage::Sign,
        Stage::Hmac,
        Stage::Serialize,
        Stage::Http,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::WalletUnlock => "wallet_unlock",
            Stage::TickLookup => "tick_lookup",
            Stage::BuildOrder => "build_order",
            Stage::Sign => "sign",
            Stage::Hmac => "hmac",
            Stage::Serialize => "serialize",
            Stage::Http => "http",
        }
    }

    /// 该阶段是否属于本地计算（非网络）
    pub fn is_local(&self) -> bool {
        !matches!(self, Stage::Http)
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单笔下单的延迟明细
#[derive(Debug, Clone, Default)]
pub struct OrderLatency {
    stages: Vec<(Stage, u64)>,
    total_ns: u64,
}

impl OrderLatency {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录某阶段耗时
    pub fn record(&mut self, stage: Stage, ns: u64) {
        self.stages.push((stage, ns));
    }

    /// 记录某阶段耗时（Duration 版）
    pub fn record_dur(&mut self, stage: Stage, d: Duration) {
        self.record(stage, d.as_nanos() as u64);
    }

    /// 设置总耗时
    pub fn set_total(&mut self, d: Duration) {
        self.total_ns = d.as_nanos() as u64;
    }

    pub fn total_ns(&self) -> u64 {
        self.total_ns
    }

    pub fn total_ms(&self) -> f64 {
        self.total_ns as f64 / 1e6
    }

    /// 某阶段耗时（纳秒）
    pub fn stage_ns(&self, stage: Stage) -> u64 {
        self.stages
            .iter()
            .filter(|(s, _)| *s == stage)
            .map(|(_, n)| *n)
            .sum()
    }

    /// 本地计算总耗时（不含网络）
    pub fn local_ns(&self) -> u64 {
        self.stages
            .iter()
            .filter(|(s, _)| s.is_local())
            .map(|(_, n)| *n)
            .sum()
    }

    /// 网络耗时
    pub fn network_ns(&self) -> u64 {
        self.stage_ns(Stage::Http)
    }

    /// 是否达成延迟目标
    pub fn within(&self, budget: Duration) -> bool {
        self.total_ns <= budget.as_nanos() as u64
    }

    pub fn stages(&self) -> &[(Stage, u64)] {
        &self.stages
    }
}

impl fmt::Display for OrderLatency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "total={:.3}ms [", self.total_ms())?;
        let mut first = true;
        for (s, ns) in &self.stages {
            if !first {
                write!(f, " ")?;
            }
            first = false;
            write!(f, "{}={:.3}ms", s.as_str(), *ns as f64 / 1e6)?;
        }
        write!(f, "]")
    }
}

/// 计时守卫：构造时开始计时，`stop()` 时写入 `OrderLatency`
pub struct StageTimer {
    start: Instant,
}

impl StageTimer {
    #[inline]
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    #[inline]
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// 结束计时并写入延迟记录
    #[inline]
    pub fn stop(self, lat: &mut OrderLatency, stage: Stage) {
        lat.record_dur(stage, self.start.elapsed());
    }
}

/// 固定容量的样本环形缓冲
#[derive(Debug)]
struct Samples {
    buf: Vec<u64>,
    cap: usize,
    next: usize,
    len: usize,
}

impl Samples {
    fn new(cap: usize) -> Self {
        Self {
            buf: vec![0; cap],
            cap,
            next: 0,
            len: 0,
        }
    }

    fn push(&mut self, v: u64) {
        self.buf[self.next] = v;
        self.next = (self.next + 1) % self.cap;
        if self.len < self.cap {
            self.len += 1;
        }
    }

    /// 返回排序后的样本副本
    fn sorted(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.buf[..self.len].to_vec();
        v.sort_unstable();
        v
    }
}

/// 分位数摘要（纳秒）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Percentiles {
    pub count: usize,
    pub min: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
    pub mean: u64,
}

impl Percentiles {
    fn from_sorted(v: &[u64]) -> Self {
        if v.is_empty() {
            return Self::default();
        }
        let n = v.len();
        let pick = |q: f64| -> u64 {
            // 最近秩法：idx = ceil(q * n) - 1
            let idx = ((q * n as f64).ceil() as usize).saturating_sub(1);
            v[idx.min(n - 1)]
        };
        let sum: u128 = v.iter().map(|x| *x as u128).sum();
        Self {
            count: n,
            min: v[0],
            p50: pick(0.50),
            p95: pick(0.95),
            p99: pick(0.99),
            max: v[n - 1],
            mean: (sum / n as u128) as u64,
        }
    }

    pub fn p50_ms(&self) -> f64 {
        self.p50 as f64 / 1e6
    }
    pub fn p95_ms(&self) -> f64 {
        self.p95 as f64 / 1e6
    }
    pub fn p99_ms(&self) -> f64 {
        self.p99 as f64 / 1e6
    }
    pub fn max_ms(&self) -> f64 {
        self.max as f64 / 1e6
    }
    pub fn mean_ms(&self) -> f64 {
        self.mean as f64 / 1e6
    }
}

impl fmt::Display for Percentiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} mean={:.3}ms p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms",
            self.count,
            self.mean_ms(),
            self.p50_ms(),
            self.p95_ms(),
            self.p99_ms(),
            self.max_ms()
        )
    }
}

/// 延迟追踪器
///
/// 线程安全。保存最近 `capacity` 笔样本用于计算分位数。
pub struct LatencyTracker {
    total: RwLock<Samples>,
    per_stage: RwLock<HashMap<Stage, Samples>>,
    capacity: usize,
    /// 超预算次数
    breaches: AtomicU64,
    /// 总记录数
    recorded: AtomicU64,
    /// 延迟预算（纳秒）
    budget_ns: u64,
}

impl LatencyTracker {
    /// 创建追踪器
    ///
    /// - `capacity`: 滚动窗口样本数
    /// - `budget`: 延迟预算，超过则计入 breach
    pub fn new(capacity: usize, budget: Duration) -> Self {
        let cap = capacity.max(1);
        Self {
            total: RwLock::new(Samples::new(cap)),
            per_stage: RwLock::new(HashMap::new()),
            capacity: cap,
            breaches: AtomicU64::new(0),
            recorded: AtomicU64::new(0),
            budget_ns: budget.as_nanos() as u64,
        }
    }

    /// 默认：1024 样本窗口，100ms 预算
    pub fn with_default_budget() -> Self {
        Self::new(1024, Duration::from_millis(100))
    }

    /// 记录一笔下单的延迟
    pub fn record(&self, lat: &OrderLatency) {
        self.recorded.fetch_add(1, Ordering::Relaxed);
        if lat.total_ns > self.budget_ns {
            self.breaches.fetch_add(1, Ordering::Relaxed);
        }
        if let Ok(mut t) = self.total.write() {
            t.push(lat.total_ns);
        }
        if let Ok(mut m) = self.per_stage.write() {
            for (stage, ns) in &lat.stages {
                m.entry(*stage)
                    .or_insert_with(|| Samples::new(self.capacity))
                    .push(*ns);
            }
        }
    }

    /// 总耗时分位数
    pub fn total_percentiles(&self) -> Percentiles {
        self.total
            .read()
            .map(|t| Percentiles::from_sorted(&t.sorted()))
            .unwrap_or_default()
    }

    /// 指定阶段的分位数
    pub fn stage_percentiles(&self, stage: Stage) -> Percentiles {
        self.per_stage
            .read()
            .ok()
            .and_then(|m| m.get(&stage).map(|s| Percentiles::from_sorted(&s.sorted())))
            .unwrap_or_default()
    }

    /// 超预算次数
    pub fn breaches(&self) -> u64 {
        self.breaches.load(Ordering::Relaxed)
    }

    /// 总记录数
    pub fn recorded(&self) -> u64 {
        self.recorded.load(Ordering::Relaxed)
    }

    /// 达标率（0.0 ~ 1.0）
    pub fn compliance_rate(&self) -> f64 {
        let n = self.recorded();
        if n == 0 {
            return 1.0;
        }
        1.0 - (self.breaches() as f64 / n as f64)
    }

    /// 生成可读报告
    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str("─────────── 下单延迟报告 ───────────\n");
        s.push_str(&format!(
            "预算: {:.1}ms   样本: {}   超标: {} ({:.2}% 达标)\n",
            self.budget_ns as f64 / 1e6,
            self.recorded(),
            self.breaches(),
            self.compliance_rate() * 100.0
        ));
        s.push_str(&format!("总计   {}\n", self.total_percentiles()));
        s.push_str("── 分阶段 ──\n");
        for stage in Stage::ALL {
            let p = self.stage_percentiles(stage);
            if p.count > 0 {
                s.push_str(&format!("{:<14} {}\n", stage.as_str(), p));
            }
        }
        s
    }

    /// 清空统计
    pub fn reset(&self) {
        if let Ok(mut t) = self.total.write() {
            *t = Samples::new(self.capacity);
        }
        if let Ok(mut m) = self.per_stage.write() {
            m.clear();
        }
        self.breaches.store(0, Ordering::Relaxed);
        self.recorded.store(0, Ordering::Relaxed);
    }
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::with_default_budget()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lat_with_total(ms: u64) -> OrderLatency {
        let mut l = OrderLatency::new();
        l.set_total(Duration::from_millis(ms));
        l
    }

    #[test]
    fn test_stage_names_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in Stage::ALL {
            assert!(seen.insert(s.as_str()), "阶段名重复: {}", s.as_str());
        }
        assert_eq!(seen.len(), 7);
    }

    #[test]
    fn test_only_http_is_network() {
        for s in Stage::ALL {
            assert_eq!(s.is_local(), s != Stage::Http);
        }
    }

    #[test]
    fn test_order_latency_accumulate() {
        let mut l = OrderLatency::new();
        l.record(Stage::Sign, 1_000_000); // 1ms
        l.record(Stage::Hmac, 500_000); // 0.5ms
        l.record(Stage::Http, 40_000_000); // 40ms
        l.set_total(Duration::from_millis(42));

        assert_eq!(l.stage_ns(Stage::Sign), 1_000_000);
        assert_eq!(l.local_ns(), 1_500_000);
        assert_eq!(l.network_ns(), 40_000_000);
        assert!((l.total_ms() - 42.0).abs() < 1e-6);
    }

    #[test]
    fn test_within_budget() {
        let l = lat_with_total(80);
        assert!(l.within(Duration::from_millis(100)));
        assert!(!l.within(Duration::from_millis(50)));
    }

    #[test]
    fn test_stage_timer() {
        let mut l = OrderLatency::new();
        let t = StageTimer::start();
        std::thread::sleep(Duration::from_millis(5));
        t.stop(&mut l, Stage::Sign);
        assert!(l.stage_ns(Stage::Sign) >= 4_000_000);
    }

    #[test]
    fn test_percentiles_basic() {
        // 1..=100 ms
        let v: Vec<u64> = (1..=100).map(|i| i * 1_000_000).collect();
        let p = Percentiles::from_sorted(&v);
        assert_eq!(p.count, 100);
        assert_eq!(p.min, 1_000_000);
        assert_eq!(p.max, 100_000_000);
        assert_eq!(p.p50, 50_000_000);
        assert_eq!(p.p95, 95_000_000);
        assert_eq!(p.p99, 99_000_000);
    }

    #[test]
    fn test_percentiles_empty() {
        let p = Percentiles::from_sorted(&[]);
        assert_eq!(p.count, 0);
        assert_eq!(p.p99, 0);
    }

    #[test]
    fn test_tracker_records_and_breaches() {
        let tr = LatencyTracker::new(100, Duration::from_millis(100));
        for ms in [10u64, 20, 30, 150, 200] {
            tr.record(&lat_with_total(ms));
        }
        assert_eq!(tr.recorded(), 5);
        assert_eq!(tr.breaches(), 2); // 150, 200 超 100ms
        assert!((tr.compliance_rate() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn test_tracker_stage_percentiles() {
        let tr = LatencyTracker::new(100, Duration::from_millis(100));
        for i in 1..=10u64 {
            let mut l = OrderLatency::new();
            l.record(Stage::Http, i * 1_000_000);
            l.set_total(Duration::from_millis(i));
            tr.record(&l);
        }
        let p = tr.stage_percentiles(Stage::Http);
        assert_eq!(p.count, 10);
        assert_eq!(p.max, 10_000_000);
        // 未记录的阶段应为空
        assert_eq!(tr.stage_percentiles(Stage::Sign).count, 0);
    }

    #[test]
    fn test_ring_buffer_wraps() {
        let tr = LatencyTracker::new(5, Duration::from_millis(100));
        for ms in 1..=20u64 {
            tr.record(&lat_with_total(ms));
        }
        let p = tr.total_percentiles();
        assert_eq!(p.count, 5, "环形缓冲应只保留最近 5 个样本");
        // 最近 5 个是 16..20 ms
        assert_eq!(p.min, 16_000_000);
        assert_eq!(p.max, 20_000_000);
        // 但累计计数仍完整
        assert_eq!(tr.recorded(), 20);
    }

    #[test]
    fn test_reset() {
        let tr = LatencyTracker::new(10, Duration::from_millis(100));
        tr.record(&lat_with_total(500));
        assert_eq!(tr.breaches(), 1);
        tr.reset();
        assert_eq!(tr.recorded(), 0);
        assert_eq!(tr.breaches(), 0);
        assert_eq!(tr.total_percentiles().count, 0);
    }

    #[test]
    fn test_report_contains_stages() {
        let tr = LatencyTracker::new(10, Duration::from_millis(100));
        let mut l = OrderLatency::new();
        l.record(Stage::Sign, 1_000_000);
        l.record(Stage::Http, 30_000_000);
        l.set_total(Duration::from_millis(31));
        tr.record(&l);

        let r = tr.report();
        assert!(r.contains("sign"));
        assert!(r.contains("http"));
        assert!(r.contains("延迟报告"));
    }

    #[test]
    fn test_display_order_latency() {
        let mut l = OrderLatency::new();
        l.record(Stage::Sign, 1_500_000);
        l.set_total(Duration::from_millis(2));
        let s = format!("{l}");
        assert!(s.contains("sign=1.500ms"));
        assert!(s.contains("total=2.000ms"));
    }
}
