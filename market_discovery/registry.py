"""市场发现模块 -- 市场注册表。

职责:
  - 维护"已见过"的天气盘列表，按 condition_id 去重。
  - JSON 持久化到 data/market_registry.json，重启不丢状态。
  - diff() 对比最新扫描结果，找出真正"新开的盘"。
"""

from __future__ import annotations

import json
import logging
import os
from datetime import datetime, timezone
from typing import Dict, List, Optional

from .models import MarketInfo

logger = logging.getLogger(__name__)

DEFAULT_REGISTRY_PATH = "./data/market_registry.json"


class MarketRegistry:
    """增量市场注册表。

    storage: 内部结构
      {
        "version": 1,
        "last_scan_utc": "ISO",
        "markets": {
          "<condition_id>": {
            **MarketInfo.to_dict(),
            "first_seen_utc": "ISO",
            "last_seen_utc": "ISO",
          }
        }
      }
    """

    def __init__(self, path: str = DEFAULT_REGISTRY_PATH):
        self.path = path
        self._markets: Dict[str, dict] = {}  # condition_id -> record
        self.last_scan_utc: Optional[str] = None
        self._load()

    # ----------------------------------------------------------------
    # 持久化
    # ----------------------------------------------------------------

    def _load(self):
        if not os.path.exists(self.path):
            logger.info("注册表不存在 (%s)，从空状态开始", self.path)
            return
        try:
            with open(self.path, "r", encoding="utf-8") as f:
                data = json.load(f)
            self._markets = data.get("markets", {})
            self.last_scan_utc = data.get("last_scan_utc")
            logger.info(
                "注册表加载: %d 个市场 (%s)", len(self._markets), self.path
            )
        except (json.JSONDecodeError, OSError) as e:
            logger.warning("注册表读取失败 (%s)，从空状态开始: %s", self.path, e)

    def save(self):
        os.makedirs(os.path.dirname(self.path) or ".", exist_ok=True)
        payload = {
            "version": 1,
            "last_scan_utc": self.last_scan_utc,
            "markets": self._markets,
        }
        tmp = f"{self.path}.tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(payload, f, ensure_ascii=False, indent=2)
        os.replace(tmp, self.path)  # 原子替换，避免写一半损坏

    # ----------------------------------------------------------------
    # 增量对比
    # ----------------------------------------------------------------

    def diff(self, scanned: List[MarketInfo]) -> List[MarketInfo]:
        """对比扫描结果与已见列表，返回新出现的市场。

        已见市场会刷新 last_seen；新市场记 first_seen 并返回。
        """
        now_iso = datetime.now(timezone.utc).isoformat()
        self.last_scan_utc = now_iso

        new_markets: List[MarketInfo] = []
        for m in scanned:
            rec = self._markets.get(m.condition_id)
            if rec is None:
                # 新盘
                record = m.to_dict()
                record["first_seen_utc"] = now_iso
                record["last_seen_utc"] = now_iso
                self._markets[m.condition_id] = record
                new_markets.append(m)
            else:
                # 已见: 刷新时间戳 + 最新状态（活跃度可能变化）
                rec["last_seen_utc"] = now_iso
                rec.update(m.to_dict())

        self.save()
        return new_markets

    def record_seen(self, scanned: List[MarketInfo]):
        """仅登记（不返回新盘），供不关心新盘的调用方使用。"""
        self.diff(scanned)

    # ----------------------------------------------------------------
    # 查询
    # ----------------------------------------------------------------

    def known_market_ids(self) -> set:
        return set(self._markets.keys())

    def count(self) -> int:
        return len(self._markets)

    def all_markets(self) -> List[dict]:
        return list(self._markets.values())

    def summary(self) -> dict:
        """注册表状态摘要（供日志/CLI 输出）。"""
        total = len(self._markets)
        active = sum(
            1 for r in self._markets.values() if not r.get("closed", False)
        )
        by_event: Dict[str, int] = {}
        for r in self._markets.values():
            et = r.get("event_type", "rain")
            by_event[et] = by_event.get(et, 0) + 1
        return {
            "total": total,
            "active": active,
            "by_event_type": by_event,
            "last_scan_utc": self.last_scan_utc,
            "path": self.path,
        }
