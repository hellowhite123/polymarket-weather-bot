# HANDOFF — AI Weather 项目审查成果交接清单

> 生成目的：新会话不自动携带原对话历史，本文件物化已完成结论，供新对话一次性接力。
> 用法：新对话第一句 →「读 docs/HANDOFF_...md，基于其中结论继续。」

## 一、当前项目状态
- 路径：`D:\WorkBuddyData\AI Weather`
- 项目：Polymarket 天气预测量化系统（Python 策略 ~21.6k 行 + Rust 执行 ~20.9k 行）
- Python venv：`C:\Users\32389\.workbuddy\binaries\python\envs\default\Scripts\python.exe`
- Rust 构建需：`export PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe`
- 测试基线（独立验证）：
  - Python `pytest` = **123 passed / 0 failed**
  - Rust `cargo test` = **338 + 4 + 18 + 1 = 361 passed / 0 failed**（经 git archive 干净副本实跑）
  - 注：工作区 `fast/` 下 6 个 `.rs`（fill_records/market_events/mempool/nonce_manager/order_state/state_sync）曾被进程独占锁拒读；若再现，用 `git archive HEAD | tar -x -C <tmp>` 解副本跑测试绕过。

## 二、已交付 opencode 的整改任务（文件即成果）
- **`docs/INSTRUCTION_TO_OPENCODE_AGI1_riskgate_units.md`** ⭐ 唯一正式待执行项
  - 内容：`risk_gate.py` Shares vs USDC 单位混淆修复（P1，已实测复现）
  - 关键点：`_check_position_limit`/`_check_global_exposure` 拿 shares 对比 USD 上限；
    BUY_NO 用 target_price 会高估 4x；存量应取 `position_notionals`；FAIL-CLOSED 策略；
    6 个测试 + 鉴别力验证 + 验收锚点（pytest ≥125）。

## 三、R5 整改审查结论（代码正确、无资金级回归）
R5 共 9 项（R5-001~009，09-23 授权），逐项核验：
- ✅ **R5-001** 集中度 Sell 分支：`already_reserved = matches!(side,Buy)` 正确，mutation 测试到位。
- ✅ **R5-002** MultiSource 有界 sizing：残差σ、饱和 edge、USDC cap 层、fail-closed 完备。
- ✅ **R5-003** `clean_forecast` 深拷贝兑现不可变契约。
- ✅ **R5-004** `AdaptiveWeighter` min_weight 归一化后抬升 + 超量按比例回退（已实跑验证）。
- ✅ **R5-005** 私钥/派生密钥 `Zeroizing` 零化。
- ✅ **R5-006** clippy 0 error（82→23），纯 lint 无业务夹带。
- ✅ **R5-007** 回测年化按真实 timestamp 反推（n-1 间隔）。
- ✅ **R5-008** 持仓时间按 token FIFO 配对。
- ⚠️ **R5-001 残留 P1**：`check_concentration` 分母用 `committed_total`（含 reserved），
  但分子 `new_market` 仍用 `market_position()`（仅 filled）→ 同 token 多笔 pending 预留时
  集中度被低估。**未修**，测试只用不同 token 验证，缺同 token 分子场景。
- ⚠️ **记录矛盾**：commit `04665b7` message 自述「R5-002 未实施 / pytest 109」，与工作区实际
  （R5-002 已实施、123 passed）不符；建议修正或补说明。

## 四、待排任务（新对话可从中选）
1. **R5-001 分子口径修正**（P1）：`check_concentration` 的 `new_market` 改为该市场
   `filled + reserved`（新增 `market_committed(token)`），使分子/分母同为 committed 口径；
   补同 token 场景测试 + mutation 鉴别。
2. **R5-002 补 cap 溢出测试**：`max_position_per_market` 硬顶恰好命中/溢出断言。
3. **risk_gate.py（已在二、交付，按指令执行即可）**。
4. **`position_notionals` 数据链接线**（P1，独立于 AGI1）：该字段生产无写入点，
   导致 `get_global_exposure()` 实际回退到 shares；补齐写入或单独归档 finding。
5. **修正 `04665b7` commit message**（低优先）。

## 五、工具提醒
- 代码审查不走 8 路并行子代理（曾因模型层 429 限流全失败）；改由主会话亲自读 + git archive 验证。
- git 对象库（`git show/cat-file`）不受工作区文件锁影响。
- 触碰资金/风控改动须守住红线：不改交易语义 / RiskConfig 阈值含义 / 聚合器与 gRPC 契约。