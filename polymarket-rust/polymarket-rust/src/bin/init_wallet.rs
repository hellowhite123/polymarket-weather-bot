use polymarket_rust::wallet::WalletManager;
use std::io::{self, Write};

fn main() {
    if let Err(e) = run() {
        eprintln!("钱包初始化失败: {:?}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = "keystore.json";

    let manager = WalletManager::new(path);

    print!("请输入私钥: ");
    io::stdout().flush()?;

    let mut private_key = String::new();
    io::stdin().read_line(&mut private_key)?;

    print!("请输入密码: ");
    io::stdout().flush()?;

    let mut password = String::new();
    io::stdin().read_line(&mut password)?;

    let address = manager.import_private_key(private_key.trim(), password.trim())?;

    println!("=================");
    println!("钱包创建成功");
    println!("地址: {}", address);
    println!("keystore: {}", path);
    println!("=================");

    Ok(())
}
