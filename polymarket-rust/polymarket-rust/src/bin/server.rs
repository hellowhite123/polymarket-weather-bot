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
use polymarket_rust::grpc::{serve, ExecutionServiceImpl};
use polymarket_rust::order::OrderFlow;
use polymarket_rust::types::Chain;
use polymarket_rust::wallet::WalletManager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr = env::var("GRPC_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".to_string());
    let keystore_path = env::var("KEYSTORE_PATH").unwrap_or_else(|_| "keystore.json".to_string());
    let password = env::var("WALLET_PASSWORD").unwrap_or_default();
    let address = env::var("WALLET_ADDRESS").unwrap_or_default();
    let clob_host = env::var("CLOB_HOST").unwrap_or_else(|_| "https://clob.polymarket.com".to_string());
    let chain_id: u64 = env::var("CHAIN_ID").unwrap_or_else(|_| "137".to_string()).parse()?;

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
        host: clob_host,
        chain,
        ..Default::default()
    };

    // 创建下单流程
    let mut order_flow = OrderFlow::new(wallet_mgr.clone(), clob_config);

    // 初始化 API Key (如果有钱包地址和密码)
    if !address.is_empty() && !password.is_empty() {
        match order_flow.init_api_key(&address, &password).await {
            Ok(_) => tracing::info!("API Key 已初始化"),
            Err(e) => tracing::warn!("API Key 初始化失败 (稍后可重试): {}", e),
        }
    } else {
        tracing::warn!("未提供 WALLET_ADDRESS/WALLET_PASSWORD, API Key 未初始化");
    }

    // 创建执行服务
    let service = ExecutionServiceImpl::new(order_flow, wallet_mgr, address, password);

    // 启动 gRPC server
    serve(service, &addr).await?;

    Ok(())
}
