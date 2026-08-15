# Windows 11 SSH 连接 Ubuntu Server 完整教程

> 适用场景：你有一台装了 Ubuntu Server 的机器（物理机/旧笔记本/云服务器），想在 Win11 上远程控制它。
> 本文覆盖：Ubuntu 端开启 SSH、获取 IP、Win11 连接、密钥登录、常见问题排查。

---

## 一、Ubuntu Server 端准备

### 1.1 安装并启动 SSH 服务

Ubuntu Server 一般默认已装 `openssh-server`，但可能没启动。执行：

```bash
# 安装（若已装会提示已是最新）
sudo apt update
sudo apt install openssh-server -y

# 启动并设置开机自启
sudo systemctl enable --now ssh

# 确认正在监听 22 端口
sudo systemctl status ssh
```

看到 `active (running)` 即成功。

### 1.2 获取服务器 IP 地址

```bash
ip addr show | grep "inet " | grep -v 127.0.0.1
```

- 局域网内通常是 `192.168.x.x` 或 `10.x.x.x`
- 如果是云服务器，用厂商控制台给的**公网 IP**

> 建议给服务器设**固定 IP**（路由器 DHCP 绑定 MAC 地址），否则路由器重启后 IP 可能变。

### 1.3 （可选）创建一个专用用户

不建议一直用 root 远程登录：

```bash
sudo adduser van          # 按提示设密码
sudo usermod -aG sudo van # 赋予 sudo 权限
```

---

## 二、Win11 端连接

Win11 自带 OpenSSH 客户端，**不需要装 PuTTY**。

### 2.1 打开终端

- 右键开始菜单 →「终端(管理员)」或「Windows 终端」
- 或按 `Win + R` → 输入 `cmd` / `powershell`

### 2.2 基础连接命令

```bash
# 格式：ssh 用户名@服务器IP
ssh van@192.168.1.100
```

首次连接会问：

```
The authenticity of host '192.168.1.100' can't be established.
ED25519 key fingerprint is SHA256:xxxxxx.
Are you sure you want to continue connecting (yes/no/[fingerprint])?
```

输入 `yes` 回车，然后输入密码即可登录。

### 2.3 指定端口（如果改过 SSH 端口）

```bash
ssh -p 2222 van@192.168.1.100
```

---

## 三、配置密钥登录（推荐，免密码+更安全）

每次输密码麻烦，且容易被暴力破解。用密钥：

### 3.1 在 Win11 生成密钥对

```bash
# 在 Win11 终端执行
ssh-keygen -t ed25519 -C "van@win11"
```

一路回车（默认存到 `C:\Users\你的用户名\.ssh\id_ed25519`）。

### 3.2 把公钥传到服务器

**方法 A：一行命令自动上传（最简单）**

```bash
ssh-copy-id van@192.168.1.100
```

如果 Win11 没有 `ssh-copy-id`（老版本），用方法 B：

**方法 B：手动复制**

```bash
# 1. Win11 查看公钥内容
cat C:\Users\你的用户名\.ssh\id_ed25519.pub
# 复制输出的全部文本

# 2. 在 Ubuntu 上粘贴
mkdir -p ~/.ssh
chmod 700 ~/.ssh
nano ~/.ssh/authorized_keys   # 粘贴公钥，Ctrl+O 保存，Ctrl+X 退出
chmod 600 ~/.ssh/authorized_keys
```

### 3.3 验证免密登录

```bash
ssh van@192.168.1.100
```

直接进系统、不用输密码即成功。

### 3.4 （可选）禁用密码登录，彻底防暴力破解

确认密钥能登录后，在 Ubuntu 上：

```bash
sudo nano /etc/ssh/sshd_config
```

改两行：

```
PasswordAuthentication no
PubkeyAuthentication yes
```

```bash
sudo systemctl restart ssh
```

> ⚠️ 改之前务必确认密钥登录可用，否则会把自己锁外面。

---

## 四、保存连接配置（不用每次打长命令）

### 4.1 用 SSH config 文件

在 Win11 上编辑 `C:\Users\你的用户名\.ssh\config`（没有就新建），写入：

```
Host ubuntu
    HostName 192.168.1.100
    User van
    Port 22
    IdentityFile C:\Users\你的用户名\.ssh\id_ed25519
```

之后只需：

```bash
ssh ubuntu
```

### 4.2 用 Windows 终端「标签页」

Windows 终端 → 下拉箭头 →「SSH 连接」→ 填 `van@192.168.1.100`，可固定到开始菜单。

---

## 五、文件传输

### 5.1 用 scp（命令行）

```bash
# 本地 → 服务器
scp C:\path\to\file.zip van@192.168.1.100:/home/van/

# 服务器 → 本地
scp van@192.168.1.100:/home/van/data.csv C:\Users\你的用户名\Downloads\
```

### 5.2 用可视化工具（更直观）

- **WinSCP**：图形化，左右分栏拖拽
- **VS Code + Remote-SSH 插件**：直接在 Win11 上编辑服务器文件，体验最好

---

## 六、常见问题排查

| 现象 | 原因 | 解决 |
|------|------|------|
| `Connection refused` | SSH 没启动 / 端口错 | `sudo systemctl status ssh` 检查；确认端口 |
| `Connection timed out` | IP 错 / 防火墙 / 不在同网段 | `ping 192.168.1.100` 测试；关 Ubuntu 防火墙 `sudo ufw allow ssh` |
| `Permission denied` | 密码错 / 密钥错 | 确认用户名；`chmod 600 ~/.ssh/authorized_keys` |
| 连上立刻断开 | 磁盘满 / 内存耗尽 | `df -h` 查磁盘；重启后再试 |
| 云服务器连不上 | 安全组没放行 22 端口 | 到云厂商控制台放行入站 22 |

---

## 七、本项目（AI Weather）的连接示例

```bash
# 1. 连接服务器
ssh ubuntu

# 2. 进入项目目录
cd ~/weather-bot/ai-weather-deploy

# 3. 启动（Docker 已配 sudo 免密或加 docker 组后）
sudo docker compose up -d

# 4. 看日志
sudo docker compose logs -f
```

---

*教程版本：2026-08-12 | 适用 Win11 22H2+ / Ubuntu Server 22.04+*
