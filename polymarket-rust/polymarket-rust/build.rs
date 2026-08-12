//! Build script: 编译 proto 文件生成 Rust gRPC 代码。
//!
//! 依赖: protoc (Protocol Buffers Compiler) 必须在 PATH 中。
//!   - Windows: https://github.com/protocolbuffers/protobuf/releases
//!   - macOS:   brew install protobuf
//!   - Linux:   apt install protobuf-compiler
//!
//! 生成的代码在 OUT_DIR/polymarket.rs, 通过 tonic::include_proto! 引入。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)  // Rust 端只做 server
        .compile_protos(&["proto/polymarket.proto"], &["proto"])?;

    println!("cargo:rerun-if-changed=proto/polymarket.proto");

    Ok(())
}
