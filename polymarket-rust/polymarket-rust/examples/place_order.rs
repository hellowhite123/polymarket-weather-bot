//! 基础下单流程示例
//!
//! 展示完整的端到端流程：
//! 1. 创建钱包管理器并导入私钥
//! 2. 初始化 CLOB 客户端
//! 3. 派生 API Key
//! 4. 查询市场数据（价格、订单簿）
//! 5. 提交限价单
//! 6. 查询和取消订单

use polymarket_rust::{ClobConfig, OrderFlow, PlaceOrderParams, Side, WalletManager};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ===== 配置 =====
    // 实际使用时请从环境变量或安全存储中读取
    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY").unwrap_or_else(|_| {
        "0x0000000000000000000000000000000000000000000000000000000000000001".to_string()
    });
    let password =
        std::env::var("KEYSTORE_PASSWORD").unwrap_or_else(|_| "default-password".to_string());
    let keystore_path =
        std::env::var("KEYSTORE_PATH").unwrap_or_else(|_| "keystore.json".to_string());

    // ===== 1. 初始化钱包管理器 =====
    let mut wallet_mgr = WalletManager::load(&keystore_path)?;

    // 如果钱包不存在则导入
    let address = if wallet_mgr.list_addresses().is_empty() {
        println!("导入私钥...");
        wallet_mgr.import_private_key(&private_key, &password)?
    } else {
        println!("使用已有钱包: {}", wallet_mgr.list_addresses()[0]);
        wallet_mgr.list_addresses()[0].clone()
    };

    println!("钱包地址: {address}");

    let wallet_mgr = Arc::new(wallet_mgr);

    // ===== 2. 创建下单流程 =====
    let config = ClobConfig::default();
    let mut flow = OrderFlow::new(wallet_mgr.clone(), config);

    // ===== 3. 派生 API Key =====
    println!("\n派生 API Key...");
    match flow.init_api_key(&address, &password).await {
        Ok(creds) => {
            println!("API Key: {}", creds.key);
            println!("Passphrase: {}", creds.passphrase);
        }
        Err(e) => {
            eprintln!("API Key 派生失败: {e}");
            eprintln!("（需要网络连接和有效的钱包）");
            return Ok(());
        }
    }

    // ===== 4. 查询市场数据 =====
    // 使用一个示例 token ID（实际使用时从 Gamma API 获取）
    let token_id = std::env::var("TOKEN_ID").unwrap_or_else(|_| {
        "48904718272642917209051062443697282654580972539858010698118450913067240868353".to_string()
    });

    println!("\n查询市场数据...");
    if let Ok(price) = flow.client().get_price(&token_id, Side::Buy).await {
        println!("买入价: {price}");
    }
    if let Ok(mid) = flow.client().get_midpoint(&token_id).await {
        println!("中间价: {mid}");
    }
    if let Ok(book) = flow.client().get_order_book(&token_id).await {
        println!(
            "订单簿 - 买单数: {}, 卖单数: {}",
            book.bids.len(),
            book.asks.len()
        );
    }

    // ===== 5. 提交限价单 =====
    println!("\n提交限价单...");
    let params = PlaceOrderParams::buy(&address, &password, &token_id, 0.01, 5.0);

    match flow.place_order(params).await {
        Ok(response) => {
            println!("下单成功！");
            println!("  订单 ID: {}", response.orderID);
            println!("  状态: {}", response.status);
            println!("  成交量: {}", response.makingAmount);

            // ===== 6. 取消订单 =====
            if !response.orderID.is_empty() {
                println!("\n取消订单 {}...", response.orderID);
                if let Err(e) = flow
                    .cancel_order(&address, &password, &response.orderID)
                    .await
                {
                    eprintln!("取消失败: {e}");
                }
            }
        }
        Err(e) => {
            eprintln!("下单失败: {e}");
        }
    }

    println!("\n流程完成。");
    Ok(())
}
