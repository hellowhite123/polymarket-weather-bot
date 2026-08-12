//! Polymarket CLOB 客户端
//!
//! 提供对 CLOB API 的完整封装，包括：
//! - API Key 管理（创建、派生、列出）
//! - 市场数据查询（价格、订单簿、市场信息）
//! - 订单管理（下单、取消、查询）

use crate::clob::auth::{create_l1_headers, create_l2_headers, L2HeaderArgs};
use crate::clob::constants::{CLOB_HOST, get_exchange_address, ZERO_ADDRESS};
use crate::clob::endpoints::*;
use crate::clob::error::{ClobError, ClobResult};
use crate::clob::signing::{generate_order_salt, sign_order};
use crate::fast::http_fast::{build_fast_client, HttpTuning};
use crate::types::clob_types::*;
use crate::types::enums::{Chain, OrderType, Side, SignatureType};
use crate::types::order::{
    round_config_for, CreateOrderOptions, COLLATERAL_DECIMALS,
};
use crate::wallet::WalletManager;
use ethers::signers::{LocalWallet, Signer};
use std::collections::HashMap;
use ethers::utils::parse_units;
use reqwest::Client;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// CLOB 客户端配置
#[derive(Debug, Clone)]
pub struct ClobConfig {
    /// CLOB 主机地址
    pub host: String,
    /// 链 ID
    pub chain: Chain,
    /// 签名类型
    pub signature_type: SignatureType,
    /// funder 地址（Polymarket Profile 地址，持有 USDC）
    /// 若不提供则使用 signer 地址
    pub funder_address: Option<String>,
}

impl Default for ClobConfig {
    fn default() -> Self {
        Self {
            host: CLOB_HOST.to_string(),
            chain: Chain::Polygon,
            signature_type: SignatureType::Eoa,
            funder_address: None,
        }
    }
}

/// Polymarket CLOB 客户端
pub struct ClobClient {
    config: ClobConfig,
    http: Client,
    wallet_manager: Arc<WalletManager>,
    api_creds: Option<ApiKeyCreds>,
}

impl ClobClient {
    /// 创建新的 CLOB 客户端
    pub fn new(wallet_manager: Arc<WalletManager>, config: ClobConfig) -> Self {
        Self {
            config,
            http: Client::builder()
                .build()
                .expect("构建 HTTP 客户端失败"),
            wallet_manager,
            api_creds: None,
        }
    }

    /// 设置 API 凭证（L2 认证）
    pub fn set_api_creds(&mut self, creds: ApiKeyCreds) {
        self.api_creds = Some(creds);
    }

    /// 检查是否已设置 API 凭证
    pub fn has_api_creds(&self) -> bool {
        self.api_creds.is_some()
    }

    /// 使用经过低延迟调优的 HTTP 客户端构造
    ///
    /// 启用 HTTP/2 多路复用、TCP_NODELAY、连接池与 TLS 预热支持，
    /// 是 [`crate::fast::FastExecutor`] 的推荐构造方式。
    pub fn new_fast(wallet_manager: Arc<WalletManager>, config: ClobConfig) -> ClobResult<Self> {
        let http = build_fast_client(&HttpTuning::default())?;
        Ok(Self {
            config,
            http,
            wallet_manager,
            api_creds: None,
        })
    }

    /// 使用自定义 HTTP 客户端构造（用于注入调优后的连接池）
    pub fn with_http(wallet_manager: Arc<WalletManager>, config: ClobConfig, http: Client) -> Self {
        Self {
            config,
            http,
            wallet_manager,
            api_creds: None,
        }
    }

    /// 暴露底层 HTTP 客户端，用于连接预热（warmup）
    pub fn http_client(&self) -> &Client {
        &self.http
    }

    /// 当前 CLOB 主机地址
    pub fn config_host(&self) -> &str {
        &self.config.host
    }

    /// 获取当前时间戳（秒）
    fn timestamp(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 解锁钱包
    fn unlock_wallet(&self, address: &str, password: &str) -> ClobResult<LocalWallet> {
        Ok(self.wallet_manager.unlock(address, password)?)
    }

    // ===== API Key 管理 =====

    /// 创建 API Key（L1 认证）
    ///
    /// 如果已存在 API Key，会返回新的；推荐使用 `create_or_derive_api_key`
    pub async fn create_api_key(&self, address: &str, password: &str) -> ClobResult<ApiKeyCreds> {
        let wallet = self.unlock_wallet(address, password)?;
        let timestamp = self.timestamp();
        let headers = create_l1_headers(&wallet, timestamp, 0).await?;

        let url = format!("{}{}", self.config.host, CREATE_API_KEY);
        let resp = self.http.post(&url).headers(self.build_header_map(&headers)?).send().await?;

        let creds = self.handle_response::<ApiKeyCreds>(resp).await?;
        Ok(creds)
    }

    /// 派生 API Key（L1 认证）
    ///
    /// 从钱包签名确定性派生 API Key，适合每次运行时调用
    pub async fn derive_api_key(&self, address: &str, password: &str) -> ClobResult<ApiKeyCreds> {
        let wallet = self.unlock_wallet(address, password)?;
        let timestamp = self.timestamp();
        let headers = create_l1_headers(&wallet, timestamp, 0).await?;

        let url = format!("{}{}", self.config.host, DERIVE_API_KEY);
        let resp = self.http.post(&url).headers(self.build_header_map(&headers)?).send().await?;

        let creds = self.handle_response::<ApiKeyCreds>(resp).await?;
        Ok(creds)
    }

    /// 创建或派生 API Key
    ///
    /// 先尝试创建，若已存在则派生
    pub async fn create_or_derive_api_key(&self, address: &str, password: &str) -> ClobResult<ApiKeyCreds> {
        // 先尝试创建
        match self.create_api_key(address, password).await {
            Ok(creds) => Ok(creds),
            Err(_) => {
                // 创建失败（可能已存在），尝试派生
                self.derive_api_key(address, password).await
            }
        }
    }

    /// 列出所有 API Key（L2 认证）
    pub async fn get_api_keys(&self, address: &str, password: &str) -> ClobResult<Vec<ApiKeyCreds>> {
        let wallet = self.unlock_wallet(address, password)?;
        let creds = self.require_creds()?;
        let timestamp = self.timestamp();
        let l2_args = L2HeaderArgs {
            method: "GET".to_string(),
            request_path: GET_API_KEYS.to_string(),
            body: None,
        };
        let headers = create_l2_headers(&wallet, &creds, &l2_args, timestamp)?;

        let url = format!("{}{}", self.config.host, GET_API_KEYS);
        let resp = self.http.get(&url).headers(self.build_header_map(&headers)?).send().await?;

        #[derive(serde::Deserialize)]
        struct ApiKeysResponse {
            apiKeys: Vec<ApiKeyCreds>,
        }

        let result: ApiKeysResponse = self.handle_response(resp).await?;
        Ok(result.apiKeys)
    }

    // ===== 市场数据 =====

    /// 获取指定 token 的价格
    pub async fn get_price(&self, token_id: &str, side: Side) -> ClobResult<f64> {
        let url = format!("{}{}", self.config.host, GET_PRICE);
        let resp = self
            .http
            .get(&url)
            .query(&[("token_id", token_id), ("side", side.as_str())])
            .send()
            .await?;

        let price_resp: PriceResponse = self.handle_response(resp).await?;
        price_resp
            .price
            .parse::<f64>()
            .map_err(|e| ClobError::MarketDataError(format!("价格解析失败: {e}")))
    }

    /// 获取中间价
    pub async fn get_midpoint(&self, token_id: &str) -> ClobResult<f64> {
        let url = format!("{}{}", self.config.host, GET_MIDPOINT);
        let resp = self.http.get(&url).query(&[("token_id", token_id)]).send().await?;

        let mid_resp: MidpointResponse = self.handle_response(resp).await?;
        mid_resp
            .mid
            .parse::<f64>()
            .map_err(|e| ClobError::MarketDataError(format!("中间价解析失败: {e}")))
    }

    /// 获取订单簿
    pub async fn get_order_book(&self, token_id: &str) -> ClobResult<OrderBookSummary> {
        let url = format!("{}{}", self.config.host, GET_ORDER_BOOK);
        let resp = self.http.get(&url).query(&[("token_id", token_id)]).send().await?;

        self.handle_response(resp).await
    }

    /// 获取 tick size
    pub async fn get_tick_size(&self, token_id: &str) -> ClobResult<String> {
        let url = format!("{}{}", self.config.host, GET_TICK_SIZE);
        let resp = self.http.get(&url).query(&[("token_id", token_id)]).send().await?;

        #[derive(serde::Deserialize)]
        struct TickSizeResponse {
            #[serde(rename = "tick_size")]
            tick_size: String,
        }

        let result: TickSizeResponse = self.handle_response(resp).await?;
        Ok(result.tick_size)
    }

    // ===== 订单管理 =====

    /// 提交限价单
    ///
    /// # 参数
    /// - `address`: 钱包地址
    /// - `password`: 钱包密码
    /// - `user_order`: 用户订单参数
    /// - `options`: 订单选项（tickSize, negRisk）
    /// - `order_type`: 订单类型（GTC/FOK/GTD/FAK）
    pub async fn post_limit_order(
        &self,
        address: &str,
        password: &str,
        user_order: &UserOrder,
        options: &CreateOrderOptions,
        order_type: OrderType,
    ) -> ClobResult<OrderResponse> {
        let wallet = self.unlock_wallet(address, password)?;
        let creds = self.require_creds()?;

        // 构建 Order
        let signed_order = self.build_and_sign_limit_order(
            &wallet,
            user_order,
            options,
        )?;

        let owner = self.get_owner_address(&wallet);
        let body = NewOrderRequest {
            order: signed_order,
            owner,
            orderType: order_type.as_str().to_string(),
            postOnly: None,
        };
        let body_json = serde_json::to_string(&body)?;

        let timestamp = self.timestamp();
        let l2_args = L2HeaderArgs {
            method: "POST".to_string(),
            request_path: POST_ORDER.to_string(),
            body: Some(body_json.clone()),
        };
        let headers = create_l2_headers(&wallet, &creds, &l2_args, timestamp)?;

        let url = format!("{}{}", self.config.host, POST_ORDER);
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .headers(self.build_header_map(&headers)?)
            .body(body_json)
            .send()
            .await?;

        self.handle_response(resp).await
    }

    /// 提交市价单
    ///
    /// # 参数
    /// - `address`: 钱包地址
    /// - `password`: 钱包密码
    /// - `user_market_order`: 用户市价单参数
    /// - `options`: 订单选项
    pub async fn post_market_order(
        &self,
        address: &str,
        password: &str,
        user_market_order: &UserMarketOrder,
        options: &CreateOrderOptions,
    ) -> ClobResult<OrderResponse> {
        let wallet = self.unlock_wallet(address, password)?;
        let creds = self.require_creds()?;

        let signed_order = self.build_and_sign_market_order(
            &wallet,
            user_market_order,
            options,
        )?;

        let order_type = user_market_order
            .order_type
            .unwrap_or(OrderType::FOK)
            .as_str()
            .to_string();

        let owner = self.get_owner_address(&wallet);
        let body = NewOrderRequest {
            order: signed_order,
            owner,
            orderType: order_type,
            postOnly: None,
        };
        let body_json = serde_json::to_string(&body)?;

        let timestamp = self.timestamp();
        let l2_args = L2HeaderArgs {
            method: "POST".to_string(),
            request_path: POST_ORDER.to_string(),
            body: Some(body_json.clone()),
        };
        let headers = create_l2_headers(&wallet, &creds, &l2_args, timestamp)?;

        let url = format!("{}{}", self.config.host, POST_ORDER);
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .headers(self.build_header_map(&headers)?)
            .body(body_json)
            .send()
            .await?;

        self.handle_response(resp).await
    }

    /// 取消订单（L2 认证）
    pub async fn cancel_order(&self, address: &str, password: &str, order_id: &str) -> ClobResult<CancelOrderResponse> {
        let wallet = self.unlock_wallet(address, password)?;
        let creds = self.require_creds()?;

        let body = CancelOrderRequest {
            orderID: order_id.to_string(),
        };
        let body_json = serde_json::to_string(&body)?;

        let timestamp = self.timestamp();
        let l2_args = L2HeaderArgs {
            method: "DELETE".to_string(),
            request_path: format!("{CANCEL_ORDER}"),
            body: Some(body_json.clone()),
        };
        let headers = create_l2_headers(&wallet, &creds, &l2_args, timestamp)?;

        let url = format!("{}{}", self.config.host, CANCEL_ORDER);
        let resp = self
            .http
            .delete(&url)
            .header("Content-Type", "application/json")
            .headers(self.build_header_map(&headers)?)
            .body(body_json)
            .send()
            .await?;

        self.handle_response(resp).await
    }

    /// 取消所有订单（L2 认证）
    pub async fn cancel_all_orders(&self, address: &str, password: &str) -> ClobResult<CancelOrderResponse> {
        let wallet = self.unlock_wallet(address, password)?;
        let creds = self.require_creds()?;

        let timestamp = self.timestamp();
        let l2_args = L2HeaderArgs {
            method: "DELETE".to_string(),
            request_path: CANCEL_ALL.to_string(),
            body: None,
        };
        let headers = create_l2_headers(&wallet, &creds, &l2_args, timestamp)?;

        let url = format!("{}{}", self.config.host, CANCEL_ALL);
        let resp = self
            .http
            .delete(&url)
            .headers(self.build_header_map(&headers)?)
            .send()
            .await?;

        self.handle_response(resp).await
    }

    // ===== 快速路径（预解锁钱包 + 缓存 tick） =====
    //
    // 以下方法拆解出「构建 / 序列化 / L2 签名 / 发送」各阶段，供
    // [`crate::fast::FastExecutor`] 分别计时，并消除原 `post_limit_order`
    // 中的 PBKDF2 解锁与 `GET /tick-size` 往返。原公开方法保持不变。

    /// 构建未签名的 `Order`（同步、零 await）
    ///
    /// 与原 `build_and_sign_limit_order` 中构建部分完全一致，仅不含签名，
    /// 便于上层单独测量「构建」与「签名」两个阶段的耗时。
    pub fn build_unsigned_order(
        &self,
        wallet: &LocalWallet,
        user_order: &UserOrder,
        options: &CreateOrderOptions,
    ) -> ClobResult<Order> {
        let round_config = round_config_for(options.tick_size);
        let (side_u8, raw_maker_amt, raw_taker_amt) = get_order_raw_amounts(
            user_order.side,
            user_order.size,
            user_order.price,
            round_config,
        );
        let maker_amount = parse_units_to_string(raw_maker_amt, COLLATERAL_DECIMALS);
        let taker_amount = parse_units_to_string(raw_taker_amt, COLLATERAL_DECIMALS);
        let signer_address = format!("{:?}", wallet.address());
        let maker = self.get_owner_address(wallet);
        let taker = user_order
            .taker
            .clone()
            .unwrap_or_else(|| ZERO_ADDRESS.to_string());
        let fee_rate_bps = user_order.fee_rate_bps.unwrap_or(0).to_string();
        let nonce = user_order.nonce.unwrap_or(0).to_string();
        let expiration = user_order.expiration.unwrap_or(0).to_string();
        let salt = generate_order_salt();

        Ok(Order {
            salt,
            maker,
            signer: signer_address,
            taker,
            token_id: user_order.token_id.clone(),
            maker_amount,
            taker_amount,
            expiration,
            nonce,
            fee_rate_bps,
            side: side_u8,
            signature_type: self.config.signature_type as u8,
        })
    }

    /// 构建未签名的市价单 `Order`（同步）
    pub fn build_unsigned_market_order(
        &self,
        wallet: &LocalWallet,
        user_market_order: &UserMarketOrder,
        options: &CreateOrderOptions,
    ) -> ClobResult<Order> {
        let round_config = round_config_for(options.tick_size);
        let price = user_market_order.price.unwrap_or(1.0);
        let (side_u8, raw_maker_amt, raw_taker_amt) = get_market_order_raw_amounts(
            user_market_order.side,
            user_market_order.amount,
            price,
            round_config,
        );
        let maker_amount = parse_units_to_string(raw_maker_amt, COLLATERAL_DECIMALS);
        let taker_amount = parse_units_to_string(raw_taker_amt, COLLATERAL_DECIMALS);
        let signer_address = format!("{:?}", wallet.address());
        let maker = self.get_owner_address(wallet);
        let taker = user_market_order
            .taker
            .clone()
            .unwrap_or_else(|| ZERO_ADDRESS.to_string());
        let fee_rate_bps = user_market_order.fee_rate_bps.unwrap_or(0).to_string();
        let nonce = user_market_order.nonce.unwrap_or(0).to_string();
        let salt = generate_order_salt();

        Ok(Order {
            salt,
            maker,
            signer: signer_address,
            taker,
            token_id: user_market_order.token_id.clone(),
            maker_amount,
            taker_amount,
            expiration: "0".to_string(),
            nonce,
            fee_rate_bps,
            side: side_u8,
            signature_type: self.config.signature_type as u8,
        })
    }

    /// 序列化下单请求体（同步）
    pub fn build_order_body(
        &self,
        wallet: &LocalWallet,
        signed_order: &SignedOrder,
        order_type: OrderType,
    ) -> ClobResult<String> {
        let owner = self.get_owner_address(wallet);
        let body = NewOrderRequest {
            order: signed_order.clone(),
            owner,
            orderType: order_type.as_str().to_string(),
            postOnly: None,
        };
        Ok(serde_json::to_string(&body)?)
    }

    /// 计算 L2 认证请求头（同步，含 HMAC-SHA256）
    pub fn build_order_l2_headers(
        &self,
        wallet: &LocalWallet,
        body_json: &str,
    ) -> ClobResult<HashMap<String, String>> {
        let creds = self.require_creds()?;
        let timestamp = self.timestamp();
        let l2_args = L2HeaderArgs {
            method: "POST".to_string(),
            request_path: POST_ORDER.to_string(),
            body: Some(body_json.to_string()),
        };
        create_l2_headers(wallet, creds, &l2_args, timestamp)
    }

    /// 发送已签名的下单请求（异步，仅 HTTP 往返）
    pub async fn send_order(
        &self,
        body_json: String,
        headers: HashMap<String, String>,
    ) -> ClobResult<OrderResponse> {
        let url = format!("{}{}", self.config.host, POST_ORDER);
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .headers(self.build_header_map(&headers)?)
            .body(body_json)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    /// 签名并提交一份已签名的订单（异步）
    ///
    /// 等价于 `build_order_body` + `build_order_l2_headers` + `send_order`，
    /// 供不走分阶段计时的常规快速路径复用。
    pub async fn submit_signed_order(
        &self,
        wallet: &LocalWallet,
        signed_order: &SignedOrder,
        order_type: OrderType,
    ) -> ClobResult<OrderResponse> {
        let body = self.build_order_body(wallet, signed_order, order_type)?;
        let headers = self.build_order_l2_headers(wallet, &body)?;
        self.send_order(body, headers).await
    }

    /// 快速限价单（预解锁钱包）：消除 PBKDF2 与 tick 往返之外的全部开销
    pub async fn post_limit_order_pre(
        &self,
        wallet: &LocalWallet,
        user_order: &UserOrder,
        options: &CreateOrderOptions,
        order_type: OrderType,
    ) -> ClobResult<OrderResponse> {
        let order = self.build_unsigned_order(wallet, user_order, options)?;
        let verifying = get_exchange_address(self.config.chain, options.neg_risk);
        let signature = sign_order(wallet, &order, self.config.chain.id(), verifying)?;
        let signed = SignedOrder { order, signature };
        self.submit_signed_order(wallet, &signed, order_type).await
    }

    /// 快速市价单（预解锁钱包）
    pub async fn post_market_order_pre(
        &self,
        wallet: &LocalWallet,
        user_market_order: &UserMarketOrder,
        options: &CreateOrderOptions,
    ) -> ClobResult<OrderResponse> {
        let order = self.build_unsigned_market_order(wallet, user_market_order, options)?;
        let verifying = get_exchange_address(self.config.chain, options.neg_risk);
        let signature = sign_order(wallet, &order, self.config.chain.id(), verifying)?;
        let signed = SignedOrder { order, signature };
        let order_type = user_market_order
            .order_type
            .unwrap_or(OrderType::FOK)
            .as_str()
            .to_string();
        let owner = self.get_owner_address(wallet);
        let body = NewOrderRequest {
            order: signed,
            owner,
            orderType: order_type,
            postOnly: None,
        };
        let body_json = serde_json::to_string(&body)?;
        let headers = self.build_order_l2_headers(wallet, &body_json)?;
        self.send_order(body_json, headers).await
    }

    // ===== 内部辅助方法 =====

    /// 构建并签名限价单
    fn build_and_sign_limit_order(
        &self,
        wallet: &LocalWallet,
        user_order: &UserOrder,
        options: &CreateOrderOptions,
    ) -> ClobResult<SignedOrder> {
        let round_config = round_config_for(options.tick_size);

        // 计算原始金额
        let (side_u8, raw_maker_amt, raw_taker_amt) =
            get_order_raw_amounts(user_order.side, user_order.size, user_order.price, round_config);

        // 转为链上单位（6 decimals）
        let maker_amount = parse_units_to_string(raw_maker_amt, COLLATERAL_DECIMALS);
        let taker_amount = parse_units_to_string(raw_taker_amt, COLLATERAL_DECIMALS);

        let signer_address = format!("{:?}", wallet.address());
        let maker = self.get_owner_address(wallet);

        let taker = user_order
            .taker
            .clone()
            .unwrap_or_else(|| crate::clob::constants::ZERO_ADDRESS.to_string());

        let fee_rate_bps = user_order.fee_rate_bps.unwrap_or(0).to_string();
        let nonce = user_order.nonce.unwrap_or(0).to_string();
        let expiration = user_order.expiration.unwrap_or(0).to_string();

        let salt = generate_order_salt();

        let order = Order {
            salt,
            maker,
            signer: signer_address.clone(),
            taker,
            token_id: user_order.token_id.clone(),
            maker_amount,
            taker_amount,
            expiration,
            nonce,
            fee_rate_bps,
            side: side_u8,
            signature_type: self.config.signature_type as u8,
        };

        // 签名
        let verifying_contract = get_exchange_address(self.config.chain, options.neg_risk);
        let signature = sign_order(wallet, &order, self.config.chain.id(), verifying_contract)?;

        Ok(SignedOrder { order, signature })
    }

    /// 构建并签名市价单
    fn build_and_sign_market_order(
        &self,
        wallet: &LocalWallet,
        user_market_order: &UserMarketOrder,
        options: &CreateOrderOptions,
    ) -> ClobResult<SignedOrder> {
        let round_config = round_config_for(options.tick_size);
        let price = user_market_order.price.unwrap_or(1.0);

        let (side_u8, raw_maker_amt, raw_taker_amt) = get_market_order_raw_amounts(
            user_market_order.side,
            user_market_order.amount,
            price,
            round_config,
        );

        let maker_amount = parse_units_to_string(raw_maker_amt, COLLATERAL_DECIMALS);
        let taker_amount = parse_units_to_string(raw_taker_amt, COLLATERAL_DECIMALS);

        let signer_address = format!("{:?}", wallet.address());
        let maker = self.get_owner_address(wallet);

        let taker = user_market_order
            .taker
            .clone()
            .unwrap_or_else(|| crate::clob::constants::ZERO_ADDRESS.to_string());

        let fee_rate_bps = user_market_order.fee_rate_bps.unwrap_or(0).to_string();
        let nonce = user_market_order.nonce.unwrap_or(0).to_string();

        let salt = generate_order_salt();

        let order = Order {
            salt,
            maker,
            signer: signer_address.clone(),
            taker,
            token_id: user_market_order.token_id.clone(),
            maker_amount,
            taker_amount,
            expiration: "0".to_string(),
            nonce,
            fee_rate_bps,
            side: side_u8,
            signature_type: self.config.signature_type as u8,
        };

        let verifying_contract = get_exchange_address(self.config.chain, options.neg_risk);
        let signature = sign_order(wallet, &order, self.config.chain.id(), verifying_contract)?;

        Ok(SignedOrder { order, signature })
    }

    /// 获取 owner 地址（funder 或 signer）
    fn get_owner_address(&self, wallet: &LocalWallet) -> String {
        self.config
            .funder_address
            .clone()
            .unwrap_or_else(|| format!("{:?}", wallet.address()))
    }

    /// 获取 API 凭证（必须已设置）
    fn require_creds(&self) -> ClobResult<&ApiKeyCreds> {
        self.api_creds
            .as_ref()
            .ok_or(ClobError::NoApiCreds)
    }

    /// 将 HashMap 转为 reqwest::HeaderMap
    fn build_header_map(&self, headers: &std::collections::HashMap<String, String>) -> ClobResult<reqwest::header::HeaderMap> {
        let mut map = reqwest::header::HeaderMap::new();
        for (k, v) in headers {
            map.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| ClobError::AuthFailed(format!("无效的 header 名: {e}")))?,
                reqwest::header::HeaderValue::from_str(v)
                    .map_err(|e| ClobError::AuthFailed(format!("无效的 header 值: {e}")))?,
            );
        }
        Ok(map)
    }

    /// 处理 HTTP 响应
    async fn handle_response<T: DeserializeOwned>(&self, resp: reqwest::Response) -> ClobResult<T> {
        let status = resp.status().as_u16();
        let text = resp.text().await?;

        if status >= 400 {
            return Err(ClobError::Api {
                status,
                message: text,
            });
        }

        serde_json::from_str(&text).map_err(|e| ClobError::Api {
            status,
            message: format!("JSON 解析失败: {e}, 原始响应: {text}"),
        })
    }
}

/// 计算限价单的原始金额
///
/// - BUY: makerAmount = size * price (USDC), takerAmount = size (shares)
/// - SELL: makerAmount = size (shares), takerAmount = size * price (USDC)
fn get_order_raw_amounts(
    side: Side,
    size: f64,
    price: f64,
    config: crate::types::order::RoundConfig,
) -> (u8, f64, f64) {
    let raw_price = round_normal(price, config.price);

    match side {
        Side::Buy => {
            let raw_taker_amt = round_down(size, config.size);
            let mut raw_maker_amt = raw_taker_amt * raw_price;
            if decimal_places(raw_maker_amt) > config.amount {
                raw_maker_amt = round_up(raw_maker_amt, config.amount + 4);
                if decimal_places(raw_maker_amt) > config.amount {
                    raw_maker_amt = round_down(raw_maker_amt, config.amount);
                }
            }
            (0, raw_maker_amt, raw_taker_amt)
        }
        Side::Sell => {
            let raw_maker_amt = round_down(size, config.size);
            let mut raw_taker_amt = raw_maker_amt * raw_price;
            if decimal_places(raw_taker_amt) > config.amount {
                raw_taker_amt = round_up(raw_taker_amt, config.amount + 4);
                if decimal_places(raw_taker_amt) > config.amount {
                    raw_taker_amt = round_down(raw_taker_amt, config.amount);
                }
            }
            (1, raw_maker_amt, raw_taker_amt)
        }
    }
}

/// 计算市价单的原始金额
///
/// - BUY: makerAmount = amount (USDC), takerAmount = amount / price (shares)
/// - SELL: makerAmount = amount (shares), takerAmount = amount * price (USDC)
fn get_market_order_raw_amounts(
    side: Side,
    amount: f64,
    price: f64,
    config: crate::types::order::RoundConfig,
) -> (u8, f64, f64) {
    let raw_price = round_down(price, config.price);

    match side {
        Side::Buy => {
            let raw_maker_amt = round_down(amount, config.size);
            let mut raw_taker_amt = raw_maker_amt / raw_price;
            if decimal_places(raw_taker_amt) > config.amount {
                raw_taker_amt = round_up(raw_taker_amt, config.amount + 4);
                if decimal_places(raw_taker_amt) > config.amount {
                    raw_taker_amt = round_down(raw_taker_amt, config.amount);
                }
            }
            (0, raw_maker_amt, raw_taker_amt)
        }
        Side::Sell => {
            let raw_maker_amt = round_down(amount, config.size);
            let mut raw_taker_amt = raw_maker_amt * raw_price;
            if decimal_places(raw_taker_amt) > config.amount {
                raw_taker_amt = round_up(raw_taker_amt, config.amount + 4);
                if decimal_places(raw_taker_amt) > config.amount {
                    raw_taker_amt = round_down(raw_taker_amt, config.amount);
                }
            }
            (1, raw_maker_amt, raw_taker_amt)
        }
    }
}

/// 将浮点数转为指定精度的字符串，再转为链上单位
fn parse_units_to_string(value: f64, decimals: u32) -> String {
    // 先格式化到合理精度，避免浮点误差
    let formatted = format!("{:.10}", value);
    // 使用 ethers 的 parse_units
    match parse_units(&formatted, decimals as u32) {
        Ok(u) => u.to_string(),
        Err(_) => {
            // 回退：手动计算
            let multiplier = 10f64.powi(decimals as i32);
            let scaled = (value * multiplier).round();
            scaled.to_string()
        }
    }
}

/// 四舍五入到指定小数位
fn round_normal(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).round() / factor
}

/// 向下取整到指定小数位
fn round_down(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).floor() / factor
}

/// 向上取整到指定小数位
fn round_up(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).ceil() / factor
}

/// 计算浮点数的小数位数
fn decimal_places(value: f64) -> u32 {
    let s = format!("{:.10}", value);
    if let Some(dot) = s.find('.') {
        let after_dot = &s[dot + 1..];
        let trimmed = after_dot.trim_end_matches('0');
        trimmed.len() as u32
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_normal() {
        assert_eq!(round_normal(3.14159, 2), 3.14);
        assert_eq!(round_normal(3.146, 2), 3.15);
    }

    #[test]
    fn test_round_down() {
        assert_eq!(round_down(3.999, 2), 3.99);
    }

    #[test]
    fn test_round_up() {
        assert_eq!(round_up(3.001, 2), 3.01);
    }

    #[test]
    fn test_get_order_raw_amounts_buy() {
        let config = crate::types::order::RoundConfig {
            price: 2,
            size: 2,
            amount: 4,
        };
        let (side, maker, taker) = get_order_raw_amounts(Side::Buy, 100.0, 0.5, config);
        assert_eq!(side, 0);
        assert!((maker - 50.0).abs() < 0.0001);
        assert!((taker - 100.0).abs() < 0.0001);
    }

    #[test]
    fn test_get_order_raw_amounts_sell() {
        let config = crate::types::order::RoundConfig {
            price: 2,
            size: 2,
            amount: 4,
        };
        let (side, maker, taker) = get_order_raw_amounts(Side::Sell, 100.0, 0.5, config);
        assert_eq!(side, 1);
        assert!((maker - 100.0).abs() < 0.0001);
        assert!((taker - 50.0).abs() < 0.0001);
    }

    #[test]
    fn test_parse_units_to_string() {
        let result = parse_units_to_string(10.5, 6);
        assert_eq!(result, "10500000");
    }
}
