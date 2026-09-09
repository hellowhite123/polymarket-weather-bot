//! gRPC 执行层服务入口。
//!
//! 启动 gRPC server, 监听 Python 策略引擎的连接。
//!
//! 用法:
//!   cargo run --bin server
//!   WALLET_ADDRESS=0x... WALLET_PASSWORD=xxx cargo run --bin server
//!
//! 环境变量:
//!   GRPC_ADDR       - 监听地址 (默认 127.0.0.1:50051)
//!   KEYSTORE_PATH   - 密钥库路径 (默认 keystore.json)
//!   WALLET_ADDRESS  - 钱包地址
//!   WALLET_PASSWORD - 钱包密码
//!   CLOB_HOST       - CLOB API 主机 (默认 https://clob.polymarket.com)
//!   CHAIN_ID        - 链 ID (默认 137)

use std::env;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use polymarket_rust::clob::client::ClobConfig;
use polymarket_rust::fast::executor::FastExecutor;
use polymarket_rust::fast::risk_engine::{RiskConfig, RiskEngine};
use polymarket_rust::fast::state_sync::StateSync;
use polymarket_rust::fast::trading_gate::{TradingGate, TradingGateState};
use polymarket_rust::grpc::{serve, ExecutionServiceImpl};
use polymarket_rust::order::OrderFlow;
use polymarket_rust::types::Chain;
use polymarket_rust::wallet::WalletManager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr = env::var("GRPC_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".to_string());
    let keystore_path = env::var("KEYSTORE_PATH").unwrap_or_else(|_| "keystore.json".to_string());
    // P0-002：优先读 WALLET_PASSWORD_FILE（Docker secret），回退 WALLET_PASSWORD
    let password = load_wallet_password();
    let address = env::var("WALLET_ADDRESS").unwrap_or_default();
    let clob_host =
        env::var("CLOB_HOST").unwrap_or_else(|_| "https://clob.polymarket.com".to_string());
    let chain_id: u64 = env::var("CHAIN_ID")
        .unwrap_or_else(|_| "137".to_string())
        .parse()?;
    let trading_gate = Arc::new(TradingGate::new());
    trading_gate.set_state(TradingGateState::Initializing);

    tracing::info!("Trading Gate initialized: {:?}", trading_gate.state());

    tracing::info!("启动 gRPC 执行层服务...");
    tracing::info!("  监听地址: {}", addr);
    tracing::info!("  密钥库: {}", keystore_path);
    tracing::info!("  CLOB: {}", clob_host);
    tracing::info!("  Chain ID: {}", chain_id);

    // 加载钱包
    let wallet_mgr = Arc::new(WalletManager::load(&keystore_path)?);
    tracing::info!("钱包密钥库已加载");

    // 创建 CLOB 配置
    let chain = match chain_id {
        137 => Chain::Polygon,
        80002 => Chain::Amoy,
        other => return Err(format!("Unsupported chain id: {}", other).into()),
    };
    let clob_config = ClobConfig {
        host: clob_host.clone(),
        chain,
        ..Default::default()
    };

    // 创建下单流程
    let mut order_flow = OrderFlow::new(wallet_mgr.clone(), clob_config.clone());

    // 初始化 API Key（P1-001：失败 FAIL-CLOSED，闸门 halt + 退出）
    if !address.is_empty() && !password.is_empty() {
        match order_flow.init_api_key(&address, &password).await {
            Ok(_) => tracing::info!("API Key 已初始化"),
            Err(e) => {
                tracing::error!("API Key 初始化失败: {}", e);
                trading_gate.halt(format!("api_key_init_failed: {e}"));
                return Err(format!("API Key 初始化失败: {e}").into());
            }
        }
    } else {
        tracing::error!("未提供 WALLET_ADDRESS/WALLET_PASSWORD, API Key 未初始化");
        trading_gate.halt("api_key_credentials_missing");
        return Err("未提供 WALLET_ADDRESS/WALLET_PASSWORD".into());
    }

    // 创建执行服务
    let mut service = ExecutionServiceImpl::new(
        order_flow,
        wallet_mgr.clone(),
        address.clone(),
        password.clone(),
        trading_gate.clone(),
    );

    // ★ R2-P0-08 接入风控引擎：place_order 强制走 RiskEngine.pre_check
    let risk_engine = Arc::new(RiskEngine::new(RiskConfig::default()));
    service.set_risk_engine(risk_engine.clone());
    tracing::info!("RiskEngine 已接入 place_order pre-check");

    // ★ P0：初始化 FastExecutor，并完成 API Key / 钱包 / 网络预热。
    let mut fe = FastExecutor::new(wallet_mgr.clone(), clob_config.clone())?;

    if let Err(e) = fe.init_api_key(&address, &password).await {
        tracing::error!("FastExecutor API Key 初始化失败: {}", e);
        trading_gate.halt(format!("fast_executor_api_key_failed: {e}"));
        return Err(format!("FastExecutor API Key 初始化失败: {e}").into());
    }

    // P1-002：CLOB 网络健康检查，失败 FAIL-CLOSED
    if let Err(e) = fe.clob_client().health_check().await {
        tracing::error!("CLOB 网络健康检查失败: {}", e);
        trading_gate.halt(format!("clob_health_check_failed: {e}"));
        return Err(format!("CLOB 网络健康检查失败: {e}").into());
    }
    tracing::info!("CLOB 网络健康检查通过");

    // 获取 L2 API 凭证
    let api_creds = fe
        .clob_client()
        .api_creds()
        .ok_or("FastExecutor API Key missing")?
        .clone();

    tracing::info!("FastExecutor API Key、签名域、HTTP 连接、热钱包预热完成");

    // 解锁钱包用于 StateSync L2 请求签名
    let wallet = wallet_mgr.unlock(&address, &password)?;

    // 保存 FastExecutor
    service.set_fast_executor(Arc::new(fe));

    tracing::info!("FastExecutor 已接入（亚秒级执行路径）");

    // StateSync 使用同一套认证上下文
    let state_sync = StateSync::new(
        &clob_host,
        "https://data-api.polymarket.com",
        &address,
        wallet,
        api_creds,
    );
    // ★ 获取启动时本地仓位与远端活动订单快照
    let local_positions = service.snapshot_positions().await;
    let local_pending_orders = service.snapshot_pending_exchange_order_ids();

    // ★ 4B：启动时执行一次完整远端状态对账
    let (pos_report, order_report) = state_sync
        .full_reconcile(&local_positions, &local_pending_orders)
        .await
        .map_err(|e| format!("启动 StateSync reconciliation 失败: {}", e))?;

    tracing::info!(
    "StateSync 启动对账完成: positions_in_sync={}, orders_in_sync={}, position_diffs={}, order_diffs={}",
    pos_report.in_sync,
    order_report.in_sync,
    pos_report.diffs.len(),
    order_report.diffs.len(),
);
    // ★ 只有持仓和订单都完全同步，才允许进入 READY（决策逻辑见 TradingGate::reconcile_complete，可单测）。
    trading_gate.reconcile_complete(pos_report.in_sync, order_report.in_sync);
    if trading_gate.state() == TradingGateState::Ready {
        tracing::info!("StateSync 启动对账通过，TradingGate -> READY");
    } else {
        tracing::error!(
        "StateSync 启动对账未通过，保持 TradingGate=Reconciling: positions_in_sync={}, orders_in_sync={}",
        pos_report.in_sync,
        order_report.in_sync,
    );
    }

    // P1-007：StateSync 后台周期对账（每 30s），断线/差异自动告警
    let service = Arc::new(service);
    let state_sync = Arc::new(state_sync);
    {
        let svc = service.clone();
        let ss = state_sync.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                let local_positions = svc.snapshot_positions().await;
                let local_orders = svc.snapshot_pending_exchange_order_ids();
                match ss.full_reconcile(&local_positions, &local_orders).await {
                    Ok((pos, ord)) => {
                        if !pos.in_sync || !ord.in_sync {
                            tracing::warn!(
                                "StateSync 后台对账发现差异: positions_in_sync={}, orders_in_sync={}",
                                pos.in_sync,
                                ord.in_sync,
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("StateSync 后台对账失败: {e}");
                    }
                }
            }
        });
    }

    // 启动 gRPC server
    serve(service, &addr).await?;

    Ok(())
}

/// 解析钱包密码：优先文件内容（trim），回退环境变量值。
/// 抽为纯函数便于单测（不触碰全局环境变量）。
fn resolve_password(file_content: Option<String>, env_password: String) -> String {
    match file_content {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => env_password,
    }
}

/// 读取钱包密码（P0-002）。
///
/// 优先读取 `WALLET_PASSWORD_FILE` 指向的 Docker secret 文件并 trim；
/// 未配置或读失败时回退 `WALLET_PASSWORD` 环境变量；两者均为空则返回空串，
/// 由上层（`!password.is_empty()` 检查）保持 fail-closed。
fn load_wallet_password() -> String {
    let file_content = env::var("WALLET_PASSWORD_FILE")
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok());
    resolve_password(file_content, env::var("WALLET_PASSWORD").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::resolve_password;

    #[test]
    fn secret_loaded_consistently_in_docker_file_priority() {
        // Docker secret 文件内容应优先，且 trim 掉换行/空白
        assert_eq!(
            resolve_password(Some("  supersecret\n".to_string()), "env-pass".to_string()),
            "supersecret"
        );
    }

    #[test]
    fn secret_loaded_consistently_in_docker_env_fallback() {
        // 未配置文件 → 回退环境变量
        assert_eq!(resolve_password(None, "env-pass".to_string()), "env-pass");
    }

    #[test]
    fn secret_loaded_consistently_in_docker_empty_file_falls_back() {
        // 文件存在但内容全空白 → 视为无效，回退环境变量
        assert_eq!(
            resolve_password(Some("   \n\t".to_string()), "env-pass".to_string()),
            "env-pass"
        );
    }

    #[test]
    fn secret_loaded_consistently_in_docker_both_empty_fail_closed() {
        // 两者都空 → 返回空串，上层 fail-closed
        assert_eq!(resolve_password(None, String::new()), "");
    }
}
