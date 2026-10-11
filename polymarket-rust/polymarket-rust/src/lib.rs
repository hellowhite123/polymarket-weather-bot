//! # Polymarket Rust 端
//!
//! 提供：
//! - **钱包管理**：AES-256-GCM 加密的私钥安全存储，支持助记词和以太坊签名
//! - **Polymarket CLOB API 封装**：L1/L2 认证、EIP-712 订单签名、市场数据和订单管理
//! - **基础下单流程**：整合钱包与 CLOB API 的端到端下单流程
//!
//! ## 快速开始
//!
//! ```no_run
//! use polymarket_rust::{WalletManager, ClobClient, ClobConfig, OrderFlow, PlaceOrderParams, Side};
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // 1. 初始化钱包管理器
//! let mut wallet_mgr = WalletManager::load("keystore.json")?;
//! let address = wallet_mgr.import_private_key(
//!     "0x your_private_key",
//!     "your_password",
//! )?;
//!
//! // 2. 创建下单流程
//! let wallet_mgr = Arc::new(wallet_mgr);
//! let mut flow = OrderFlow::new(wallet_mgr, ClobConfig::default());
//!
//! // 3. 初始化 API Key
//! flow.init_api_key(&address, "your_password").await?;
//!
//! // 4. 下单
//! let params = PlaceOrderParams::buy(&address, "your_password", "token_id", 0.5, 100.0);
//! let response = flow.place_order(params).await?;
//! println!("订单 ID: {}", response.orderID);
//! # Ok(())
//! # }
//! ```

pub mod clob;
pub mod fast;
pub mod grpc;
pub mod order;
pub mod types;
pub mod wallet;

// 重导出常用类型
pub use clob::{ClobClient, ClobConfig, ClobError, ClobResult};
pub use order::{OrderBuilder, OrderFlow, PlaceOrderParams};
pub use types::enums::{OrderType, Side, SignatureType, TickSize};
pub use types::{ApiKeyCreds, OrderResponse, SignedOrder, UserMarketOrder, UserOrder};
pub use wallet::{WalletError, WalletManager, WalletResult};

// 亚秒级执行层
pub use fast::executor::{ExecutionSource, FastExecutor, FastOrderParams, FastOrderResult};
pub use fast::mempool::{
    DedupStats, FrontRunDetector, FrontRunVerdict, MempoolConfig, MempoolMonitor, MempoolStats,
    OrderFingerprint, PendingEntry, PendingOrderTracker, PendingStats, PendingStatus,
    RateLimitStats, RateLimiter,
};

// 交易生命周期追踪与 Nonce 管理
pub use fast::nonce_manager::{NonceManager, NonceStats, NonceTracker, StuckTxEntry};
pub use fast::tx_lifecycle::{
    calculate_rbf_gas_price, prepare_rbf_replacement, RbfParams, TxEntry, TxState, TxStats,
    TxTracker,
};

// 多 RPC 节点池
pub use fast::rpc_pool::{
    NodeState, NodeStats, RpcNode, RpcPool, TransactionReceipt, DEFAULT_POLYGON_RPCS,
};

// 后台守护任务
pub use fast::daemon::BackgroundDaemon;

// 风控系统
pub use fast::risk_engine::{
    BreachLevel, ConcentrationChecker, DailyLossLimit, DrawdownTracker, GasFeeProtector,
    OracleDisputeMonitor, OracleStatus, PositionLimits, RiskConfig, RiskEngine, RiskSnapshot,
    TimeWindowGuard,
};

// 市场事件监听 + 状态同步
pub use fast::market_events::{
    BookSnapshot, EventStats, MarketEvent, MarketEventPoller, OrderStateEvent, SettlementEvent,
    TradeEvent,
};
pub use fast::state_sync::{
    ConnectionState, DiffItem, DiffSeverity, ReconciliationReport, ReconnectPolicy, StateSync,
    SyncStats,
};

// gRPC 执行层
pub use grpc::ExecutionServiceImpl;

pub mod constants {
    pub use crate::clob::constants::*;
}
