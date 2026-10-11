"""R5-003：`clean_forecast` 不可变契约回归测试。

`clean_forecast` docstring 声明「返回新的 WeatherForecast（不修改原始对象）」，
但此前实现是原地修改（`_clean_period_fields` 直接改字段、
`forecast.periods = periods`）。本测试锁定「深拷贝后返回新对象」的契约。
"""

from weather_data_ingestion.cleaning import clean_forecast
from weather_data_ingestion.models import ForecastPeriod, SourceMeta, WeatherForecast


def _make_forecast(temp_values):
    periods = []
    for i, t in enumerate(temp_values):
        p = ForecastPeriod(valid_time=f"2026-01-01T0{i}:00:00Z")
        p.temperature.value_c = t
        periods.append(p)
    return WeatherForecast(
        location_name="Testville",
        source=SourceMeta(source="noaa"),
        periods=periods,
    )


def test_clean_forecast_returns_new_object():
    fc = _make_forecast([5.0, 999.0, 7.0])
    original_temps = [p.temperature.value_c for p in fc.periods]

    cleaned = clean_forecast(fc)

    # 返回新对象，不修改原始快照
    assert cleaned is not fc
    assert [p.temperature.value_c for p in fc.periods] == original_temps
    assert fc.periods[1].temperature.value_c == 999.0

    # 清洗后的副本中异常值已被处理（置 None 后插值）
    assert 999.0 not in [p.temperature.value_c for p in cleaned.periods]


def test_clean_forecast_empty_periods_returns_copy():
    fc = _make_forecast([])

    cleaned = clean_forecast(fc)

    assert cleaned is not fc
    assert fc.periods == []
    assert cleaned.periods == []
