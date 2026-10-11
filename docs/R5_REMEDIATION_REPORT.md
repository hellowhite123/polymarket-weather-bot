# Round 05 整改验收报告（交独立审查）

> **诚实声明（必读）**：本报告由 **OpenCode（工程实施者）** 对自己产出的 Round 05 整改做**自测汇报**。
> **本报告不构成独立复审结论，不构成 VERIFIED，不构成 APPROVED**。最终结论必须由独立 AI / Human 给出。
> 状态在 `MASTER_TASKS.md` 记为 `FIXED`，VERIFIED 需非实施者判定。

## 1. 结论

- **是否已修正：是。** 8/9 项已实施并通过验收；**R5-002 按伯森先生指示先跳过**（目标公式/单位口径待确认）。
- 提交：`04665b7`（完整 `04665b7934c7d38039d2d28b0e417604e24dc08f`）
- 分支：`p0-blockers-2026-09-13`，父提交 `2be39c0`
- 变更规模：**29 files changed, 579 insertions(+), 170 deletions(-)**（含新增 `test_cleaning.py`）
- 生产语义红线遵守：未改交易语义 / RiskConfig 阈值含义 / 聚合器与 gRPC 契约 / 签名算法标准实现。

## 2. 提交信息

```
04665b7 fix(r5): Round05 整改 8 项（P0 集中度 TOCTOU / 私钥零化 / clippy 门禁 / 回测指标）
```

## 3. 验收硬信号（本地实测）

| 门禁 | 命令 | 结果 |
|---|---|---|
| Rust 测试 | `cargo test --target x86_64-pc-windows-msvc` | **336 + 4 + 18 + 1 passed / 0 failed** |
| Rust Lint | `cargo clippy --all-targets --target x86_64-pc-windows-msvc` | **0 error**；警告 **82 → 23** |
| Python 测试 | `python -m pytest` | **109 passed / 0 failed / 0 error**（基线 103，+6 新用例） |
| Compose | YAML 解析 + 探针本地执行 | YAML 合法；探针无守护进程时退出码 1（**本机无 Docker，未跑 `docker compose config`**） |

> 环境：Rust `PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe`，`--target x86_64-pc-windows-msvc`；Python venv `C:\Users\32389\.workbuddy\binaries\python\envs\default\Scripts\python.exe`。

## 4. 逐项落地与证据

### R5-001（P0）集中度检查 TOCTOU — 分母漏计 pending 预留
- 文件：`polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- 改动：新增 `PositionLimits::committed_total()`（`Σ(notional + reserved)`）；`check_concentration(..., already_reserved: bool)` 分母统一为 committed 口径——`check_and_reserve`（已预留，传 `true`，不重复加 notional）/ `pre_check`（未预留，传 `false`，补 notional）。
- 测试：新增 `test_concentration_counts_pending_reserves`（两笔 pending 预留 3000+3000，下单 4500 → committed=10500>2×5000 且 4500/10500≈42.9%>40% → 拒单）；既有 `test_check_and_reserve_concentration_failure_rolls_back` 仍绿。
- 验证：`risk_engine` 35 测试全绿。

### R5-003（P1）clean_forecast 原地修改输入
- 文件：`weather_data_ingestion/cleaning.py`
- 改动：方案 A——函数入口 `forecast = copy.deepcopy(forecast)`，兑现 docstring「不修改原始对象」契约。
- 测试：新增 `test_cleaning.py`（`cleaned is not fc` 且原对象字段不变；空 periods 亦返回副本）。

### R5-004（P1）AdaptiveWeighter 最小权重语义
- 文件：`strategy_engine/ensemble_fusion.py`
- 改动：先常规归一化得 `w_i` → `max(w_i, min_weight)` → 二次归一化；若多下限叠加致总和>1，仅从「高于下限」源按比例扣回超量，保证总和=1 且各源≥下限；不可行（`min_weight×n≥1`）退化为均匀。
- 测试：`test_adaptive_weighter_min_weight_after_normalization`（3 源、min_weight=0.1，断言各源≥0.1 且和=1）、`..._infeasible_falls_back_uniform`。

### R5-005（P1）私钥/密码未零化
- 文件：`Cargo.toml`（+`zeroize = "1"`）、`src/wallet/keystore.rs`、`src/wallet/manager.rs`
- 改动：`EncryptedKey::decrypt` 返回类型 `String` → **`Zeroizing<String>`**；`encrypt` 的 `pk_bytes`/`derived_key`、`decrypt` 的 `derived_key`/`plaintext` 均以 `Zeroizing` 承载，离开作用域擦除；`manager::unlock` 用 `private_key.as_str()` 解析后随作用域清零。PBKDF2 210k、AES-256-GCM 实现不变。
- 测试：`wallet` 13 测试全绿（含 roundtrip / wrong-password）。

### R5-006（P2）clippy 门禁修复
- 文件：`src/**`、`tests/**`、`examples/**`
- 改动：`client.rs` 测试用 `std::f64::consts::PI` 断言消除 2 个 `approx_constant` **error**；`field_reassign_with_default` 全改结构体初始化（risk_engine / service / mempool / tests）；2 处 `await_holding_lock`（`uma_monitor::poll`、`daemon::join`）改为 await 前释放锁；`unused import/variable`、`needless_borrow`、冗余 cast、`format!`、`redundant guard`、`is_none()` 等由 `cargo clippy --fix` + 手工清理。
- 结果：**0 error；警告 82→23**。

### R5-007（P2）权益曲线年化因子硬编码
- 文件：`strategy_engine/backtest/metrics.py`
- 改动：新增 `_resolve_periods_per_year(equity_curve, total_hours)`：`interval_hours = total_hours / len(equity_curve)`，`periods_per_year = 365.25×24 / interval_hours`；信息不足回退旧默认 `365×4`。
- 测试：`test_sharpe_annualization_follows_sampling`（6h vs 1h 采样 → Sharpe 之比 = √6）。

### R5-008（P2）持仓时间用 token_id 表匹配
- 文件：`strategy_engine/backtest/metrics.py`
- 改动：`_calc_avg_hold_time` 改为按 token 维护 OPEN 队列、**FIFO** 匹配最早未平仓 OPEN，一一对应（旧实现 `Dict[token_id]=OPEN` 覆盖错配）。
- 测试：`test_avg_hold_time_same_token_multiple_rounds`（同 token 两次开平 → 平均 11h，旧实现会得 2h）。

### R5-009（P2）假健康检查
- 文件：`docker-compose.yml`（app 段）
- 改动：`sys.exit(0)` 恒真 → `CMD-SHELL` 扫描 `/proc/*/cmdline`，**精确匹配 argv 元素** `-m` + `market_discovery`（避免误匹配 healthcheck 自身及 sh 包装进程）；新增 `start_period: 20s`。
- 验证：PyYAML 解析合法并确认折叠标量拼接结果；探针本地（无 /proc）返回 1。

## 5. 审查重点（请独立方重点核对）

1. **R5-001 口径统一**：`already_reserved` 布尔是否覆盖全部调用路径、两种路径是否等价于同一「prospective committed total」；既有回滚测试是否仍有效。
2. **R5-005 公共 API 变更**：`decrypt` 返回类型变化是否影响其他调用点（已确认仅 `manager.rs` + 本文件测试）；明文私钥是否仍存在非零化副本。
3. **R5-006 行为变更（需复核）**：`src/fast/daemon.rs:88` 原 `let _ = rpc.check_health_all();` 为「非绑定 future」，启动健康检查实际从未执行；改为 `rpc.check_health_all().await;`。**这是行为修正，请确认符合预期。**
4. **R5-006 `clippy --fix` 副作用**：该工具曾误删 `service.rs` 中仅测试使用的 `RiskConfig` import（lib 构建判 unused），已将该 import 移入 `mod tests` 修复；请复核 `--fix` 触达的其余文件无语义漂移。
5. **R5-007 公式口径**：采用伯森先生指定的 `total_hours / len(equity_curve)`（而非 `/(n-1)`），年化因子随采样粒度变化，是否认可。
6. **R5-009 探测健壮性**：依赖 Linux `/proc` 与 argv 精确匹配；容器内 `python -c` 自身与 `/bin/sh -c` 是否已排除误匹配。

## 6. 残留与未决（记录在案）

- **R5-002 未实施**：`MultiSourceStrategyParams` 增加 `kelly_fraction`/`base_capital`、`_calculate_quantity` 改 `kelly_fraction × base_capital × 价差优势系数`——「价差优势」定义与 quantity/USDC 单位口径待确认。
- **clippy 残留 23 警告**（非资金类）：8 处 serde API 字段名 `non_snake_case`（JSON 契约，不可改名）、never-read/dead-code、complex type、too-many-args、`is_empty` 建议。
- **Docker 未实机验证**：本机无 `docker`，`docker compose config` 与「app 失活转 unhealthy」未在容器内实测，仅做 YAML 解析与探针逻辑本地验证。
- **验证级别**：以上为**实施者自测**，不构成 VERIFIED / APPROVED；VERIFIED 需非实施者判定。
