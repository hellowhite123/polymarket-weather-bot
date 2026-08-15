"""
test_mock.py - 使用 mock 数据测试 NOAA 和彩云天气的解析逻辑

不依赖网络，纯本地测试解析与清洗是否正确。
"""

import json
import sys
import os

# 确保能导入包
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from weather_data_ingestion.config import Location, load_config
from weather_data_ingestion.models import (
    WeatherForecast, ForecastPeriod, Temperature, Precipitation, Wind, SourceMeta, now_utc_iso
)
from weather_data_ingestion.sources.noaa_source import NOAADataSource
from weather_data_ingestion.sources.caiyun_source import CaiyunDataSource
from weather_data_ingestion.cleaning import clean_forecast
from weather_data_ingestion.formatter import to_json, summary


def test_noaa_parse():
    """测试 NOAA gridpoints 响应解析。"""
    print("=" * 60)
    print("测试 NOAA 解析")
    print("=" * 60)

    # 模拟 NOAA /points 响应
    points_resp = {
        "properties": {
            "gridId": "OKX",
            "gridX": 33,
            "gridY": 37,
        }
    }

    # 模拟 NOAA /gridpoints 响应
    gridpoints_resp = {
        "properties": {
            "validTimes": "2024-01-15T12:00:00+00:00/PT6H",
            "temperature": {
                "uom": "wmoUnit:degC",
                "values": [
                    {"validTime": "2024-01-15T12:00:00+00:00/PT1H", "value": 5.0},
                    {"validTime": "2024-01-15T13:00:00+00:00/PT1H", "value": 6.0},
                    {"validTime": "2024-01-15T14:00:00+00:00/PT1H", "value": 7.0},
                    {"validTime": "2024-01-15T15:00:00+00:00/PT1H", "value": 8.0},
                    {"validTime": "2024-01-15T16:00:00+00:00/PT1H", "value": 999.0},  # 异常值
                    {"validTime": "2024-01-15T17:00:00+00:00/PT1H", "value": None},  # 缺失值
                    {"validTime": "2024-01-15T18:00:00+00:00/PT1H", "value": 10.0},
                ]
            },
            "probabilityOfPrecipitation": {
                "uom": "wmoUnit:percent",
                "values": [
                    {"validTime": "2024-01-15T12:00:00+00:00/PT2H", "value": 20},
                    {"validTime": "2024-01-15T14:00:00+00:00/PT2H", "value": 60},
                    {"validTime": "2024-01-15T16:00:00+00:00/PT3H", "value": 80},
                ]
            },
            "quantitativePrecipitation": {
                "uom": "wmoUnit:mm",
                "values": [
                    {"validTime": "2024-01-15T12:00:00+00:00/PT3H", "value": 0.5},
                    {"validTime": "2024-01-15T15:00:00+00:00/PT3H", "value": 2.5},
                ]
            },
            "windSpeed": {
                "uom": "wmoUnit:km_h-1",
                "values": [
                    {"validTime": "2024-01-15T12:00:00+00:00/PT4H", "value": 15.0},
                    {"validTime": "2024-01-15T16:00:00+00:00/PT4H", "value": 25.0},
                ]
            },
            "windDirection": {
                "values": [
                    {"validTime": "2024-01-15T12:00:00+00:00/PT4H", "value": 180},
                    {"validTime": "2024-01-15T16:00:00+00:00/PT4H", "value": 270},
                ]
            },
            "relativeHumidity": {
                "values": [
                    {"validTime": "2024-01-15T12:00:00+00:00/PT6H", "value": 65},
                ]
            },
        }
    }

    # 用 NOAADataSource._parse 解析
    location = Location(name="New York", lat=40.7128, lon=-74.0060)

    # 直接调用 _parse（绕过网络）
    source = NOAADataSource.__new__(NOAADataSource)
    forecast = NOAADataSource._parse(source, gridpoints_resp, location)

    print(f"城市: {forecast.location_name}")
    print(f"数据源: {forecast.source.source}")
    print(f"预报发布时间: {forecast.source.issue_time}")
    print(f"周期数: {forecast.period_count}")
    print()
    print("解析结果（前5个周期）:")
    for p in forecast.periods[:5]:
        d = p.to_dict()
        print(f"  {d['valid_time']} | temp={d['temp_c']}C | "
              f"precip_prob={d['precip_prob']} | precip_mm={d['precip_amount_mm']} | "
              f"wind={d['wind_speed_kph']}kph")

    # 验证
    assert forecast.period_count == 7, f"期望7个周期，得到{forecast.period_count}"
    assert forecast.periods[0].temperature.value_c == 5.0
    assert forecast.periods[0].precipitation.probability == 0.2  # 20% → 0.2
    assert forecast.periods[0].wind.speed_kph == 15.0
    print("\n[OK] NOAA 解析测试通过")

    # 测试清洗（异常值999.0应被移除，None应被插值）
    print("\n--- 清洗测试 ---")
    cleaned = clean_forecast(forecast)
    temps = [p.temperature.value_c for p in cleaned.periods]
    print(f"清洗后温度: {temps}")
    # 999.0 应被检测为异常并置None后插值
    assert 999.0 not in temps, "异常值999.0未被清除!"
    print("[OK] 清洗测试通过（异常值已移除，缺失值已插值）")

    return cleaned


def test_caiyun_parse():
    """测试彩云天气响应解析。"""
    print("\n" + "=" * 60)
    print("测试彩云天气解析")
    print("=" * 60)

    caiyun_resp = {
        "status": "ok",
        "api_version": "2.6",
        "server_time": 1705315200,
        "timezone": "Asia/Shanghai",
        "tzshift": 28800,
        "result": {
            "forecast_keypoint": "未来两小时有阵雨",
            "realtime": {
                "temperature": 8.5,
                "humidity": 0.65,
                "pressure": 101500,  # Pa
                "visibility": 15000,  # 米
            },
            "minutely": {
                "description": "未来2小时有中雨，预计30分钟后开始",
                "precipitation_2h": [0.0, 0.0, 0.5, 1.2, 2.5, 3.0, 2.8, 1.5, 0.3, 0.0] + [0.0] * 110,
            },
            "hourly": {
                "description": "未来48小时天气预报",
                "temperature": [
                    {"value": 8.5, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 9.0, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 10.0, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 11.0, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "apparent_temperature": [
                    {"value": 6.0, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 6.5, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 7.0, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 7.5, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "precipitation": [
                    {"value": 0.0, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 0.5, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 1.2, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 0.3, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "precipitation_probability": [
                    {"value": 0.2, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 0.6, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 0.8, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 0.3, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "wind": [
                    {"speed": 15.0, "direction": 180, "datetime": "2024-01-15T12:00+08:00"},
                    {"speed": 18.0, "direction": 190, "datetime": "2024-01-15T13:00+08:00"},
                    {"speed": 20.0, "direction": 200, "datetime": "2024-01-15T14:00+08:00"},
                    {"speed": 16.0, "direction": 210, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "humidity": [
                    {"value": 0.65, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 0.70, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 0.75, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 0.68, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "cloudrate": [
                    {"value": 0.8, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 0.9, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 0.85, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 0.5, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "pressure": [
                    {"value": 101500, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 101400, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 101300, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 101450, "datetime": "2024-01-15T15:00+08:00"},
                ],
                "visibility": [
                    {"value": 15000, "datetime": "2024-01-15T12:00+08:00"},
                    {"value": 12000, "datetime": "2024-01-15T13:00+08:00"},
                    {"value": 8000, "datetime": "2024-01-15T14:00+08:00"},
                    {"value": 20000, "datetime": "2024-01-15T15:00+08:00"},
                ],
            },
            "daily": {
                "temperature": {
                    "max": [
                        {"value": 12.0, "date": "2024-01-15T00:00+08:00"},
                        {"value": 13.0, "date": "2024-01-16T00:00+08:00"},
                    ],
                    "min": [
                        {"value": 2.0, "date": "2024-01-15T00:00+08:00"},
                        {"value": 3.0, "date": "2024-01-16T00:00+08:00"},
                    ],
                },
                "precipitation": {
                    "avg": [
                        {"value": 2.0, "date": "2024-01-15T00:00+08:00"},
                        {"value": 0.0, "date": "2024-01-16T00:00+08:00"},
                    ],
                },
            },
        }
    }

    location = Location(name="Beijing", lat=39.9042, lon=116.4074, timezone="Asia/Shanghai")

    # 直接调用 _parse
    source = CaiyunDataSource.__new__(CaiyunDataSource)
    forecast = CaiyunDataSource._parse(source, caiyun_resp, location)

    print(f"城市: {forecast.location_name}")
    print(f"数据源: {forecast.source.source}")
    print(f"预报发布时间: {forecast.source.issue_time}")
    print(f"小时级周期数: {len([p for p in forecast.periods if p.valid_time.endswith(':00:00Z')])}")
    print(f"分钟级降水强度数组长度: {len(forecast.minutely_intensity_mmh)}")
    print(f"分钟级描述: {forecast.minutely_description}")
    print()

    print("解析结果（前4个小时级周期）:")
    for p in forecast.periods[:4]:
        d = p.to_dict()
        print(f"  {d['valid_time']} | temp={d['temp_c']}C | "
              f"precip_prob={d['precip_prob']} | precip_mm={d['precip_amount_mm']} | "
              f"wind={d['wind_speed_kph']}kph | pressure={d['pressure_hpa']}hPa | "
              f"vis={d['visibility_km']}km")

    # 验证
    assert forecast.source.source == "caiyun"
    assert len(forecast.minutely_intensity_mmh) == 120
    assert forecast.periods[0].temperature.value_c == 8.5
    assert forecast.periods[0].precipitation.probability == 0.2
    assert forecast.periods[0].wind.speed_kph == 15.0
    assert forecast.periods[0].pressure_hpa == 1015.0  # 101500 Pa → 1015.0 hPa
    assert forecast.periods[0].visibility_km == 15.0   # 15000 m → 15.0 km
    print("\n[OK] 彩云天气解析测试通过")

    # 测试日级数据
    daily_periods = [p for p in forecast.periods if p.temperature.high_c is not None]
    print(f"\n日级周期数: {len(daily_periods)}")
    for p in daily_periods:
        print(f"  {p.valid_time} | high={p.temperature.high_c}C | low={p.temperature.low_c}C | precip={p.precipitation.amount_mm}mm")
    assert len(daily_periods) == 2
    assert daily_periods[0].temperature.high_c == 12.0
    assert daily_periods[0].temperature.low_c == 2.0
    print("[OK] 日级数据解析测试通过")

    return forecast


def test_formatter(noaa_fc, caiyun_fc):
    """测试格式化输出。"""
    print("\n" + "=" * 60)
    print("测试格式化输出")
    print("=" * 60)

    # JSON 序列化
    json_str = to_json(noaa_fc)
    parsed = json.loads(json_str)
    assert parsed["location_name"] == "New York"
    print(f"NOAA JSON 长度: {len(json_str)} 字符")
    print("[OK] JSON 序列化测试通过")

    # 摘要
    print("\n--- NOAA 预报摘要 ---")
    print(summary(noaa_fc))

    print("\n--- 彩云天气预报摘要 ---")
    print(summary(caiyun_fc))
    print("\n[OK] 格式化输出测试通过")


def main():
    print("气象数据接入引擎 - Mock 数据测试")
    print(f"Python: {sys.version.split()[0]}")
    print()

    noaa_fc = test_noaa_parse()
    caiyun_fc = test_caiyun_parse()
    test_formatter(noaa_fc, caiyun_fc)

    print("\n" + "=" * 60)
    print("全部测试通过！")
    print("=" * 60)


if __name__ == "__main__":
    main()
