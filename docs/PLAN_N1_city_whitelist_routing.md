# N1 — 城市白名单 + 数据源地域路由（新功能拓展）

> **性质**：新功能拓展，**独立于 bug 修复 R 系列**（R6/T1–T5 不动）。编号 N 系列，本任务为 N1。
> **来源**：grill-me 质询（Q1–Q11）已全部对齐，结论见文末「决策记录」。
> **VERIFIED 由非实施者（伯森先生）判定**。红线不变：不改交易语义 / RiskConfig 阈值含义 / 聚合器与 gRPC 契约 / 签名与 cold_wallet 标准实现。

## 背景与目标
Polymarket 逐城市天气盘当前**极薄**（实测 1000 个活跃市场，天气类仅 7 条且全为宏观气候盘，目标 15 城 0 盘）。本任务把系统做成**「备好弹药等猎物」**形态：城市白名单可配置、数据源按地域自动路由，盘出现时自动接入；同时不扫无数据的城市空转。

**现状问题（代码已确认）**：
1. 摄取侧 `pipeline.py:137-139` 是「全启用源 × 全城市」笛卡尔积，无地域路由——北京会去打 NOAA（必败）、纽约会去打彩云（低效），失败被静默 catch 成 None，城市一多全是噪音 + 白费配额。
2. 市场发现**无城市白名单过滤**——`extract_location` 内置 25 个美国城市（无北京/上海），扫进的盘若非目标城市会因缺预报数据空转。
3. 城市配置**两套不一致**：摄取侧默认「纽约+北京」，发现侧内置「25 美国城市」。

## 范围（IN）
- **N1-1** 统一城市注册表（JSON 文件，单一权威源）
- **N1-2** 摄取侧按城市路由数据源（替代全城×全源）
- **N1-3** 市场发现白名单过滤（scanner 层注入）
- **N1-4** mock 端到端联调脚本（验证「盘出现→策略→下单」链路）
- **N1-5** 测试 + 文档（城市配置说明）

## 范围（OUT，明确不做）
- ❌ SQL 数据库（另立 **N2**，见下）
- ❌ ECMWF / JMA 的 key 注册与启用（美国+中国阶段不需要；Tier-2 城市再说）
- ❌ Tier-2 城市（首尔/东京/新加坡/伦敦/香港）的**启用**（仅注册进表，`enabled=false`）
- ❌ 彩云 token 申请（用户自行到彩云开放平台申请，是中国三城硬前提）
- ❌ 任何 R6 bug 修复项

---

## N1-1 城市注册表（单一权威源）
新建 `strategy_engine/city_registry.py` + 数据文件 `config/cities.json`（或 `.yaml`）。

**数据模型**（每城市一条）：
```json
{
  "name": "New York",
  "lat": 40.7128, "lon": -74.0060, "timezone": "America/New_York",
  "sources": ["noaa", "open_meteo"],
  "tier": 1, "enabled": true
}
```

**初始注册内容**：
- **Tier-1（enabled=true）**：
  - 美国 7 城 `sources=["noaa","open_meteo"]`：New York / Chicago / Miami / Los Angeles / San Francisco / Seattle / Atlanta
  - 中国 3 城 `sources=["caiyun","open_meteo"]`：Shanghai / Beijing / Shenzhen
- **Tier-2（enabled=false）**：Seoul / Tokyo（`["jma","open_meteo"]`）、Singapore / London / Hong Kong（`["open_meteo"]`，香港可加 `caiyun`）

**API**（供两侧读取）：
- `load_cities() -> List[CityConfig]`
- `enabled_cities() -> List[CityConfig]`（仅 enabled）
- `enabled_location_names_normalized() -> Set[str]`（用 `normalize_location`）
- `sources_for(city_name) -> List[str]`

**Allowed Files**：新建 `strategy_engine/city_registry.py`、`config/cities.json`。

## N1-2 摄取侧地域路由
改 `weather_data_ingestion/pipeline.py`：
- `run()` 默认 `locations` 改为「注册表 enabled 城市」（保持入参可覆盖）。
- 采集任务构建（137-139行）从「全源×全城」改为「按城市 `sources_for(city)` 选源」——只对该城市路由表里的源发任务。
- 数据源 `SOURCE_NAME` 需与注册表字符串对齐（`noaa`/`caiyun`/`ecmwf`/`jma` + 新增 `open_meteo` 兜底标识；若现有源类无独立 `open_meteo`，注明复用 ECMWF/JMA 的 Open-Meteo 回退路径，不新造源类）。
- 某城所有路由源失败仍 catch 成 None（行为不变），但**不再产生跨域必败任务**。

**Allowed Files**：`weather_data_ingestion/pipeline.py`、`weather_data_ingestion/config.py`（如需让 `locations` 默认来自注册表）。

## N1-3 市场发现白名单过滤
改 `market_discovery/scanner.py`：
- 在 `scan_once()` 拿到 `List[MarketInfo]` 后、registry 去重前，过滤：`normalize_location(m.location) in city_registry.enabled_location_names_normalized()`。
- 不在白名单的盘：记 debug 日志（`跳过非白名单城市: X`），不进 registry、不触发回调。
- 白名单为空时：fail-safe 记 warning 并放行全部（避免配置缺失导致全静默）。

**Allowed Files**：`market_discovery/scanner.py`、`market_discovery/models.py`（仅当 `extract_location` 已知城市库需补中国/亚太城市，见下）。

**注意**：`extract_location` 的内置城市库（models.py:216-222）目前全是美国城市、无北京/上海/深圳等。需把 Tier-1+Tier-2 全部 15 城加入已知库，否则白名单匹配不到（`extract_location` 返回 Unknown）。

## N1-4 mock 端到端联调
新建 `verify_city_pipeline_e2e.py`：
- 用 mock 盘（白名单内城市，如 New York）走「发现→预报匹配→策略→聚合→风控」全链路，断言能产出 `AggregatedOrder`。
- 用一个**非白名单** mock 盘（如 Berlin）断言被白名单过滤、不产生信号。
- 验证 Tier-2（enabled=false）城市不产生采集任务、不接盘。

## N1-5 测试与文档
- `test_city_registry.py`：注册表加载、enabled 过滤、归一化匹配、`sources_for` 路由正确（美国城含 noaa、中国城含 caiyun、Tier-2 不在 enabled）。
- `test_market_discovery.py` 增补：白名单过滤用例（白名单内/外城市）。
- `docs/` 增加 N1 城市配置说明（如何加城市、如何开 Tier-2、数据源与凭证对照表）。

---

## 验收门禁（每个子项提交前）
- `python -m pytest -q` 全绿（基线 123，新增用例后应 ≥125）。
- 不触碰 Rust 侧 / proto / 风控阈值 / 交易语义。
- `git status` 除 N1 授权文件外无多余改动。
- 若 `fast/*.rs` 再现文件锁：N1 不动 Rust，不应受影响；如误触，用 `git archive HEAD | tar -x -C <tmp>` 验证。

## 决策记录（grill-me Q1–Q11）
| Q | 决策 |
|---|---|
| Q1 | A — 备好弹药等猎物（盘来了自动接入），非现在就有盘可交易 |
| Q2 | 建统一城市注册表，单一权威源，两处读取 |
| Q3 | 是 — 引入「地域→数据源」路由，替代全城×全源 |
| Q4 | 美国+中国阶段不开 ECMWF/JMA key；彩云 token 用户自行申请（中国三城硬前提） |
| Q5 | 先美国 7 城 + 中国 3 城（Tier-1），Tier-2 待命 |
| Q6 | 统一注册表，市场发现白名单过滤从它读 |
| Q7 | A+B — 真实摄取保管道活，另跑 mock 端到端联调 |
| Q8 | SQL 无现有；新功能独立编号，不混入 bug 修复 |
| Q9 | 城市注册表用 JSON/YAML 文件（对齐现有 registry.py 模式），SQL 留给数据层 |
| Q10 | SQL 数据层范围 = 预报历史+信号/订单/成交/PnL+回测结果（另立 **N2**） |
| Q11 | 编号 **N1**（非 R7），属新功能拓展；SQL 另立 **N2** |

## 后续（不在本任务）
- **N2 — SQL 数据层**：SQLAlchemy + SQLite 起步（可升 PostgreSQL），存预报历史/信号/订单/成交/PnL/回测结果。待 N1 完成后单独立项质询。
- **Tier-2 城市启用**：ECMWF/JMA key 到位后，改 `cities.json` 的 `enabled=true` 即可，无需改代码。
