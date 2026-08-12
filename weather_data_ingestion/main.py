"""
main.py - CLI 入口

用法:
    # 默认采集（纽约 + 北京，NOAA + 彩云）
    python -m weather_data_ingestion.main

    # 指定城市
    python -m weather_data_ingestion.main --city "New York:40.7128,-74.0060"

    # 仅 NOAA
    python -m weather_data_ingestion.main --no-caiyun

    # 输出 CSV
    python -m weather_data_ingestion.main --csv

    # 打印摘要
    python -m weather_data_ingestion.main --summary

环境变量:
    CAIYUN_API_TOKEN  - 彩云天气 API Token（必需，如启用彩云）
    NOAA_USER_AGENT   - NOAA 请求 User-Agent（可选）
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import sys

from .config import Location, load_config
from .pipeline import IngestionPipeline


def parse_city_arg(city_str: str) -> Location:
    """
    解析 --city 参数: "城市名:纬度,经度"

    例如: "New York:40.7128,-74.0060"
          "北京:39.9042,116.4074"
    """
    parts = city_str.split(":")
    if len(parts) != 2:
        raise ValueError(f"城市参数格式错误，应为 '名称:纬度,经度'，得到: {city_str}")
    name = parts[0].strip()
    coords = parts[1].strip().split(",")
    if len(coords) != 2:
        raise ValueError(f"坐标格式错误，应为 '纬度,经度'，得到: {parts[1]}")
    lat = float(coords[0].strip())
    lon = float(coords[1].strip())
    return Location(name=name, lat=lat, lon=lon)


def setup_logging(verbose: bool = False) -> None:
    level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )


async def run_async(args: argparse.Namespace) -> int:
    """异步主逻辑。"""
    # 解析城市
    locations = []
    for city_str in args.city:
        try:
            locations.append(parse_city_arg(city_str))
        except ValueError as e:
            print(f"错误: {e}", file=sys.stderr)
            return 1

    # 加载配置
    config = load_config(
        locations=locations or None,
        enable_noaa=not args.no_noaa,
        enable_caiyun=not args.no_caiyun,
        timeout=args.timeout,
        output_dir=args.output_dir,
    )

    if not config.active_sources:
        print("错误: 未启用任何数据源", file=sys.stderr)
        return 1

    # 运行管线
    async with IngestionPipeline(config) as pipeline:
        forecasts = await pipeline.run(
            locations=config.locations,
            save_to_disk=not args.no_save,
        )

        if not forecasts:
            print("警告: 所有采集任务均失败", file=sys.stderr)
            return 1

        # 输出
        if args.summary:
            for fc in forecasts:
                print(pipeline.format_summary(fc))
                print()

        if args.csv:
            df = pipeline.to_dataframe(forecasts)
            csv_path = f"{args.output_dir}/combined_output.csv"
            df.to_csv(csv_path, index=False, encoding="utf-8-sig")
            print(f"CSV 已保存: {csv_path}")
            print(f"共 {len(df)} 行数据")

        if args.dataframe:
            df = pipeline.to_dataframe(forecasts)
            print(f"\nDataFrame 预览 (前 10 行):")
            print(df.head(10).to_string())

        print(f"\n完成: 成功采集 {len(forecasts)} 个预报")

    return 0


def main() -> None:
    """CLI 入口。"""
    parser = argparse.ArgumentParser(
        description="气象数据接入与融合引擎 - NOAA & 彩云天气",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
示例:
  python -m weather_data_ingestion.main
  python -m weather_data_ingestion.main --city "New York:40.7128,-74.0060" --summary
  python -m weather_data_ingestion.main --no-caiyun --csv
  python -m weather_data_ingestion.main --verbose --dataframe
        """,
    )
    parser.add_argument(
        "--city", action="append", default=[],
        metavar="NAME:LAT,LON",
        help="目标城市，格式 '名称:纬度,经度'，可多次指定",
    )
    parser.add_argument("--no-noaa", action="store_true", help="禁用 NOAA 数据源")
    parser.add_argument("--no-caiyun", action="store_true", help="禁用彩云天气数据源")
    parser.add_argument("--timeout", type=float, default=30.0, help="请求超时（秒）")
    parser.add_argument("--output-dir", default="./data/raw", help="输出目录")
    parser.add_argument("--no-save", action="store_true", help="不保存到磁盘")
    parser.add_argument("--summary", action="store_true", help="打印预报摘要")
    parser.add_argument("--csv", action="store_true", help="输出合并 CSV 文件")
    parser.add_argument("--dataframe", action="store_true", help="打印 DataFrame 预览")
    parser.add_argument("-v", "--verbose", action="store_true", help="详细日志")

    args = parser.parse_args()
    setup_logging(args.verbose)

    exit_code = asyncio.run(run_async(args))
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
