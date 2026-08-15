# Polymarket Rust 端

Rust 实现的 Polymarket 交易客户端，包含三大核心模块：

| 模块 | 说明 |
|------|------|
| **钱包管理** | AES-256-GCM 加密存储私钥，PBKDF2 密钥派生，支持磁盘持久化 |
| **CLOB API 封装** | L1/L2 认证、EIP-712 订单签名、市场数据、订单管理 |
| **基础下单流程** | 整合钱包与 API，端到端下单：派生 API Key → 签名 → 提交 |

## 快速开始

```rust
use polymarket_rust::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 初始化钱包管理器
    let mut wallet_mgr = WalletManager::load("keystore.json")?;
    let address = wallet_mgr.import_private_key(
        "0x your_private_key",
        "your_password",
    )?;

    // 2. 创建下单流程
    let wallet_mgr = Arc::new(wallet_mgr);
    let mut flow = OrderFlow::new(wallet_mgr, ClobConfig::default());

    // 3. 派生 API Key
    flow.init_api_key(&address, "your_password").await?;

    // 4. 下单
    let params = PlaceOrderParams::buy(&address, "your_password", "token_id", 0.5, 100.0);
    let response = flow.place_order(params).await?;
    println!("订单 ID: {}", response.orderID);

    Ok(())
}
```

## 架构

```
src/
├── wallet/              # 钱包管理（AES-256 加密）
│   ├── keystore.rs      #   AES-256-GCM 加密存储 + PBKDF2 密钥派生
│   ├── manager.rs       #   钱包管理器（导入/解锁/签名/持久化）
│   └── error.rs         #   错误类型
├── clob/                # Polymarket CLOB API 封装
│   ├── client.rs        #   HTTP 客户端（API Key/市场数据/订单管理）
│   ├── auth.rs          #   L1/L2 认证头构建
│   ├── signing/         #   签名实现
│   │   ├── eip712_order.rs  # EIP-712 订单签名
│   │   ├── l1_auth.rs       # L1 ClobAuth 签名
│   │   └── hmac_sig.rs      # L2 HMAC-SHA256 签名
│   ├── constants.rs     #   合约地址/协议常量
│   └── endpoints.rs     #   API 端点定义
├── order/               # 下单流程
│   ├── flow.rs          #   端到端下单流程
│   └── order_builder.rs #   订单构建器
└── types/               # 类型定义
    ├── clob_types.rs    #   API 数据结构
    ├── enums.rs         #   Side/OrderType/SignatureType 等
    └── order.rs         #   订单相关类型
```

## 核心功能

### 钱包管理（AES-256-GCM 加密）

- 私钥使用 **AES-256-GCM** 对称加密存储
- 加密密钥通过 **PBKDF2-HMAC-SHA256**（210,000 次迭代）从用户密码派生
- 每个条目使用随机 16 字节 salt + 12 字节 nonce
- 支持磁盘持久化（JSON 格式）

### Polymarket CLOB API 封装

| 认证级别 | 签名方式 | 用途 |
|---------|---------|------|
| L1 | EIP-712 ClobAuth | 创建/派生 API Key |
| L2 | HMAC-SHA256 | 所有需认证的 API 调用 |

**订单签名**：EIP-712 类型化数据签名，Order 结构包含 12 个字段（salt, maker, signer, taker, tokenId, makerAmount, takerAmount, expiration, nonce, feeRateBps, side, signatureType）。

**支持的 API**：
- API Key：创建、派生、列出
- 市场数据：价格、中间价、订单簿、tickSize
- 订单管理：限价单、市价单、取消单个/全部

### 基础下单流程

1. 派生 API Key（L1 认证）
2. 查询 tickSize（自动）
3. 构建 Order（price/size → makerAmount/takerAmount）
4. EIP-712 签名
5. 提交订单（L2 认证）

## 运行示例

```bash
export POLYMARKET_PRIVATE_KEY="0x..."
export KEYSTORE_PASSWORD="your_password"
cargo run --example place_order
```

## 测试

```bash
cargo test
```
