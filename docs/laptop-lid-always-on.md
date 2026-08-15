# Ubuntu Server 笔记本合盖继续运行教程

> 适用场景：你用**旧笔记本**当 Ubuntu Server 跑服务（比如 AI Weather 量化交易系统），希望**合上屏幕后机器不休眠、继续跑后台任务**。
> 核心思路：改 `logind` 配置 → 合盖不休眠 → 配好自动启动服务 → 远程管理。

---

## 一、为什么合盖会停

Ubuntu 默认策略：检测到笔记本盖子关闭 → 触发 `suspend`（挂起）。挂起后 CPU 停转、网络断、Docker 容器暂停、SSH 连不上。

我们要把合盖动作从「挂起」改成「啥也不做」。

---

## 二、修改合盖策略（核心步骤）

### 2.1 编辑 logind 配置

```bash
sudo nano /etc/systemd/logind.conf
```

找到这几行（可能被注释，前面带 `#`），改成如下值：

```
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
HandleLidSwitchDocked=ignore
```

> 三项都设 `ignore`：
> - `HandleLidSwitch`：用电池时也忽略合盖
> - `HandleLidSwitchExternalPower`：插电时忽略
> - `HandleLidSwitchDocked`：接了外接显示器时忽略

如果文件里没有这些行，直接在末尾追加即可。

### 2.2 让配置生效

```bash
# 重启 logind 服务（不会重启机器）
sudo systemctl restart systemd-logind
```

> 某些版本重启 logind 会踢掉当前会话，建议**直接重启机器**最稳：
> ```bash
> sudo reboot
> ```

### 2.3 验证

重启后合上盖子，等 30 秒，从另一台机器 `ping` 它的 IP 或 `ssh` 连一下：

```bash
# 在 Win11 上
ping 192.168.1.100
ssh ubuntu
```

能连上 = 合盖运行成功 ✅

---

## 三、防屏幕常亮烧屏（可选但推荐）

合盖虽不休眠，但如果你**不盖盖子**只息屏，可以设自动息屏省电：

```bash
# 10 分钟无操作自动熄屏（不休眠）
gsettings set org.gnome.desktop.session idle-delay 600
```

合盖场景其实不需要这步，盖子一盖物理上就黑了。

---

## 四、确保服务开机自启（断电恢复后自动跑）

合盖运行只解决「不休眠」，但**断电再来电 / 重启后**服务得自己起来。

### 4.1 Docker 服务（本项目 AI Weather 用这个）

让 Docker 服务本身开机自启：

```bash
sudo systemctl enable docker
```

让容器随 Docker 启动（在 `docker-compose.yml` 里给服务加 `restart: unless-stopped`）：

```yaml
services:
  app:
    restart: unless-stopped
  polymorph:
    restart: unless-stopped
```

然后启动：

```bash
cd ~/weather-bot/ai-weather-deploy
sudo docker compose up -d
```

之后无论断电、重启、合盖，容器都会自动拉起。

### 4.2 纯 systemd 服务（不用 Docker 时）

创建一个服务文件：

```bash
sudo nano /etc/systemd/system/ai-weather.service
```

内容：

```ini
[Unit]
Description=AI Weather Trading System
After=network.target docker.service

[Service]
Type=simple
WorkingDirectory=/home/van/weather-bot/ai-weather-deploy
ExecStart=/usr/bin/docker compose up
ExecStop=/usr/bin/docker compose down
Restart=always
User=van

[Install]
WantedBy=multi-user.target
```

启用：

```bash
sudo systemctl enable --now ai-weather.service
sudo systemctl status ai-weather.service
```

---

## 五、断电后来电自动开机（BIOS 设置）

如果担心停电后笔记本不自动开机，进 BIOS 设置：

1. 开机按 `F2` / `Del` / `F10`（看品牌）进 BIOS
2. 找 `Power Management` → `Restore on AC Power Loss` / `After Power Loss`
3. 设为 `Power On`（来电自动开机）
4. 保存退出（`F10`）

这样小区跳闸、拔插头后再通电，笔记本会自动启动并跑服务。

---

## 六、远程管理笔记本（不用开盖）

合盖后完全当无头服务器用，靠 SSH 管理：

```bash
# Win11 上
ssh ubuntu

# 看服务状态
sudo docker compose ps

# 看资源占用
htop
df -h

# 看合盖后的运行时间（uptime 不变说明没休眠）
uptime
```

---

## 七、散热与电池建议

- **长期合盖运行**：建议一直插电，避免电池循环损耗
- **散热**：笔记本合盖散热差，长期高负载可架高底部或用散热支架；可在 BIOS 里把风扇策略设 aggressive
- **电池保养**：多数笔记本插电满充后会自动维护，无需拔电池；若担心，可设充电阈值（联想/戴尔有官方工具）

---

## 八、排错清单

| 问题 | 排查 |
|------|------|
| 合盖后 SSH 连不上 | `logind.conf` 没改对 / 没重启；`sudo systemctl status systemd-logind` |
| 合盖后熄灯但风扇停 | 实际进入了 suspend，确认 `HandleLidSwitch=ignore` 已生效 |
| 重启后服务没起来 | `systemctl enable` 没跑；`docker compose` 没设 `restart` |
| 来电不自动开机 | BIOS 没设 `Restore on AC Power Loss` |
| 温度过热降频 | 清理风扇灰尘；改善通风 |

---

*教程版本：2026-08-12 | 适用 Ubuntu Server 22.04+ / 各类笔记本*
