//! gRPC 执行层模块。
//!
//! 实现 ExecutionService, 接收 Python 策略引擎的交易指令,
//! 通过 OrderFlow 执行实际下单。
//!
//! 架构:
//!   Python (client) --gRPC--> Rust (server)
//!   AggregatedOrder  --------> ExecutionServiceImpl --------> OrderFlow
//!                                                   <-------- OrderResponse
//!   OrderResult     <--------
//!
//! 使用方式:
//!   let service = ExecutionServiceImpl::new(order_flow, wallet_addr, password);
//!   polymarket_rust::grpc::serve(service, "127.0.0.1:50051").await;

pub mod service;

pub use service::{consume_trade_events, ExecutionServiceImpl};

use std::sync::Arc;
use tonic::transport::Server;

/// 启动 gRPC 服务器。
///
/// Args:
///   service: 执行服务实现
///   addr: 监听地址, 如 "127.0.0.1:50051" 或 "[::1]:50051"
pub async fn serve(
    service: Arc<ExecutionServiceImpl>,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr: std::net::SocketAddr = addr.parse()?;

    tracing::info!("gRPC 执行层服务启动, 监听 {}", addr);

    Server::builder()
        .add_service(polymarket::execution_service_server::ExecutionServiceServer::new(
            (*service).clone(),
        ))
        .serve(addr)
        .await?;

    Ok(())
}

/// 生成的 proto 代码模块。
///
/// 通过 tonic::include_proto! 引入 build.rs 编译生成的代码。
/// 如果 protoc 未安装或编译失败, 此模块将不存在, cargo build 会报错。
pub mod polymarket {
    tonic::include_proto!("polymarket");
}
