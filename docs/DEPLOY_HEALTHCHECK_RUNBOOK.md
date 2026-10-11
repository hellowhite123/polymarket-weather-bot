# 部署运行手册：容器健康与 unhealthy 处置（R5-009）

> 适用：`docker-compose.yml` 的 `app` 服务（Python 策略引擎 + `market_discovery` 守护）。
> 核心语义：**容器存活（Up）≠ 业务健康（Healthy）。**

## 1. 健康语义

| 现象 | 含义 |
|------|------|
| `Up` + `healthy` | `market_discovery` 进程存活（healthcheck 通过） |
| `Up` + `unhealthy` | 容器仍在运行，但 `market_discovery` **已死/异常**（业务不健康） |
| `Exited` | 容器已退出（本项目 daemon 模式下**不应**因 discovery 死亡而退出） |

- **healthcheck 检查的是真实业务进程**：扫描 `/proc/*/cmdline`，要求同一进程的 argv 同时含独立元素 `-m` 与 `market_discovery`（即 `python -m market_discovery …`）；**不检查 entrypoint PID**，也**不会误匹配**其他 `python -m <module>`。
- 判定由 **Docker healthcheck** 负责：`interval=30s / retries=3` → 连续 3 次失败转 `unhealthy`（entrypoint **不**自行计数）。
- `market_discovery` 退出时，entrypoint **不退出容器**（保持 `Up`），交由 healthcheck 反映业务异常。

## 2. 谁会因 unhealthy 而重启？——**不会自动重启**

`unhealthy` **本身不触发** Docker 自动重启（本项目未接入 supervisor/orchestrator）。`restart: unless-stopped` 只在**容器退出**时生效，而本设计下容器保持 `Up`。

因此**必须**有人/系统负责发现与处置：

- **单机 Compose（当前）**：由值班人员/监控告警发现 `unhealthy` 后**人工处置**。
- **多机/编排（未来）**：由外部 orchestrator（如 Swarm/K8s/自研 supervisor）消费 `unhealthy` 触发重启/替换（**不在本任务范围**）。

## 3. 监控与告警（建议）

至少配置其一，避免 `unhealthy` 无人知晓：

- 定时巡检：`docker inspect --format '{{.State.Health.Status}}' ai-weather-app`
- 日志关键字告警：`market_discovery 异常退出；容器保持运行`
- 或外部探针轮询健康状态。

## 4. 人工处置步骤

```powershell
# 1) 确认状态
docker inspect --format '{{.State.Status}} / {{.State.Health.Status}}' ai-weather-app

# 2) 查异常日志
docker compose logs --tail=200 app

# 3) 处置（单机 Compose）
docker compose restart app

# 4) 复核
docker inspect --format '{{.State.Health.Status}}' ai-weather-app
```

> `docker compose restart app` 会重新运行 entrypoint 并重启 `market_discovery`。

## 5. 真实 Docker 验收步骤（R5-009）

> ⚠️ OpenCode 当前环境**无 Docker**，无法执行；请在**有 Docker 的主机**运行。

```powershell
cd <项目根>
docker compose config                                   # Step 1: 合法
docker compose up -d --build
docker inspect --format '{{.State.Status}}/{{.State.Health.Status}}' ai-weather-app   # Step 2: running/healthy

docker compose logs app | Select-String "market_discovery PID"   # Step 3: 取 PID
docker compose exec app kill <PID>

docker inspect --format '{{.State.Status}}' ai-weather-app        # Step 4: 仍为 running（不退出）
Start-Sleep -Seconds 120                                          # Step 5: 等 ≥3×interval(30s)
docker inspect --format '{{.State.Health.Status}}' ai-weather-app # 期望 unhealthy
docker compose logs app                                           # 应见“market_discovery 异常退出；容器保持运行…”
```

## 6. 信号处理（静态核验）

- `docker stop` 向 PID 1（entrypoint）发 `SIGTERM`；`trap "... kill $PID_DISCOVERY ...; exit 0" TERM INT` 会**先杀 discovery 子进程再退出**，无孤儿进程。
- 该 `trap` 在当前 `while true; sleep 5` 循环下最多延迟一个 `sleep`（≤5s）触发；`set -e` 不会因 `kill -0` 失败（其处 `if` 条件）而中断。
- 未发现异常退出循环或无法退出的风险；**仍需真实 Docker 复验**（`docker stop` 后确认 `Exited` 且无残留进程）。
