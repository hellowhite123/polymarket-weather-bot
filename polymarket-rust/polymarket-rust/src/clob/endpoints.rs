//! CLOB API 端点路径
//!
//! 与 Polymarket 官方 SDK 的 endpoints 定义保持一致。

// ===== 服务器时间 =====
pub const TIME: &str = "/time";

// ===== API Key 端点 =====
pub const CREATE_API_KEY: &str = "/auth/api-key";
pub const GET_API_KEYS: &str = "/auth/api-keys";
pub const DELETE_API_KEY_PATH: &str = "/auth/api-key";
pub const DERIVE_API_KEY: &str = "/auth/derive-api-key";

// ===== 市场数据 =====
pub const GET_MARKETS: &str = "/markets";
pub const GET_MARKET: &str = "/markets/"; // 后接 conditionId
pub const GET_ORDER_BOOK: &str = "/book";
pub const GET_MIDPOINT: &str = "/midpoint";
pub const GET_PRICE: &str = "/price";
pub const GET_PRICES: &str = "/prices";
pub const GET_SPREAD: &str = "/spread";
pub const GET_LAST_TRADE_PRICE: &str = "/last-trade-price";
pub const GET_TICK_SIZE: &str = "/tick-size";
pub const GET_NEG_RISK: &str = "/neg-risk";

// ===== 订单端点 =====
pub const POST_ORDER: &str = "/order";
pub const POST_ORDERS: &str = "/orders";
pub const CANCEL_ORDER: &str = "/order";
pub const CANCEL_ORDERS: &str = "/orders";
pub const CANCEL_ALL: &str = "/cancel-all";
pub const CANCEL_MARKET_ORDERS: &str = "/cancel-market-orders";
pub const GET_OPEN_ORDERS: &str = "/data/orders";
pub const GET_TRADES: &str = "/data/trades";

// ===== 余额 =====
pub const GET_BALANCE_ALLOWANCE: &str = "/balance-allowance";

// ===== 价格历史 =====
pub const GET_PRICES_HISTORY: &str = "/prices-history";
