"""
radar_source.py - NEXRAD 雷达回波数据源

接入美国 NEXRAD 雷达网络实时反射率数据，填补 RadarObservation 中
一直为 None 的 `radar_reflectivity_dbz` 字段。

数据来源：Iowa Environmental Mesonet (IEM) — 免费、无需 API Key
  - 雷达站列表: /json/radar.py?operation=available
  - 最新数据:   /json/radar.py?operation=latest&site=KOKX

流程：
  1. 根据目标坐标找到最近 NEXRAD 站点
  2. 查询该站点最新雷达数据
  3. 提取最大反射率 (dBZ)
  4. 返回 RadarObservation 供 NowcastCorrectionStrategy 消费
"""

from __future__ import annotations

import logging
import math
from datetime import datetime, timezone
from typing import Optional

from ..config import Location
from ..models import RadarObservation
from .base import BaseWeatherSource

logger = logging.getLogger(__name__)

# IEM Radar API
IEM_RADAR_BASE = "https://mesonet.agron.iastate.edu/json/radar.py"

# NEXRAD 雷达站点经纬度 (美国本土主要站点，从 IEM 获取)
_NEXRAD_SITES = {
    "KABR": (45.45, -98.41), "KABX": (35.15, -106.82), "KAKQ": (36.98, -77.01),
    "KAMA": (35.23, -101.71), "KAMX": (25.61, -80.41), "KAPX": (44.91, -84.72),
    "KARX": (43.82, -91.19), "KATX": (48.19, -122.50), "KBBX": (39.50, -121.63),
    "KBGM": (42.20, -75.98), "KBHX": (40.50, -124.29), "KBIS": (46.77, -100.76),
    "KBLX": (45.85, -108.61), "KBMX": (33.17, -86.77), "KBOX": (41.96, -71.14),
    "KBRO": (25.92, -97.42), "KBUF": (42.95, -78.74), "KBYX": (24.60, -81.70),
    "KCAE": (33.95, -81.12), "KCBW": (46.04, -67.81), "KCBX": (43.49, -116.24),
    "KCCX": (40.92, -78.00), "KCLE": (41.41, -81.86), "KCLX": (32.66, -81.04),
    "KCRP": (27.78, -97.51), "KCXX": (44.51, -73.17), "KCYS": (41.15, -104.81),
    "KDAX": (38.50, -121.68), "KDDC": (37.76, -99.97), "KDFX": (29.27, -100.28),
    "KDGX": (32.28, -89.98), "KDIX": (39.95, -74.41), "KDLH": (46.84, -92.21),
    "KDMX": (41.73, -93.72), "KDOX": (38.83, -75.44), "KDTX": (42.70, -83.47),
    "KDVN": (41.61, -90.58), "KDYX": (32.54, -99.25), "KEAX": (38.81, -94.26),
    "KEMX": (31.89, -110.63), "KENX": (42.59, -74.06), "KEOX": (31.46, -85.46),
    "KEPZ": (31.87, -106.70), "KESX": (35.70, -114.89), "KEVX": (30.57, -85.92),
    "KEWX": (29.70, -98.03), "KEYX": (35.10, -117.56), "KFCX": (37.02, -80.27),
    "KFDR": (34.36, -98.98), "KFDX": (34.63, -103.62), "KFFC": (33.36, -84.57),
    "KFSD": (43.58, -96.73), "KFSX": (34.57, -111.20), "KFTG": (39.79, -104.55),
    "KFWS": (32.57, -97.30), "KGGW": (48.21, -106.63), "KGJX": (39.06, -108.21),
    "KGLD": (39.37, -101.70), "KGRB": (44.50, -88.11), "KGRK": (30.72, -97.38),
    "KGRR": (42.89, -85.54), "KGSP": (34.88, -82.22), "KGWX": (33.90, -89.34),
    "KGYX": (43.89, -70.26), "KHDX": (33.08, -106.12), "KHGX": (29.47, -95.08),
    "KHNX": (36.31, -119.63), "KHPX": (36.74, -87.28), "KHTX": (34.93, -86.08),
    "KHUN": (34.86, -86.77), "KICX": (37.59, -112.86), "KILN": (39.42, -83.82),
    "KILX": (40.15, -89.34), "KIND": (39.71, -86.28), "KINX": (36.18, -95.56),
    "KIWA": (33.29, -111.67), "KJAX": (30.48, -81.70), "KJGX": (32.68, -83.35),
    "KJKL": (37.59, -83.31), "KLBB": (33.65, -101.81), "KLCH": (30.13, -93.22),
    "KLIX": (30.34, -89.83), "KLNX": (41.96, -100.58), "KLOT": (41.60, -88.08),
    "KLRX": (40.74, -116.80), "KLSX": (38.70, -90.68), "KLTX": (33.99, -78.43),
    "KLVX": (37.98, -85.94), "KLWX": (38.98, -77.49), "KMAX": (42.08, -122.72),
    "KMBX": (48.39, -100.86), "KMHX": (34.78, -76.88), "KMKX": (42.97, -88.55),
    "KMLB": (28.11, -80.65), "KMOB": (30.68, -88.24), "KMPX": (44.85, -93.57),
    "KMQT": (46.53, -87.55), "KMRX": (36.17, -83.40), "KMSX": (47.04, -113.99),
    "KMTX": (41.26, -112.45), "KMUX": (37.16, -121.90), "KMVX": (47.53, -97.33),
    "KMXX": (32.54, -85.79), "KNKX": (32.92, -117.04), "KNQA": (35.35, -89.87),
    "KOAX": (41.32, -96.37), "KOHX": (36.25, -86.57), "KOKX": (40.87, -72.86),
    "KOTX": (47.68, -117.63), "KPAH": (37.07, -88.77), "KPBZ": (40.53, -80.22),
    "KPDT": (45.69, -118.85), "KPOE": (31.16, -92.98), "KPUX": (38.46, -104.18),
    "KRAX": (35.67, -78.49), "KRGX": (39.75, -119.46), "KRIW": (43.07, -108.48),
    "KRLX": (38.31, -81.72), "KRTX": (45.72, -122.97), "KSFX": (43.11, -112.69),
    "KSGF": (37.24, -93.12), "KSHV": (32.45, -93.84), "KSJT": (31.37, -100.49),
    "KSOX": (33.82, -117.64), "KSRX": (35.29, -94.36), "KTBW": (27.71, -82.40),
    "KTFX": (47.46, -111.39), "KTLH": (30.40, -84.33), "KTLX": (35.33, -97.28),
    "KTWX": (38.99, -96.23), "KTYX": (43.76, -75.68), "KUDX": (44.12, -102.83),
    "KUEX": (40.32, -98.44), "KVAX": (30.89, -83.00), "KVBX": (34.84, -120.40),
    "KVNX": (36.74, -98.13), "KVTX": (34.41, -119.18), "KVWX": (38.26, -87.72),
    "KYUX": (32.50, -114.66),
}

# NOAA NEXRAD Level III 公开数据 URL
NOAA_RADAR_BASE = "https://www.ncei.noaa.gov/access/radar"


class RadarDataSource(BaseWeatherSource):
    """
    NEXRAD 雷达回波数据源。

    从 Iowa Environmental Mesonet 获取 NEXRAD 雷达实时反射率数据，
    供给 NowcastCorrectionStrategy 做临近修正。
    """

    SOURCE_NAME = "radar"

    def __init__(self, session, cache, timeout=20.0, retry_count=2, retry_backoff=1.5):
        super().__init__(session, cache, timeout, retry_count, retry_backoff)

    # ------------------------------------------------------------------
    # 公开 API
    # ------------------------------------------------------------------

    async def fetch(self, location: Location) -> RadarObservation:
        """获取指定位置的实时雷达观测。"""
        # 找到最近雷达站
        site = self._nearest_radar(location.lat, location.lon)
        if not site:
            return RadarObservation(
                station_id="UNKNOWN",
                station_name=f"No NEXRAD near {location.name}",
                lat=location.lat, lon=location.lon,
                timestamp=datetime.now(timezone.utc).isoformat(),
            )

        self._current_site = site
        raw = await self._fetch_with_retry(location)
        return self._parse(raw, location, site)

    # ------------------------------------------------------------------
    # 核心实现
    # ------------------------------------------------------------------

    async def _fetch_raw(self, location: Location) -> dict:
        """获取 NEXRAD 站点最新雷达数据。"""
        site = getattr(self, '_current_site', 'KOKX')
        url = f"{IEM_RADAR_BASE}?operation=latest&site={site}&product=N0Q"
        return await self._http_get(url)

    def _parse(self, raw: dict, location: Location, site: str) -> RadarObservation:
        """解析 IEM 雷达 JSON 响应，提取反射率信息。"""
        now = datetime.now(timezone.utc)

        # IEM 返回格式: {"site": "KOKX", "product": "N0Q", "radars": [...], "generated_at": "..."}
        radars = raw.get("radars", [])
        max_dbz = None
        station_name = site

        if radars:
            # 提取所有方位的最大反射率作为该站点的代表性反射率
            dbz_values = []
            for r in radars:
                try:
                    val = float(r.get("value", 0))
                    if val > 0:
                        dbz_values.append(val)
                except (TypeError, ValueError):
                    continue

            if dbz_values:
                max_dbz = round(max(dbz_values), 1)
                logger.debug(
                    "[radar] %s 最大反射率: %.1f dBZ (%d 个有效点)",
                    site, max_dbz, len(dbz_values)
                )

        return RadarObservation(
            station_id=site,
            station_name=station_name,
            lat=location.lat,
            lon=location.lon,
            timestamp=raw.get("generated_at", now.isoformat()),
            radar_reflectivity_dbz=max_dbz,
        )

    # ------------------------------------------------------------------
    # 雷达站定位
    # ------------------------------------------------------------------

    @staticmethod
    def _nearest_radar(lat: float, lon: float) -> Optional[str]:
        """根据坐标找最近的 NEXRAD 站点（美国大陆）。"""
        min_dist = float("inf")
        nearest = None
        for site, (rlat, rlon) in _NEXRAD_SITES.items():
            dist = RadarDataSource._haversine(lat, lon, rlat, rlon)
            if dist < min_dist:
                min_dist = dist
                nearest = site
        # 如果最近站点超过 500km，认为超出覆盖范围
        if min_dist > 500:
            return None
        return nearest

    @staticmethod
    def _haversine(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
        """Haversine 距离 (km)。"""
        R = 6371.0
        dlat = math.radians(lat2 - lat1)
        dlon = math.radians(lon2 - lon1)
        a = math.sin(dlat / 2) ** 2 + math.cos(math.radians(lat1)) * math.cos(
            math.radians(lat2)
        ) * math.sin(dlon / 2) ** 2
        return R * 2 * math.atan2(math.sqrt(a), math.sqrt(1 - a))
