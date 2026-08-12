"""
cache.py - 轻量 TTL 缓存

对 API 响应做短期缓存，避免高频请求触发限流。
基于内存字典 + 过期时间戳，适合单进程场景。
后续如需多进程共享可替换为 Redis。
"""

from __future__ import annotations

import time
from typing import Any, Optional


class TTLCache:
    """简单的 TTL（生存时间）缓存。"""

    def __init__(self, ttl_seconds: int = 300):
        self._ttl = ttl_seconds
        self._store: dict[str, tuple[float, Any]] = {}

    def get(self, key: str) -> Optional[Any]:
        entry = self._store.get(key)
        if entry is None:
            return None
        expire_at, value = entry
        if time.monotonic() > expire_at:
            # 已过期，清理
            del self._store[key]
            return None
        return value

    def set(self, key: str, value: Any) -> None:
        expire_at = time.monotonic() + self._ttl
        self._store[key] = (expire_at, value)

    def clear(self) -> None:
        self._store.clear()

    def __len__(self) -> int:
        return len(self._store)
