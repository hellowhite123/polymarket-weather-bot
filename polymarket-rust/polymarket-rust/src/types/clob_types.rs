//! Polymarket CLOB core data structures
// API JSON fields use PascalCase (consistent with Polymarket API)

use crate::types::enums::{OrderType, Side, SignatureType};
use serde::{Deserialize, Serialize};

/// API Key credentials (for L2 auth)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyCreds {
    #[serde(default, rename = "apiKey")]
    pub key: String,

    #[serde(default)]
    pub secret: String,

    #[serde(default)]
    pub passphrase: String,
}

/// User order parameters (simplified, user-friendly)
#[derive(Debug, Clone)]
pub struct UserOrder {
    /// Conditional Token tokenID
    pub token_id: String,
    /// Price, between 0~1
    pub price: f64,
    /// Quantity (Conditional Token count)
    pub size: f64,
    /// Direction
    pub side: Side,
    /// Fee rate (bps), optional
    pub fee_rate_bps: Option<u64>,
    /// On-chain cancel nonce, optional
    pub nonce: Option<u64>,
    /// Expiration timestamp, optional
    pub expiration: Option<u64>,
    /// Taker address, None = public order
    pub taker: Option<String>,
}

/// User market order parameters
#[derive(Debug, Clone)]
pub struct UserMarketOrder {
    pub token_id: String,
    /// Price (optional for market orders, None = market price)
    pub price: Option<f64>,
    /// BUY: pUSD amount to spend; SELL: share quantity to sell
    pub amount: f64,
    pub side: Side,
    pub fee_rate_bps: Option<u64>,
    pub nonce: Option<u64>,
    pub taker: Option<String>,
    pub order_type: Option<OrderType>, // FOK or FAK
}

/// EIP-712 Order struct (full order for on-chain signing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub salt: String, // uint256
    pub maker: String,
    pub signer: String,
    pub taker: String,
    pub token_id: String,     // uint256
    pub maker_amount: String, // uint256
    pub taker_amount: String, // uint256
    pub expiration: String,   // uint256
    pub nonce: String,        // uint256
    pub fee_rate_bps: String, // uint256
    pub side: u8,             // uint8
    pub signature_type: u8,   // uint8
}

/// Signed order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedOrder {
    #[serde(flatten)]
    pub order: Order,
    pub signature: String,
}

/// Submit order API request body
#[derive(Debug, Clone, Serialize)]
pub struct NewOrderRequest {
    pub order: SignedOrder,
    pub owner: String,
    #[allow(non_snake_case)]
    pub orderType: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[allow(non_snake_case)]
    pub postOnly: Option<bool>,
}

/// Order API response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    pub success: bool,
    #[serde(default)]
    #[allow(non_snake_case)]
    pub errorMsg: String,
    #[serde(default)]
    #[allow(non_snake_case)]
    pub orderID: String,
    #[serde(default, rename = "transactionsHashes")]
    pub transactions_hashes: Vec<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    #[allow(non_snake_case)]
    pub takingAmount: String,
    #[serde(default)]
    #[allow(non_snake_case)]
    pub makingAmount: String,
}

/// Order book entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderSummary {
    pub price: String,
    pub size: String,
}

/// Order book summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSummary {
    pub market: String,
    pub asset_id: String,
    pub timestamp: String,
    pub bids: Vec<OrderSummary>,
    pub asks: Vec<OrderSummary>,
    pub min_order_size: String,
    pub tick_size: String,
    pub neg_risk: bool,
    #[serde(default)]
    pub last_trade_price: String,
    #[serde(default)]
    pub hash: String,
}

/// Price response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceResponse {
    pub price: String,
}

/// Midpoint response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidpointResponse {
    pub mid: String,
}

/// Market info (simplified)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub condition_id: String,
    pub question: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub tokens: Vec<MarketToken>,
    #[serde(default)]
    pub neg_risk: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketToken {
    pub token_id: String,
    pub outcome: String,
    #[serde(default)]
    pub price: f64,
}

/// Paginated response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginationResponse<T> {
    pub limit: u64,
    pub count: u64,
    #[serde(default)]
    pub next_cursor: String,
    pub data: Vec<T>,
}

/// Position info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub market: String,
    pub asset_id: String,
    pub size: String,
    #[serde(default)]
    pub cash_pnl: String,
}

/// Cancel order request
#[derive(Debug, Clone, Serialize)]
pub struct CancelOrderRequest {
    #[allow(non_snake_case)]
    pub orderID: String,
}

/// Batch cancel orders request
#[derive(Debug, Clone, Serialize)]
pub struct CancelOrdersRequest {
    #[allow(non_snake_case)]
    pub orderIDs: Vec<String>,
}

/// Cancel order response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderResponse {
    pub success: bool,
    #[serde(default)]
    #[allow(non_snake_case)]
    pub errorMsg: String,
}

/// Convert SignatureType to u8
impl From<SignatureType> for u8 {
    fn from(st: SignatureType) -> Self {
        st as u8
    }
}
