//! HTTP 传输层调优 —— 连接复用、TLS 预热、超时收紧
//!
//! ## 为什么这一层决定了能否进 100ms
//!
//! 一次「冷」HTTPS 请求要经历：
//!
//! ```text
//! DNS 解析      ~20-100ms
//! TCP 三次握手   ~1 RTT   (到美东 CLOB，从国内约 200ms+)
//! TLS 1.3 握手   ~1 RTT
//! HTTP 请求      ~1 RTT
//! ────────────────────────
//! 合计           3+ RTT
//! ```
//!
//! 而「热」请求（连接已在池中、HTTP/2 多路复用）只需 **1 RTT**。
//! 因此**连接复用是唯一能把网络部分压进预算的手段**——签名再快，
//! 握手一次就全白干。
//!
//! ## 具体措施
//!
//! | 措施 | 作用 |
//! |------|------|
//! | `tcp_nodelay(true)` | 关闭 Nagle 算法，小 JSON 包立即发出，省去最多 40ms 延迟确认 |
//! | `pool_max_idle_per_host` | 保持多条热连接，突发下单不排队 |
//! | `pool_idle_timeout(90s)` | 长于 CLOB 侧的空闲超时，避免连接被静默回收 |
//! | HTTP/2 keep-alive | 主动 PING 维持连接，同时探测半死连接 |
//! | `warmup()` | 启动时完成 DNS + TCP + TLS，把 3 RTT 移出交易时段 |
//! | 收紧的超时 | 宁可快速失败重试，也不要挂死等待 |

use reqwest::Client;
use std::time::{Duration, Instant};

/// 传输层调优参数
#[derive(Debug, Clone)]
pub struct HttpTuning {
    /// 每个 host 保留的最大空闲连接数
    pub pool_max_idle_per_host: usize,
    /// 空闲连接存活时间
    pub pool_idle_timeout: Duration,
    /// HTTP/2 keep-alive PING 间隔
    pub http2_keep_alive_interval: Duration,
    /// keep-alive PING 超时
    pub http2_keep_alive_timeout: Duration,
    /// 建立连接超时
    pub connect_timeout: Duration,
    /// 整体请求超时
    pub request_timeout: Duration,
    /// User-Agent
    pub user_agent: String,
}

impl Default for HttpTuning {
    fn default() -> Self {
        Self {
            // 8 条足以吸收信号聚合器一次性放出的批量订单
            pool_max_idle_per_host: 8,
            // 长于常见的 60s 服务端空闲超时
            pool_idle_timeout: Duration::from_secs(90),
            http2_keep_alive_interval: Duration::from_secs(20),
            http2_keep_alive_timeout: Duration::from_secs(10),
            // 连接超时设短：连不上就快速失败，交给上层重试/切换
            connect_timeout: Duration::from_millis(3_000),
            // 下单请求超时。超过这个时间行情早已变化，成交也无意义
            request_timeout: Duration::from_millis(5_000),
            user_agent: concat!("polymarket-rust/", env!("CARGO_PKG_VERSION")).to_string(),
        }
    }
}

impl HttpTuning {
    /// 面向低延迟的激进配置
    pub fn low_latency() -> Self {
        Self {
            pool_max_idle_per_host: 16,
            pool_idle_timeout: Duration::from_secs(120),
            http2_keep_alive_interval: Duration::from_secs(10),
            http2_keep_alive_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_millis(2_000),
            request_timeout: Duration::from_millis(3_000),
            ..Default::default()
        }
    }
}

/// 构建面向低延迟的 HTTP 客户端
///
/// 注意：不使用 `http2_prior_knowledge()`。对 HTTPS 而言，h2 会通过 TLS ALPN
/// 自动协商；强制 prior knowledge 反而会在服务端仅支持 HTTP/1.1 时直接失败。
pub fn build_fast_client(t: &HttpTuning) -> reqwest::Result<Client> {
    Client::builder()
        // 小包立即发送，这是交易类客户端的必选项
        .tcp_nodelay(true)
        .pool_max_idle_per_host(t.pool_max_idle_per_host)
        .pool_idle_timeout(t.pool_idle_timeout)
        .http2_keep_alive_interval(Some(t.http2_keep_alive_interval))
        .http2_keep_alive_timeout(t.http2_keep_alive_timeout)
        // 空闲时也保活，避免 NAT / LB 静默断链导致下一笔单触发重连
        .http2_keep_alive_while_idle(true)
        .http2_adaptive_window(true)
        .connect_timeout(t.connect_timeout)
        .timeout(t.request_timeout)
        .user_agent(&t.user_agent)
        .build()
}

/// 使用默认调优参数构建客户端
pub fn build_default_fast_client() -> reqwest::Result<Client> {
    build_fast_client(&HttpTuning::default())
}

/// 单次预热结果
#[derive(Debug, Clone)]
pub struct WarmupResult {
    pub host: String,
    pub ok: bool,
    pub elapsed: Duration,
    pub detail: String,
}

/// 预热到指定主机的连接
///
/// 完成 DNS 解析、TCP 握手、TLS 握手，并把连接留在池中。
/// **应在服务启动、行情开盘前调用**，把 3 个 RTT 的成本移出交易时段。
///
/// 即使返回非 2xx 也算预热成功——只要 TCP+TLS 建起来了，目的就达到了。
pub async fn warmup(client: &Client, host: &str) -> WarmupResult {
    let url = format!("{}/", host.trim_end_matches('/'));
    let t0 = Instant::now();
    match client.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            // 必须读完 body，否则连接不会归还到空闲池
            let _ = resp.bytes().await;
            WarmupResult {
                host: host.to_string(),
                ok: true,
                elapsed: t0.elapsed(),
                detail: format!("HTTP {status}"),
            }
        }
        Err(e) => WarmupResult {
            host: host.to_string(),
            ok: false,
            elapsed: t0.elapsed(),
            detail: e.to_string(),
        },
    }
}

/// 预热多轮
///
/// 预热不在关键路径上，顺序执行即可；HTTP/2 下首条连接建立后即可多路复用，
/// 额外轮次主要用于确认连接稳定可复用。
pub async fn warmup_n(client: &Client, host: &str, n: usize) -> Vec<WarmupResult> {
    let mut out = Vec::with_capacity(n.max(1));
    for _ in 0..n.max(1) {
        out.push(warmup(client, host).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_tuning_sane() {
        let t = HttpTuning::default();
        // 空闲超时必须长于服务端常见的 60s，否则连接会被静默回收
        assert!(t.pool_idle_timeout > Duration::from_secs(60));
        assert!(t.connect_timeout < t.request_timeout);
        assert!(t.pool_max_idle_per_host >= 4);
    }

    #[test]
    fn test_low_latency_is_tighter() {
        let d = HttpTuning::default();
        let l = HttpTuning::low_latency();
        assert!(l.request_timeout < d.request_timeout);
        assert!(l.connect_timeout < d.connect_timeout);
        assert!(l.pool_max_idle_per_host > d.pool_max_idle_per_host);
        assert!(l.http2_keep_alive_interval < d.http2_keep_alive_interval);
    }

    #[test]
    fn test_build_client_succeeds() {
        assert!(build_default_fast_client().is_ok());
        assert!(build_fast_client(&HttpTuning::low_latency()).is_ok());
    }

    #[test]
    fn test_user_agent_contains_version() {
        let t = HttpTuning::default();
        assert!(t.user_agent.contains("polymarket-rust/"));
    }
}
