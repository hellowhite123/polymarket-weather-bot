"""market_discovery 模块测试。

覆盖:
  1. MarketInfo 模型: 事件类型推断 / 位置提取 / 过期判断
  2. GammaClient mock: 数据生成 / 逐轮新增
  3. MarketRegistry: 增量去重 / first_seen / 持久化
  4. MarketScanner: 单次扫描发现新盘 / 二次扫描无新盘 / 常驻循环节流
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from market_discovery.gamma_client import GammaClient
from market_discovery.models import MarketInfo, infer_event_type, extract_location
from market_discovery.registry import MarketRegistry
from market_discovery.scanner import MarketScanner

PASS = 0
FAIL = 0


def check(name: str, cond: bool, detail: str = ""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  [PASS] {name}")
    else:
        FAIL += 1
        print(f"  [FAIL] {name} {detail}")


# ----------------------------------------------------------------
# 1. 模型
# ----------------------------------------------------------------

def test_models():
    print("\n== 1. MarketInfo 模型 ==")
    m = MarketInfo(
        condition_id="c1", yes_token_id="t1", no_token_id="t1-no",
        question="Will it rain in New York on 08-11?",
        end_date_utc=9999999999.0,
    )
    check("事件类型推断 rain", m.event_type == "rain", m.event_type)
    check("位置提取 New York", m.location == "New York", m.location)

    t = MarketInfo(
        condition_id="c2", yes_token_id="t2", no_token_id="t2-no",
        question="Will the high temperature in Chicago on 08-12 exceed 30°C?",
        end_date_utc=9999999999.0,
    )
    check("事件类型推断 temperature", t.event_type == "temperature", t.event_type)

    s = MarketInfo(
        condition_id="c3", yes_token_id="t3", no_token_id="t3-no",
        question="Will it snow in Denver on 08-13?",
        end_date_utc=9999999999.0,
    )
    check("事件类型推断 snow", s.event_type == "snow", s.event_type)

    # 过期判断
    import time
    past = MarketInfo(
        condition_id="c4", yes_token_id="t4", no_token_id="t4-no",
        question="Will it rain in Boston on 08-01?",
        end_date_utc=time.time() - 100,
    )
    check("已过期市场判定", past.is_expired)
    check("未过期市场判定", not m.is_expired)

    # 缺少截止时间应报错
    try:
        MarketInfo(condition_id="c5", yes_token_id="t5", no_token_id="t5-no",
                   question="Will it rain in Miami?", end_date_utc=None)
        check("缺截止时间报错", False, "未抛异常")
    except ValueError:
        check("缺截止时间报错", True)


def test_p0_yes_no_tokens():
    print("\n== P0-01: YES/NO token 拆分 ==")
    m = MarketInfo.from_gamma({
        "conditionId": "c1",
        "clobTokenIds": ["YES123", "NO456"],
        "outcomes": ["Yes", "No"],
        "endDate": "2099-01-01T00:00:00Z",
    })
    check("yes_token_id 正确", m.yes_token_id == "YES123", m.yes_token_id)
    check("no_token_id 正确", m.no_token_id == "NO456", m.no_token_id)
    check("token_id property 返回 YES", m.token_id == "YES123", m.token_id)

    # 缺少 NO token 应报错
    try:
        MarketInfo.from_gamma({
            "conditionId": "c2",
            "clobTokenIds": ["ONLYONE"],
            "outcomes": ["Yes", "No"],
            "endDate": "2099-01-01T00:00:00Z",
        })
        check("缺 NO token 报错", False, "未抛异常")
    except ValueError:
        check("缺 NO token 报错", True)


# ----------------------------------------------------------------
# 2. Gamma 客户端 mock
# ----------------------------------------------------------------

def test_gamma_mock():
    print("\n== 2. GammaClient mock ==")
    client = GammaClient(force_mock=True, mock_seed=42)
    round1 = client.fetch_mock_markets()
    round2 = client.fetch_mock_markets()
    round3 = client.fetch_mock_markets()

    check("首轮生成基础盘", len(round1) >= 6, f"n={len(round1)}")
    check("每轮不缩水", len(round3) >= len(round2) >= len(round1),
          f"{len(round1)}->{len(round2)}->{len(round3)}")
    check("condition_id 唯一", len({m.condition_id for m in round3}) == len(round3))

    async def fetch_once():
        return await client.fetch_weather_markets()

    markets = asyncio.run(fetch_once())
    check("fetch_weather_markets 返回非空", len(markets) > 0)
    all_valid = all(m.end_date_utc is not None and m.token_id for m in markets)
    check("所有市场含 token/截止时间", all_valid)


# ----------------------------------------------------------------
# 3. 注册表
# ----------------------------------------------------------------

def test_registry():
    print("\n== 3. MarketRegistry 增量去重 ==")
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "registry.json")
        reg = MarketRegistry(path)
        check("空注册表计数 0", reg.count() == 0)

        # 首轮: 3 个新盘
        ms = [
            MarketInfo(condition_id=f"a{i}", yes_token_id=f"t{i}", no_token_id=f"t{i}-no",
                       question=f"Will it rain in City{i} on 08-1{i}?",
                       end_date_utc=9999999999.0)
            for i in range(3)
        ]
        new1 = reg.diff(ms)
        check("首轮发现 3 个新盘", len(new1) == 3, f"n={len(new1)}")
        check("注册表计数 3", reg.count() == 3)

        # 二轮: 2 个重复 + 1 个新
        ms2 = ms[:2] + [
            MarketInfo(condition_id="new1", yes_token_id="tn", no_token_id="tn-no",
                       question="Will it snow in Denver on 08-20?",
                       end_date_utc=9999999999.0)
        ]
        new2 = reg.diff(ms2)
        check("二轮只发现 1 个新盘", len(new2) == 1 and new2[0].condition_id == "new1",
              f"n={len(new2)}")
        check("注册表计数 4", reg.count() == 4)

        # 持久化: 重新加载
        reg2 = MarketRegistry(path)
        check("重载后计数一致", reg2.count() == 4)
        check("重载后 last_scan 存在", reg2.last_scan_utc is not None)

        # 已见市场刷新 last_seen
        first_seen = reg._markets["a0"]["first_seen_utc"]
        reg.diff([ms[0]])
        check("first_seen 不变", reg._markets["a0"]["first_seen_utc"] == first_seen)
        check("last_seen 更新", reg._markets["a0"]["last_seen_utc"] >= first_seen)

        # 摘要
        s = reg.summary()
        check("摘要 total=4", s["total"] == 4, str(s))


# ----------------------------------------------------------------
# 4. 扫描器
# ----------------------------------------------------------------

async def _run_scanner_twice():
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "registry.json")
        client = GammaClient(force_mock=True, mock_seed=7)
        scanner = MarketScanner(
            interval_seconds=600, registry_path=path, client=client,
        )
        new1 = await scanner.scan_once()
        new2 = await scanner.scan_once()
        return new1, new2, scanner


def test_scanner():
    print("\n== 4. MarketScanner ==")
    new1, new2, scanner = asyncio.run(_run_scanner_twice())
    check("首次扫描发现新盘", len(new1) >= 6, f"n={len(new1)}")
    check("累计扫描 2 次", scanner.scan_count == 2)
    check("注册表非空", scanner.registry.count() >= 6)
    # 第二次扫描也可能出现 mock 新增盘，但绝不减少
    check("二扫无缩水", scanner.registry.count() >= len(new1))

    # 常驻循环节流: 间隔 0.1s, 跑 2 次后停止
    async def loop_test():
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "r.json")
            client = GammaClient(force_mock=True, mock_seed=1)
            scanner = MarketScanner(
                interval_seconds=0.05, registry_path=path, client=client,
            )
            seen = []

            async def cb(new_mkts):
                seen.extend(new_mkts)

            task = asyncio.create_task(scanner.start(on_new_markets=cb))
            await asyncio.sleep(0.13)  # 约 2 次扫描
            scanner.stop()
            await asyncio.wait_for(task, timeout=2)
            return scanner, seen

    scanner2, seen = asyncio.run(loop_test())
    check("常驻循环运行多轮", scanner2.scan_count >= 2, f"scan={scanner2.scan_count}")
    check("回调触发新盘", len(seen) >= 6, f"seen={len(seen)}")
    check("stop 后循环退出", not scanner2.is_running)


# ----------------------------------------------------------------
# 5. CLI 冒烟
# ----------------------------------------------------------------

def test_cli():
    print("\n== 5. CLI ==")
    import subprocess
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "registry.json")
        r1 = subprocess.run(
            [sys.executable, "-m", "market_discovery", "--run-once", "--mock",
             "--registry-path", path],
            capture_output=True, text=True,
        )
        check("CLI --run-once 退出码 0", r1.returncode == 0,
              f"code={r1.returncode} stderr={r1.stderr[:200]}")
        check("CLI 生成注册表文件", os.path.exists(path))

        r2 = subprocess.run(
            [sys.executable, "-m", "market_discovery", "--status",
             "--registry-path", path],
            capture_output=True, text=True,
        )
        check("CLI --status 退出码 0", r2.returncode == 0,
              f"code={r2.returncode} stderr={r2.stderr[:200]}")


# ----------------------------------------------------------------
# 6. MarketInfo → MarketSnapshot (第二步: 策略引擎接入)
# ----------------------------------------------------------------

def test_market_snapshot():
    print("\n== 6. MarketInfo → MarketSnapshot ==")
    m = MarketInfo(
        condition_id="c1", yes_token_id="t1", no_token_id="t1-no",
        question="Will it rain in New York on 08-11?",
        outcome_prices=[0.55],
        end_date_utc=9999999999.0,
    )
    check("yes_price 从 outcome_prices 解析", abs(m.yes_price - 0.55) < 1e-9,
          str(m.yes_price))
    snap = m.to_market_snapshot()
    check("快照含 token_id", snap["token_id"] == "t1")
    check("快照 yes/no 互补", abs((snap["yes_price"] + snap["no_price"]) - 1.0) < 1e-9,
          str(snap))
    check("快照含成交量", snap["market_volume"] == m.volume)

    # 无价格时默认 0.5
    m2 = MarketInfo(condition_id="c2", yes_token_id="t2", no_token_id="t2-no",
                    question="Will it snow in Denver on 08-13?",
                    end_date_utc=9999999999.0)
    check("无价格默认 0.5", abs(m2.yes_price - 0.5) < 1e-9, str(m2.yes_price))

    # from_dict 往返（registry 持久化兼容）
    d = m.to_dict()
    m3 = MarketInfo.from_dict(d)
    check("from_dict 往返 condition_id", m3.condition_id == "c1")
    check("from_dict 往返 event_type", m3.event_type == "rain")
    check("from_dict 往返 prices", m3.outcome_prices == [0.55])


# ----------------------------------------------------------------
# 7. pipeline market_mode='registry' (第二步: 真实市场模式)
# ----------------------------------------------------------------

def test_pipeline_registry():
    print("\n== 7. pipeline market_mode='registry' ==")
    import tempfile as _tf

    forecast_file = "data/raw/noaa_NewYork_20260729T130543Z.json"
    if not os.path.exists(forecast_file):
        print("  [SKIP] 无气象预报文件，跳过")
        return

    with _tf.TemporaryDirectory() as tmp:
        reg_path = os.path.join(tmp, "registry.json")
        reg = MarketRegistry(reg_path)
        new = reg.diff([
            MarketInfo(condition_id="ny-rain-1", yes_token_id="real-token-ny", no_token_id="real-token-ny-no",
                       question="Will it rain in New York on 08-11?",
                       outcome_prices=[0.55],
                       end_date_utc=9999999999.0),
        ])
        check("注册表预置 1 个市场", len(new) == 1)

        import strategy_engine.models as se_models

        async def run_pipe2():
            from strategy_engine.config import StrategyConfig
            from strategy_engine.pipeline import StrategyPipeline
            config = StrategyConfig()
            config.tracker_storage_path = os.path.join(tmp, "tracker")
            config.signal_output_path = os.path.join(tmp, "signals")
            pipeline = StrategyPipeline(config)
            markets = pipeline._load_registry_markets(
                pipeline._load_forecasts([forecast_file]),
                [se_models.EventType("rain")],
                reg_path,
            )
            signals = await pipeline.run(
                forecast_files=[forecast_file],
                event_types=["rain"],
                market_mode="registry",
                registry_path=reg_path,
                output_signals=False,
            )
            return pipeline, markets, signals

        pipeline, markets, signals = asyncio.run(run_pipe2())
        check("registry 模式加载 1 个市场", len(markets) == 1, f"n={len(markets)}")
        if markets:
            check("匹配到真实 token_id", markets[0].token_id == "real-token-ny",
                  markets[0].token_id)
            check("市场价来自注册表", abs(markets[0].yes_price - 0.55) < 1e-9,
                  str(markets[0].yes_price))
        check("run 执行不抛错且返回 list", isinstance(signals, list))


# ----------------------------------------------------------------
# 8. MarketSignalBridge 闭环 (第二步: 新盘 → 策略信号)
# ----------------------------------------------------------------

def test_bridge():
    print("\n== 8. MarketSignalBridge 闭环 ==")
    forecast_file = "data/raw/noaa_NewYork_20260729T130543Z.json"
    if not os.path.exists(forecast_file):
        print("  [SKIP] 无气象预报文件，跳过")
        return

    import tempfile as _tf

    with _tf.TemporaryDirectory() as tmp:
        from market_discovery.bridge import MarketSignalBridge
        bridge = MarketSignalBridge(
            forecast_dir="data/raw",
            signal_dir=os.path.join(tmp, "signals"),
            registry_path=os.path.join(tmp, "registry.json"),
        )
        market = MarketInfo(
            condition_id="ny-rain-2", yes_token_id="real-token-ny2", no_token_id="real-token-ny2-no",
            question="Will it rain in New York on 08-12?",
            outcome_prices=[0.58],
            end_date_utc=9999999999.0,
        )

        # 预报匹配
        files = bridge.find_forecast_files(market)
        check("预报匹配 NewYork 文件", len(files) >= 1,
              f"matched={[os.path.basename(f) for f in files]}")

        # 评估
        signals = asyncio.run(bridge.evaluate_market(market))
        check("评估返回 list", isinstance(signals, list))
        s = bridge.summary()
        check("评估计数 1", s["evaluated_markets"] == 1, str(s))
        for sig in signals:
            check("信号带 condition_id", sig.metadata.get("condition_id") == "ny-rain-2")

        # 不匹配城市 → 无预报 → 跳过不崩
        miami = MarketInfo(
            condition_id="mia-rain-1", yes_token_id="t-mia", no_token_id="t-mia-no",
            question="Will it rain in Miami on 08-15?",
            outcome_prices=[0.5],
            end_date_utc=9999999999.0,
        )
        files2 = bridge.find_forecast_files(miami)
        check("无匹配城市预报为空", files2 == [], f"matched={files2}")


def main():
    print("market_discovery 测试开始")
    test_models()
    test_p0_yes_no_tokens()
    test_gamma_mock()
    test_registry()
    test_scanner()
    test_cli()
    test_market_snapshot()
    test_pipeline_registry()
    test_bridge()
    print(f"\n结果: {PASS} 通过, {FAIL} 失败")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
