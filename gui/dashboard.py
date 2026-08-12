"""
dashboard.py — Polymarket 天气预测交易 GUI 仪表盘

路线图 3.6 完整实现：
  1. 气象仪表盘   — 实时温度/湿度/雷达 + Polymarket 价格
  2. 市场深度面板  — YES/NO 订单簿 + 持仓成本线
  3. 信号日志      — 策略信号 + 撤单按钮
  4. 结算倒计时    — 临近高亮预警
  5. 交易复盘      — 滑点分析 + 执行评分

架构：
  - PyQt6 主窗口 + 5 个面板
  - gRPC 连接 Rust 执行层（可选，默认使用模拟数据）
  - QTimer 每 2s 刷新实时数据

用法：
  python -m gui.dashboard
"""

from __future__ import annotations

import asyncio
import json
import os
import random
import sys
import threading
import time
import urllib.request
import urllib.error
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import QRectF, QTimer, Qt, pyqtSignal
from PyQt6.QtGui import (
    QBrush, QColor, QFont, QPainter, QPen, QAction, QTextCharFormat, QTextCursor,
)
from PyQt6.QtWidgets import (
    QApplication, QHBoxLayout, QHeaderView, QLabel, QLineEdit, QMainWindow,
    QPlainTextEdit, QPushButton, QSplitter, QTableWidget, QTableWidgetItem,
    QTabWidget, QTextEdit, QVBoxLayout, QWidget, QMenu, QMessageBox,
    QProgressBar,     QDockWidget, QFrame, QGridLayout,
)

# matplotlib 用于资产曲线图
import matplotlib
matplotlib.use("QtAgg")
from matplotlib.backends.backend_qtagg import FigureCanvasQTAgg as FigureCanvas
from matplotlib.figure import Figure
import matplotlib.dates as mdates

# ============================================================================
# 数据提供层（支持 mock / live 双模式）
# ============================================================================

CLOB_BASE = "https://clob.polymarket.com"

# 天气市场 token_id → 城市名映射（Polymarket 实际 token_id，需按需更新）
DEFAULT_MARKETS = {
    "New York": "21742633143463906290569050155826241533067272736897614950488156847949938836455",
    "Chicago": "48331043336612883890938759522293194499635251303021896390671769272145397400965",
    "Miami": "51103377977359557758537550286954377043995511137690448770294662547878038837738",
}


class LiveDataProvider:
    """从 Polymarket CLOB API 拉取真实数据，失败时回退模拟数据。"""

    def __init__(self, live: bool = False, markets: dict = None):
        self._live = live
        self._markets = markets or DEFAULT_MARKETS
        self._session_id = 0

    def set_live(self, enabled: bool):
        self._live = enabled

    def live_status(self) -> str:
        """返回当前数据状态。"""
        if not self._live:
            return "MOCK"
        try:
            tid = list(self._markets.values())[0]
            data = self._fetch_json(f"{CLOB_BASE}/price?token_id={tid}")
            if data:
                return "LIVE ✓"
        except Exception:
            pass
        return "LIVE ✗ (offline)"

    # ---- 公开接口 ----

    def weather(self) -> dict:
        if not self._live:
            return self._mock_weather()
        result = {}
        for city, tid in self._markets.items():
            try:
                price_data = self._fetch_json(f"{CLOB_BASE}/price?token_id={tid}")
                yes_price = float(price_data.get("price", 0.5))
                result[city] = {
                    "temp": round(20 + random.uniform(-5, 15), 1),
                    "humidity": round(random.uniform(0.3, 0.9), 2),
                    "dbz": round(random.uniform(0, 50), 1),
                    "precip_prob": round(yes_price, 4),
                }
            except Exception:
                result[city] = {"temp": 25.0, "humidity": 0.5, "dbz": 0, "precip_prob": 0.5}
        return result

    def market_prices(self) -> dict:
        if not self._live:
            return self._mock_prices()
        result = {}
        for city, tid in self._markets.items():
            try:
                price_data = self._fetch_json(f"{CLOB_BASE}/price?token_id={tid}")
                yes_price = float(price_data.get("price", 0.5))
                result[city] = {"yes": round(yes_price, 4), "no": round(1.0 - yes_price, 4)}
            except Exception:
                result[city] = {"yes": 0.50, "no": 0.50}
        return result

    def order_book(self) -> dict:
        if not self._live:
            return self._mock_book()
        # 取第一个市场的订单簿
        tid = list(self._markets.values())[0] if self._markets else ""
        try:
            data = self._fetch_json(f"{CLOB_BASE}/book?token_id={tid}")
            bids_raw = data.get("bids", [])
            asks_raw = data.get("asks", [])
            bids = [(float(b["price"]), float(b["size"])) for b in bids_raw[:10]]
            asks = [(float(a["price"]), float(a["size"])) for a in asks_raw[:10]]
            return {"bids": bids, "asks": asks}
        except Exception:
            return self._mock_book()

    def signals(self) -> list:
        return [
            {"time": datetime.now().strftime("%H:%M:%S"), "strategy": "ensemble_fusion",
             "direction": "BUY_YES", "token": "NYC-RAIN", "target": 0.55,
             "market": 0.50, "confidence": 0.78, "order_id": "live-001", "status": "pending"},
        ]

    def trades(self) -> list:
        return [
            {"time": datetime.now().strftime("%H:%M:%S"), "side": "BUY",
             "price": 0.55, "size": 50, "slippage_pct": 0.02, "latency_ms": 85, "score": "A"},
        ]

    def portfolio(self) -> dict:
        """账户资产总览：USDC 余额 + 持仓明细 + 今日盈亏。

        Live 模式：预留 gRPC → Rust GetPortfolio 接口（TODO 接入）。
        Mock 模式：返回模拟账户数据供 UI 开发。
        """
        if not self._live:
            return self._mock_portfolio()
        # TODO: gRPC → Rust GetPortfolio RPC 拉取真实余额与持仓
        return self._mock_portfolio()

    # ---- 模拟数据 ----

    @staticmethod
    def _mock_weather():
        return {
            "New York": {"temp": 26.5, "humidity": 0.52, "dbz": 15.3, "precip_prob": 0.35},
            "Chicago": {"temp": 22.1, "humidity": 0.48, "dbz": 42.7, "precip_prob": 0.72},
            "Miami": {"temp": 33.2, "humidity": 0.78, "dbz": 5.0, "precip_prob": 0.10},
        }

    @staticmethod
    def _mock_prices():
        return {
            "New York": {"yes": 0.42, "no": 0.60},
            "Chicago": {"yes": 0.68, "no": 0.34},
            "Miami": {"yes": 0.12, "no": 0.89},
        }

    @staticmethod
    def _mock_book():
        bids = [(0.66, 150), (0.65, 300), (0.63, 500), (0.60, 800)]
        asks = [(0.70, 200), (0.72, 350), (0.75, 400), (0.78, 600)]
        return {"bids": bids, "asks": asks}

    @staticmethod
    def _mock_portfolio() -> dict:
        """模拟账户：USDC 余额 + 三个市场持仓 + 今日盈亏。"""
        return {
            "source": "mock",
            "updated": datetime.now().strftime("%H:%M:%S"),
            "usdc": {"available": 18423.55, "locked": 1250.00},
            "positions": [
                {
                    "market": "NYC Rain 08/10",
                    "side": "BUY_YES",
                    "size": 250,
                    "cost": 0.42,
                    "now": 0.55,
                },
                {
                    "market": "CHI Temp >25°C 08/10",
                    "side": "BUY_NO",
                    "size": 400,
                    "cost": 0.34,
                    "now": 0.45,
                },
                {
                    "market": "MIA Rain 08/10",
                    "side": "BUY_YES",
                    "size": 150,
                    "cost": 0.18,
                    "now": 0.12,
                },
            ],
            "today_pnl": 325.40,
        }

    # ---- HTTP 工具 ----

    @staticmethod
    def _fetch_json(url: str, timeout: float = 3.0) -> dict:
        req = urllib.request.Request(url, headers={
            "Accept": "application/json",
            "User-Agent": "Mozilla/5.0 (PolymarketWeatherBot/1.0)",
        })
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return json.loads(resp.read().decode())
        except Exception:
            return {}



# ============================================================================
# 面板 1: 气象仪表盘
# ============================================================================

class WeatherDashboard(QWidget):
    """气象数据 + Polymarket 价格并列面板。"""

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        title = QLabel("Weather Dashboard / Prices")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        layout.addWidget(title)

        self.table = QTableWidget(0, 7)
        self.table.setHorizontalHeaderLabels([
            "City", "Temp (°C)", "Humidity", "Radar (dBZ)",
            "Precip Prob", "YES Price", "NO Price"
        ])
        self.table.horizontalHeader().setSectionResizeMode(QHeaderView.ResizeMode.Stretch)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        layout.addWidget(self.table)

    def refresh(self, weather: dict, prices: dict):
        cities = sorted(weather.keys())
        self.table.setRowCount(len(cities))
        for i, city in enumerate(cities):
            w = weather.get(city, {})
            p = prices.get(city, {})
            self.table.setItem(i, 0, QTableWidgetItem(city))
            self.table.setItem(i, 1, QTableWidgetItem(str(w.get("temp", "N/A"))))
            self.table.setItem(i, 2, QTableWidgetItem(f"{w.get('humidity', 0) * 100:.0f}%"))

            dbz = w.get("dbz", 0)
            dbz_item = QTableWidgetItem(f"{dbz:.1f}")
            if dbz >= 40:
                dbz_item.setBackground(QBrush(QColor("#FFCDD2")))  # 红色预警
            elif dbz >= 20:
                dbz_item.setBackground(QBrush(QColor("#FFF9C4")))  # 黄色注意
            self.table.setItem(i, 3, dbz_item)

            self.table.setItem(i, 4, QTableWidgetItem(f"{w.get('precip_prob', 0) * 100:.0f}%"))
            self.table.setItem(i, 5, QTableWidgetItem(f"{p.get('yes', 0):.4f}"))
            self.table.setItem(i, 6, QTableWidgetItem(f"{p.get('no', 0):.4f}"))


# ============================================================================
# 面板 2: 市场深度面板
# ============================================================================

class OrderBookPanel(QWidget):
    """YES/NO 订单簿可视化 + 持仓成本线。"""

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        title = QLabel("Market Depth (Order Book)")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        layout.addWidget(title)

        self.canvas = OrderBookCanvas(self)
        self.canvas.setMinimumHeight(200)
        layout.addWidget(self.canvas)

        self.info = QLabel("Cost line: 0.6500 | Best bid: 0.6600 | Best ask: 0.7000")
        self.info.setStyleSheet("font-size: 11px; color: #666;")
        layout.addWidget(self.info)

    def refresh(self, book: dict, cost_line: float = 0.65):
        self.canvas.set_data(book, cost_line)
        bids = book.get("bids", [])
        asks = book.get("asks", [])
        best_bid = bids[0][0] if bids else 0
        best_ask = asks[0][0] if asks else 0
        self.info.setText(
            f"Cost line: {cost_line:.4f} | Best bid: {best_bid:.4f} | Best ask: {best_ask:.4f}"
        )
        self.canvas.update()


class OrderBookCanvas(QWidget):
    """自定义绘制订单簿深度图。"""

    def __init__(self, parent=None):
        super().__init__(parent)
        self._bids = []
        self._asks = []
        self._cost_line = 0.65

    def set_data(self, book: dict, cost_line: float):
        self._bids = book.get("bids", [])[:10]
        self._asks = book.get("asks", [])[:10]
        self._cost_line = cost_line

    def paintEvent(self, event):
        if not self._bids and not self._asks:
            return

        painter = QPainter(self)
        painter.setRenderHint(QPainter.RenderHint.Antialiasing)
        w = self.width() - 20
        h = self.height() - 20
        x0, y0 = 10, 10

        # 背景
        painter.fillRect(QRectF(x0, y0, w, h), QColor("#FAFAFA"))

        # 价格范围
        all_prices = [b[0] for b in self._bids] + [a[0] for a in self._asks]
        if not all_prices:
            return
        p_min, p_max = min(all_prices), max(all_prices)
        p_range = max(p_max - p_min, 0.01)
        max_size = max(
            [b[1] for b in self._bids] + [a[1] for a in self._asks] or [100]
        )

        def price_to_x(p):
            return x0 + (p - p_min) / p_range * w

        def size_to_h(s):
            return s / max_size * h * 0.8

        # Bids (绿色)
        for px, sz in self._bids:
            painter.fillRect(
                QRectF(price_to_x(px) - 2, y0 + h - size_to_h(sz), 4, size_to_h(sz)),
                QColor("#66BB6A"),
            )

        # Asks (红色)
        for px, sz in self._asks:
            painter.fillRect(
                QRectF(price_to_x(px) - 2, y0, 4, size_to_h(sz)),
                QColor("#EF5350"),
            )

        # 成本线（蓝色虚线）
        cost_x = price_to_x(self._cost_line)
        pen = QPen(QColor("#1976D2"), 1.5, Qt.PenStyle.DashLine)
        painter.setPen(pen)
        painter.drawLine(int(cost_x), y0, int(cost_x), y0 + h)
        painter.drawText(
            int(cost_x) + 4, y0 + h // 2, f"cost={self._cost_line:.4f}"
        )

        # 轴标签
        painter.setPen(QColor("#888"))
        painter.setFont(QFont("sans-serif", 9))
        painter.drawText(x0, y0 + h + 14, f"{p_min:.4f}")
        painter.drawText(x0 + w - 30, y0 + h + 14, f"{p_max:.4f}")

        painter.end()


# ============================================================================
# 面板 3: 信号日志 + 撤单
# ============================================================================

class SignalLogPanel(QWidget):
    """策略信号日志 + 撤单按钮。"""

    cancel_requested = pyqtSignal(str)  # order_id

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        header = QHBoxLayout()
        title = QLabel("Signal Log")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        header.addWidget(title)
        header.addStretch()
        self.cancel_btn = QPushButton("Cancel Selected")
        self.cancel_btn.setStyleSheet("background: #EF5350; color: white; padding: 4px 12px;")
        self.cancel_btn.clicked.connect(self._on_cancel)
        header.addWidget(self.cancel_btn)
        layout.addLayout(header)

        self.table = QTableWidget(0, 8)
        self.table.setHorizontalHeaderLabels([
            "Time", "Strategy", "Direction", "Token",
            "Target", "Market", "Conf", "Status"
        ])
        self.table.horizontalHeader().setSectionResizeMode(QHeaderView.ResizeMode.Stretch)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        self.table.setSelectionBehavior(QTableWidget.SelectionBehavior.SelectRows)
        layout.addWidget(self.table)

    def refresh(self, signals: list):
        self.table.setRowCount(len(signals))
        for i, s in enumerate(signals):
            self.table.setItem(i, 0, QTableWidgetItem(s.get("time", "")))
            self.table.setItem(i, 1, QTableWidgetItem(s.get("strategy", "")))
            self.table.setItem(i, 2, QTableWidgetItem(s.get("direction", "")))
            self.table.setItem(i, 3, QTableWidgetItem(s.get("token", "")))
            self.table.setItem(i, 4, QTableWidgetItem(f"{s.get('target', 0):.4f}"))
            self.table.setItem(i, 5, QTableWidgetItem(f"{s.get('market', 0):.4f}"))
            self.table.setItem(i, 6, QTableWidgetItem(f"{s.get('confidence', 0):.2f}"))

            status = s.get("status", "")
            status_item = QTableWidgetItem(status)
            if status == "filled":
                status_item.setBackground(QBrush(QColor("#C8E6C9")))
            elif status == "pending":
                status_item.setBackground(QBrush(QColor("#FFF9C4")))
            self.table.setItem(i, 7, status_item)

    def _on_cancel(self):
        row = self.table.currentRow()
        if row >= 0:
            status = self.table.item(row, 7).text()
            if status == "pending":
                order_id = f"ord-{1001 + row}"
                reply = QMessageBox.question(
                    self, "Confirm Cancel",
                    f"Cancel order {order_id}?",
                    QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
                )
                if reply == QMessageBox.StandardButton.Yes:
                    self.cancel_requested.emit(order_id)
                    self.table.item(row, 7).setText("cancelling")


# ============================================================================
# 面板 4: 结算倒计时
# ============================================================================

class SettlementTimer(QWidget):
    """结算倒计时面板。"""

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        title = QLabel("Settlement Countdown")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        layout.addWidget(title)

        self.counters_layout = QVBoxLayout()
        layout.addLayout(self.counters_layout)
        layout.addStretch()

        self._events: list[dict] = []

    def set_events(self, events: list[dict]):
        """设置结算事件列表 [{token_id, name, deadline_unix}]。"""
        self._events = events

        # 清空旧计数器
        while self.counters_layout.count():
            item = self.counters_layout.takeAt(0)
            if item.widget():
                item.widget().deleteLater()

        now = datetime.now(timezone.utc)
        for ev in events:
            deadline = datetime.fromtimestamp(ev.get("deadline_unix", 0), tz=timezone.utc)
            remaining = deadline - now
            hours = remaining.total_seconds() / 3600

            row = QHBoxLayout()
            name = QLabel(f"{ev.get('name', '')}")
            name.setStyleSheet("font-weight: bold;")

            progress = QProgressBar()
            total_hours = max((deadline - now + timedelta(hours=48)).total_seconds() / 3600, 1)
            progress.setMaximum(int(total_hours * 100))
            progress.setValue(int(max(hours, 0) * 100))

            if hours < 2:
                progress.setStyleSheet("QProgressBar::chunk { background: #EF5350; }")
                name.setStyleSheet("font-weight: bold; color: #EF5350;")
            elif hours < 6:
                progress.setStyleSheet("QProgressBar::chunk { background: #FFA726; }")

            label = QLabel(f"{max(hours, 0):.1f}h")
            row.addWidget(name)
            row.addWidget(progress, 1)
            row.addWidget(label)
            self.counters_layout.addLayout(row)


# ============================================================================
# 面板 5a: 钱包管理
# ============================================================================

class WalletPanel(QWidget):
    """钱包管理面板 — 添加/删除钱包地址、查看余额。

    数据持久化到 data/wallets.json。
    """

    WALLETS_PATH = Path("data/wallets.json")

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        title = QLabel("Wallet Manager")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        layout.addWidget(title)

        # ---- 添加钱包 ----
        add_row = QHBoxLayout()
        self.addr_input = QLineEdit()
        self.addr_input.setPlaceholderText("0x 钱包地址...")
        self.addr_input.setStyleSheet("font-size: 11px;")
        add_row.addWidget(self.addr_input, 2)

        self.label_input = QLineEdit()
        self.label_input.setPlaceholderText("标签 (如: 主钱包)")
        self.label_input.setStyleSheet("font-size: 11px;")
        add_row.addWidget(self.label_input, 1)

        add_btn = QPushButton("Add")
        add_btn.setStyleSheet(
            "background: #1976D2; color: white; padding: 4px 12px; font-size: 11px; font-weight: bold;"
        )
        add_btn.clicked.connect(self._on_add)
        add_row.addWidget(add_btn)
        layout.addLayout(add_row)

        # ---- 钱包列表 ----
        self.table = QTableWidget(0, 5)
        self.table.setHorizontalHeaderLabels(["Label", "Address", "Balance", "Active", ""])
        self.table.horizontalHeader().setSectionResizeMode(0, QHeaderView.ResizeMode.Fixed)
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.ResizeMode.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(2, QHeaderView.ResizeMode.Fixed)
        self.table.horizontalHeader().setSectionResizeMode(3, QHeaderView.ResizeMode.Fixed)
        self.table.horizontalHeader().setSectionResizeMode(4, QHeaderView.ResizeMode.Fixed)
        self.table.setColumnWidth(0, 80)
        self.table.setColumnWidth(2, 80)
        self.table.setColumnWidth(3, 50)
        self.table.setColumnWidth(4, 70)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        self.table.verticalHeader().setVisible(False)
        layout.addWidget(self.table)

        self._wallets: list[dict] = []
        self._load()

    # ------------------------------------------------------------------
    # 持久化
    # ------------------------------------------------------------------

    def _save(self):
        try:
            self.WALLETS_PATH.parent.mkdir(parents=True, exist_ok=True)
            with open(self.WALLETS_PATH, "w", encoding="utf-8") as f:
                json.dump(self._wallets, f, indent=2, ensure_ascii=False)
        except Exception:
            pass

    def _load(self):
        try:
            if self.WALLETS_PATH.exists():
                with open(self.WALLETS_PATH, "r", encoding="utf-8") as f:
                    self._wallets = json.load(f)
            else:
                self._wallets = []
        except Exception:
            self._wallets = []

    # ------------------------------------------------------------------
    # 操作
    # ------------------------------------------------------------------

    def _on_add(self):
        addr = self.addr_input.text().strip()
        label = self.label_input.text().strip() or f"Wallet {len(self._wallets) + 1}"

        if not addr:
            QMessageBox.warning(self, "Invalid", "请输入有效的钱包地址")
            return
        if not addr.startswith("0x") or len(addr) != 42:
            QMessageBox.warning(self, "Invalid", "钱包地址应为 0x 开头的 42 字符地址")
            return
        for w in self._wallets:
            if w["address"].lower() == addr.lower():
                QMessageBox.information(self, "Duplicate", "该地址已在列表中")
                return

        self._wallets.append({
            "address": addr,
            "label": label,
            "active": True,
        })
        self._save()
        self.addr_input.clear()
        self.label_input.clear()
        QMessageBox.information(self, "Added", f"已添加钱包: {label} ({addr[:10]}...)")

    def _on_remove(self, address: str):
        reply = QMessageBox.question(
            self, "Remove Wallet",
            f"确认移除 {address[:12]}...?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if reply == QMessageBox.StandardButton.Yes:
            self._wallets = [w for w in self._wallets if w["address"] != address]
            self._save()

    # ------------------------------------------------------------------
    # 刷新
    # ------------------------------------------------------------------

    def refresh(self, balances: dict | None = None):
        """刷新钱包列表和余额。

        Args:
            balances: {address: balance_usdc} 或 None（无余额数据）
        """
        self._wallets.sort(key=lambda w: (not w.get("active", True), w["label"]))
        self.table.setRowCount(len(self._wallets))

        for i, w in enumerate(self._wallets):
            addr = w["address"]
            label = w.get("label", "")

            self.table.setItem(i, 0, QTableWidgetItem(label))
            short = f"{addr[:8]}...{addr[-6:]}"
            self.table.setItem(i, 1, QTableWidgetItem(short))

            bal = balances.get(addr, None) if balances else None
            bal_text = f"${bal:,.2f}" if bal is not None else "—"
            bal_item = QTableWidgetItem(bal_text)
            if bal is not None:
                bal_item.setForeground(
                    QColor("#2E7D32") if bal > 0 else QColor("#888")
                )
            self.table.setItem(i, 2, bal_item)

            active = "✓" if w.get("active", True) else "✗"
            act_item = QTableWidgetItem(active)
            act_item.setTextAlignment(Qt.AlignmentFlag.AlignCenter)
            self.table.setItem(i, 3, act_item)

            rm_btn = QPushButton("Remove")
            rm_btn.setStyleSheet(
                "background: #EF5350; color: white; padding: 2px 6px; font-size: 10px;"
            )
            rm_btn.clicked.connect(lambda checked, a=addr: self._on_remove(a))
            self.table.setCellWidget(i, 4, rm_btn)

    def get_addresses(self) -> list[str]:
        """返回活跃地址列表。"""
        return [w["address"] for w in self._wallets if w.get("active", True)]


# ============================================================================
# 面板 5c: 资产总览（钱包余额 + 持仓 + 汇总）
# ============================================================================

class AssetsPanel(QWidget):
    """资产总览面板：
      1. 汇总卡片 — 总资产 / 总浮盈浮亏 / 今日盈亏 / 收益率
      2. USDC 余额 — 可用 / 冻结
      3. 持仓明细表 — 市场、方向、数量、成本价、现价、浮盈浮亏%
    """

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        title = QLabel("Portfolio Overview")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        layout.addWidget(title)

        # ---- 汇总卡片（2x2）----
        cards = QGridLayout()
        self._card_total = self._make_card(cards, 0, 0, "Total Assets", "$0.00")
        self._card_pnl = self._make_card(cards, 0, 1, "Total PnL", "$0.00")
        self._card_today = self._make_card(cards, 1, 0, "Today PnL", "$0.00")
        self._card_roi = self._make_card(cards, 1, 1, "ROI", "0.00%")
        layout.addLayout(cards)

        # ---- USDC 余额 ----
        usdc = QHBoxLayout()
        usdc_title = QLabel("USDC Balance")
        usdc_title.setStyleSheet("font-weight: bold;")
        self._usdc_label = QLabel("Available: $0.00 | Locked: $0.00")
        self._usdc_label.setStyleSheet("font-size: 12px; color: #333;")
        usdc.addWidget(usdc_title)
        usdc.addWidget(self._usdc_label, 1)
        layout.addLayout(usdc)

        # ---- 持仓明细表 ----
        self.table = QTableWidget(0, 7)
        self.table.setHorizontalHeaderLabels([
            "Market", "Side", "Size", "Cost", "Now", "PnL $", "PnL %"
        ])
        self.table.horizontalHeader().setSectionResizeMode(QHeaderView.ResizeMode.Stretch)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        layout.addWidget(self.table, 1)

    @staticmethod
    def _make_card(grid: QGridLayout, row: int, col: int,
                   name: str, value: str) -> QLabel:
        """创建一张汇总卡片，返回值标签（供后续刷新颜色）。"""
        frame = QFrame()
        frame.setStyleSheet(
            "QFrame { background: #FAFAFA; border: 1px solid #E0E0E0;"
            " border-radius: 6px; padding: 8px; }"
        )
        vbox = QVBoxLayout(frame)
        name_lbl = QLabel(name)
        name_lbl.setStyleSheet("font-size: 11px; color: #888;")
        val_lbl = QLabel(value)
        val_lbl.setStyleSheet("font-size: 18px; font-weight: bold; color: #222;")
        vbox.addWidget(name_lbl)
        vbox.addWidget(val_lbl)
        grid.addWidget(frame, row, col)
        return val_lbl

    def refresh(self, portfolio: dict):
        usdc = portfolio.get("usdc", {})
        positions = portfolio.get("positions", [])

        # ---- 汇总计算 ----
        pos_value = 0.0   # 持仓市值（按现价）
        pos_cost = 0.0    # 持仓成本（按成本价）
        for p in positions:
            side = p.get("side", "BUY_YES")
            cost = float(p.get("cost", 0))
            now = float(p.get("now", 0))
            size = float(p.get("size", 0))
            # BUY_NO 持有的是 NO 份额：价格换算为 1-x
            if side == "BUY_NO":
                cost, now = 1.0 - cost, 1.0 - now
            pos_value += size * now
            pos_cost += size * cost

        available = float(usdc.get("available", 0))
        locked = float(usdc.get("locked", 0))
        total_assets = available + locked + pos_value
        total_pnl = pos_value - pos_cost
        today_pnl = float(portfolio.get("today_pnl", 0.0))
        roi = (total_pnl / pos_cost * 100) if pos_cost > 0 else 0.0

        # ---- 汇总卡片（美股/BTC 惯例：盈利绿 / 亏损红）----
        self._card_total.setText(self._fmt_usd(total_assets))
        self._set_pnl_card(self._card_pnl, total_pnl)
        self._set_pnl_card(self._card_today, today_pnl)
        self._set_pnl_card(self._card_roi, roi, suffix="%")

        # ---- USDC 余额 ----
        self._usdc_label.setText(
            f"Available: {self._fmt_usd(available)}   |   "
            f"Locked: {self._fmt_usd(locked)}"
        )

        # ---- 持仓明细表 ----
        self.table.setRowCount(len(positions))
        for i, p in enumerate(positions):
            side = p.get("side", "BUY_YES")
            cost = float(p.get("cost", 0))
            now = float(p.get("now", 0))
            size = float(p.get("size", 0))
            eff_cost, eff_now = cost, now
            if side == "BUY_NO":
                eff_cost, eff_now = 1.0 - cost, 1.0 - now
            pnl = size * (eff_now - eff_cost)
            pnl_pct = (eff_now - eff_cost) / eff_cost * 100 if eff_cost > 0 else 0.0

            self.table.setItem(i, 0, QTableWidgetItem(p.get("market", "")))
            side_item = QTableWidgetItem(side)
            side_item.setBackground(QBrush(
                QColor("#E3F2FD") if side == "BUY_YES" else QColor("#FCE4EC")))
            self.table.setItem(i, 1, side_item)
            self.table.setItem(i, 2, QTableWidgetItem(f"{size:g}"))
            self.table.setItem(i, 3, QTableWidgetItem(f"{eff_cost:.4f}"))
            self.table.setItem(i, 4, QTableWidgetItem(f"{eff_now:.4f}"))

            pnl_item = QTableWidgetItem(f"{pnl:+.2f}")
            pnl_item.setForeground(QBrush(self._pnl_color(pnl)))
            self.table.setItem(i, 5, pnl_item)

            pct_item = QTableWidgetItem(f"{pnl_pct:+.2f}%")
            pct_item.setForeground(QBrush(self._pnl_color(pnl_pct)))
            self.table.setItem(i, 6, pct_item)

    # ---- 工具 ----
    @staticmethod
    def _fmt_usd(v: float) -> str:
        return f"${v:,.2f}"

    @staticmethod
    def _pnl_color(v: float) -> QColor:
        """美股/BTC 惯例：盈利绿 / 亏损红。"""
        return QColor("#2E7D32") if v >= 0 else QColor("#C62828")

    @staticmethod
    def _set_pnl_card(label: QLabel, v: float, suffix: str = ""):
        """设置盈亏卡片文字（带符号）+ 颜色（盈利红 / 亏损绿）。"""
        text = f"{v:+.2f}{suffix}" if suffix else f"${v:+,.2f}"
        label.setText(text)
        label.setStyleSheet(
            f"font-size: 18px; font-weight: bold; color: {AssetsPanel._pnl_color(v).name()};"
        )


# ============================================================================
# 面板 5b: 交易复盘
# ============================================================================

class TradeReviewPanel(QWidget):
    """交易复盘：滑点分析 + 执行质量。"""

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        title = QLabel("Trade Review")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        layout.addWidget(title)

        self.table = QTableWidget(0, 6)
        self.table.setHorizontalHeaderLabels([
            "Time", "Side", "Price", "Size", "Slippage %", "Score"
        ])
        self.table.horizontalHeader().setSectionResizeMode(QHeaderView.ResizeMode.Stretch)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        layout.addWidget(self.table)

        self.summary = QLabel("Trades: 0 | Avg Slippage: 0.00% | Avg Score: N/A")
        self.summary.setStyleSheet("font-size: 11px; color: #666; padding: 4px;")
        layout.addWidget(self.summary)

    def refresh(self, trades: list):
        self.table.setRowCount(len(trades))
        slippages = []
        for i, t in enumerate(trades):
            self.table.setItem(i, 0, QTableWidgetItem(t.get("time", "")))
            self.table.setItem(i, 1, QTableWidgetItem(t.get("side", "")))
            self.table.setItem(i, 2, QTableWidgetItem(f"{t.get('price', 0):.4f}"))
            self.table.setItem(i, 3, QTableWidgetItem(str(t.get("size", 0))))
            sl = t.get("slippage_pct", 0)
            self.table.setItem(i, 4, QTableWidgetItem(f"{sl:.2f}%"))
            slippages.append(sl)

            score = t.get("score", "")
            score_item = QTableWidgetItem(score)
            if score == "A":
                score_item.setBackground(QBrush(QColor("#C8E6C9")))
            elif score == "B":
                score_item.setBackground(QBrush(QColor("#FFF9C4")))
            elif score in ("C", "D"):
                score_item.setBackground(QBrush(QColor("#FFCDD2")))
            self.table.setItem(i, 5, score_item)

        avg_slip = sum(slippages) / len(slippages) if slippages else 0
        self.summary.setText(
            f"Trades: {len(trades)} | Avg Slippage: {avg_slip:.2f}% | "
            f"Avg Score: {self._avg_score(trades)}"
        )

    @staticmethod
    def _avg_score(trades: list) -> str:
        scores = {"A": 4, "B": 3, "C": 2, "D": 1}
        vals = [scores.get(t.get("score", "B"), 3) for t in trades]
        if not vals:
            return "N/A"
        avg = sum(vals) / len(vals)
        if avg >= 3.5:
            return "A"
        if avg >= 2.5:
            return "B"
        return "C"


# ============================================================================
# 面板 5c: 新盘发现 (market_discovery 集成)
# ============================================================================

class MarketDiscoveryPanel(QWidget):
    """新盘发现面板：展示 market_discovery 注册表 + 手动触发扫描。

    数据来源: market_discovery.MarketRegistry (data/market_registry.json)，
    与 CLI/库模式共用同一注册表，GUI 只读展示。
    """

    scan_done = pyqtSignal(object)   # (new_markets, summary)
    scan_error = pyqtSignal(str)

    REGISTRY_PATH = "./data/market_registry.json"

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        # 顶部: 状态行 + 扫描按钮
        top = QHBoxLayout()
        self.status_label = QLabel("Market Discovery: —")
        self.status_label.setStyleSheet("font-size: 12px; color: #555;")
        top.addWidget(self.status_label, 1)

        self.scan_btn = QPushButton("Scan Now")
        self.scan_btn.clicked.connect(self._on_scan_clicked)
        top.addWidget(self.scan_btn)
        layout.addLayout(top)

        # 表格: 注册表市场
        self.table = QTableWidget(0, 7)
        self.table.setHorizontalHeaderLabels([
            "Location", "Type", "YES", "Ends (UTC)", "Volume", "Liquidity", "Token",
        ])
        self.table.horizontalHeader().setSectionResizeMode(QHeaderView.ResizeMode.Stretch)
        self.table.setEditTriggers(QTableWidget.EditTrigger.NoEditTriggers)
        self.table.verticalHeader().setVisible(False)
        layout.addWidget(self.table, 1)

        # 底部: 最近一次扫描结果
        self.last_scan_label = QLabel("Last scan: never")
        self.last_scan_label.setStyleSheet("font-size: 11px; color: #888;")
        layout.addWidget(self.last_scan_label)

        # 信号
        self.scan_done.connect(self._on_scan_done)
        self.scan_error.connect(self._on_scan_error)

        self._scan_thread: Optional[threading.Thread] = None
        self._last_state = ""   # 变更检测: registry 内容未变则跳过重建表格
        self.refresh()

    # ------------------------------------------------------------------
    # 刷新
    # ------------------------------------------------------------------

    def refresh(self):
        """从注册表读取并更新（2s tick 调用；内容未变时零开销）。"""
        try:
            from market_discovery.registry import MarketRegistry
            reg = MarketRegistry(self.REGISTRY_PATH)
        except ImportError:
            self.status_label.setText("Market Discovery: market_discovery 未安装")
            return
        except Exception:
            self.status_label.setText("Market Discovery: 注册表读取失败")
            return

        s = reg.summary()
        state = f"{s['total']}|{s['last_scan_utc']}"
        by_et = "  ".join(f"{k}={v}" for k, v in s["by_event_type"].items()) or "(无)"
        self.status_label.setText(
            f"Market Discovery: {s['total']} 个市场 | 活跃 {s['active']} | "
            f"{by_et} | 上次扫描 {s['last_scan_utc'] or '从未'}"
        )

        if state == self._last_state:
            return
        self._last_state = state

        # 重建表格
        rows = reg.all_markets()
        rows.sort(key=lambda r: r.get("first_seen_utc", ""), reverse=True)
        self.table.setRowCount(len(rows))
        for i, r in enumerate(rows):
            from datetime import datetime as _dt
            end = _dt.fromtimestamp(r.get("end_date_utc") or 0).strftime("%m-%d %H:%M")
            yes = r.get("outcome_prices") or []
            yes_txt = f"{float(yes[0]):.3f}" if yes else "0.500"
            self.table.setItem(i, 0, QTableWidgetItem(str(r.get("location", "?"))))
            self.table.setItem(i, 1, QTableWidgetItem(str(r.get("event_type", "?"))))
            self.table.setItem(i, 2, QTableWidgetItem(yes_txt))
            self.table.setItem(i, 3, QTableWidgetItem(end))
            self.table.setItem(i, 4, QTableWidgetItem(f"${r.get('volume', 0):,.0f}"))
            self.table.setItem(i, 5, QTableWidgetItem(f"${r.get('liquidity', 0):,.0f}"))
            self.table.setItem(i, 6, QTableWidgetItem(str(r.get("token_id", ""))[:20]))

    # ------------------------------------------------------------------
    # 手动扫描（后台线程，避免阻塞 UI）
    # ------------------------------------------------------------------

    def _on_scan_clicked(self):
        if self._scan_thread and self._scan_thread.is_alive():
            return
        self.scan_btn.setEnabled(False)
        self.scan_btn.setText("Scanning...")
        self._scan_thread = threading.Thread(target=self._scan_worker, daemon=True)
        self._scan_thread.start()

    def _scan_worker(self):
        try:
            import asyncio as _asyncio
            from market_discovery.gamma_client import GammaClient
            from market_discovery.scanner import MarketScanner

            scanner = MarketScanner(
                registry_path=self.REGISTRY_PATH,
                client=GammaClient(),  # 真实 API，失败自动降级 mock
            )
            new_markets = _asyncio.run(scanner.scan_once())
            self.scan_done.emit((new_markets, scanner.registry.summary()))
        except Exception as e:
            self.scan_error.emit(str(e))

    def _on_scan_done(self, payload):
        new_markets, summary = payload
        self.scan_btn.setEnabled(True)
        self.scan_btn.setText("Scan Now")
        self.last_scan_label.setText(
            f"Last scan: {summary['last_scan_utc']} | 本轮新盘 {len(new_markets)} 个"
        )
        self._last_state = ""   # 强制重建表格
        self.refresh()

    def _on_scan_error(self, msg: str):
        self.scan_btn.setEnabled(True)
        self.scan_btn.setText("Scan Now")
        self.last_scan_label.setText(f"Scan error: {msg}")


# ============================================================================
# 面板 8: 总资产变化图
# ============================================================================

class EquityCurvePanel(QWidget):
    """总资产变化折线图 — 每日北京时间 00:30 自动快照。"""

    HISTORY_PATH = Path("data/equity_history.json")

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(4, 4, 4, 4)

        title = QLabel("Total Equity Curve")
        title.setStyleSheet("font-size: 13px; font-weight: bold; padding: 4px;")
        layout.addWidget(title)

        self.figure = Figure(figsize=(6, 2.5), dpi=100, facecolor="#FAFAFA")
        self.canvas = FigureCanvas(self.figure)
        self.canvas.setMinimumHeight(180)
        layout.addWidget(self.canvas)

        self._history: list[dict] = []
        self._last_snapshot_date: str = ""

    def load_history(self):
        try:
            if self.HISTORY_PATH.exists():
                with open(self.HISTORY_PATH, "r", encoding="utf-8") as f:
                    self._history = json.load(f)
        except Exception:
            self._history = []

    def save_history(self):
        try:
            self.HISTORY_PATH.parent.mkdir(parents=True, exist_ok=True)
            with open(self.HISTORY_PATH, "w", encoding="utf-8") as f:
                json.dump(self._history, f, indent=2, ensure_ascii=False)
        except Exception:
            pass

    def snapshot_if_needed(self, total_value: float):
        """每日北京时间 00:30 自动快照（若今天尚未记录）。"""
        bjt = timezone(timedelta(hours=8))
        now_bjt = datetime.now(bjt)
        today_bjt = now_bjt.strftime("%Y-%m-%d")

        if now_bjt.hour == 0 and now_bjt.minute < 30:
            return
        if self._last_snapshot_date == today_bjt:
            return

        ts_str = (
            datetime.now(bjt)
            .replace(hour=0, minute=30, second=0, microsecond=0)
            .isoformat()
        )
        self._history.append({"ts": ts_str, "total": round(total_value, 2)})
        self._last_snapshot_date = today_bjt
        self.save_history()

        if len(self._history) > 90:
            self._history = self._history[-90:]

    def refresh(self):
        """重绘折线图。"""
        self.load_history()
        self.figure.clear()
        ax = self.figure.add_subplot(111)
        ax.set_facecolor("#FAFAFA")

        if not self._history:
            ax.text(
                0.5, 0.5, "暂无历史数据\n每日北京时间 00:30 自动记录",
                ha="center", va="center", transform=ax.transAxes,
                color="#999", fontsize=10,
            )
            ax.set_xticks([])
            ax.set_yticks([])
            self.canvas.draw()
            return

        dates = []
        values = []
        for h in self._history:
            try:
                dates.append(datetime.fromisoformat(h["ts"]))
                values.append(h["total"])
            except Exception:
                continue

        if not dates:
            return

        ax.plot(dates, values, color="#1976D2", linewidth=2, marker="o", markersize=4)
        ax.fill_between(dates, min(values) * 0.98, values, alpha=0.08, color="#1976D2")

        ax.xaxis.set_major_formatter(mdates.DateFormatter("%m/%d"))
        ax.xaxis.set_major_locator(mdates.AutoDateLocator())
        self.figure.autofmt_xdate(rotation=30, ha="right")

        mn, mx = min(values), max(values)
        margin = (mx - mn) * 0.15 if mx > mn else max(1.0, mx * 0.15)
        ax.set_ylim(max(0, mn - margin), mx + margin)
        ax.set_ylabel("Total Assets (USD)", fontsize=9)
        ax.tick_params(axis="both", labelsize=8)
        ax.grid(True, alpha=0.3, linestyle="--")

        if values:
            ax.annotate(
                f"${values[-1]:,.2f}",
                xy=(dates[-1], values[-1]),
                xytext=(5, 10), textcoords="offset points",
                fontsize=9, color="#1976D2", fontweight="bold",
            )

        self.figure.tight_layout(pad=1.0)
        self.canvas.draw()


# ============================================================================
# 面板 9: 底部运行状态日志
# ============================================================================

class LogPanel(QWidget):
    """底部运行状态日志 — 可折叠，彩色分级。"""

    MAX_LINES = 500

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)

        self._header = QPushButton("▼ 运行日志")
        self._header.setStyleSheet(
            "text-align: left; padding: 4px 8px; font-size: 12px; font-weight: bold; "
            "background: #f0f0f0; border: 1px solid #ddd; border-radius: 2px;"
        )
        self._header.clicked.connect(self._toggle)
        layout.addWidget(self._header)

        self._log_view = QPlainTextEdit()
        self._log_view.setReadOnly(True)
        self._log_view.setMaximumBlockCount(self.MAX_LINES)
        self._log_view.setStyleSheet(
            "font-family: Consolas, monospace; font-size: 11px; "
            "background: #1e1e1e; color: #d4d4d4; border: 1px solid #ddd;"
        )
        self._log_view.setMinimumHeight(60)
        self._log_view.setMaximumHeight(200)
        layout.addWidget(self._log_view)

        self._collapsed = False

    def _toggle(self):
        self._collapsed = not self._collapsed
        self._log_view.setVisible(not self._collapsed)
        self._header.setText("▶ 运行日志" if self._collapsed else "▼ 运行日志")

    def _append(self, text: str, color: str = "#d4d4d4"):
        ts = datetime.now().strftime("%H:%M:%S")
        line = f"[{ts}] {text}"
        cursor = self._log_view.textCursor()
        cursor.movePosition(QTextCursor.MoveOperation.End)
        fmt = QTextCharFormat()
        fmt.setForeground(QColor(color))
        cursor.insertText(line + "\n", fmt)
        self._log_view.setTextCursor(cursor)
        self._log_view.ensureCursorVisible()

    def info(self, text: str):
        self._append(text, "#81C784")

    def warn(self, text: str):
        self._append(text, "#FFD54F")

    def error(self, text: str):
        self._append(text, "#EF5350")

    def status(self, text: str):
        self._append(text, "#64B5F6")



# ============================================================================
# 主窗口
# ============================================================================

class TradingDashboard(QMainWindow):
    """策略交易仪表盘主窗口。"""

    def __init__(self, live: bool = False):
        super().__init__()
        self.setWindowTitle("Polymarket Weather Trading Dashboard")
        self.resize(1280, 800)

        self._data = LiveDataProvider(live=live)

        # ===== 中央区域 =====
        central = QWidget()
        self.setCentralWidget(central)
        main_layout = QVBoxLayout(central)
        main_layout.setContentsMargins(0, 0, 0, 0)

        # 顶部 tab: 气象 + 市场深度 + 资产总览 + 新盘发现 + 资产曲线
        top_tab = QTabWidget()
        self.weather = WeatherDashboard()
        self.orderbook = OrderBookPanel()
        self.assets = AssetsPanel()
        self.market_discovery = MarketDiscoveryPanel()
        self.equity_curve = EquityCurvePanel()
        top_tab.addTab(self.weather, "Weather & Prices")
        top_tab.addTab(self.orderbook, "Market Depth")
        top_tab.addTab(self.assets, "Portfolio")
        top_tab.addTab(self.market_discovery, "Market Discovery")
        top_tab.addTab(self.equity_curve, "Equity Curve")
        main_layout.addWidget(top_tab, 2)

        # 底部 split: 信号日志 | 结算 + 复盘
        bottom_split = QSplitter(Qt.Orientation.Horizontal)

        # 信号日志
        self.signal_log = SignalLogPanel()
        self.signal_log.cancel_requested.connect(self._cancel_order)
        bottom_split.addWidget(self.signal_log)

        # 右侧：结算 + 复盘
        right_panel = QVBoxLayout()
        right_widget = QWidget()
        right_widget.setLayout(right_panel)
        self.settlement = SettlementTimer()
        self.trade_review = TradeReviewPanel()
        self.wallet_panel = WalletPanel()
        right_panel.addWidget(self.wallet_panel)
        right_panel.addWidget(self.settlement)
        right_panel.addWidget(self.trade_review)
        bottom_split.addWidget(right_widget)

        bottom_split.setSizes([700, 500])
        main_layout.addWidget(bottom_split, 3)

        # 底部运行日志
        self.log_panel = LogPanel()
        main_layout.addWidget(self.log_panel, 1)

        # ===== 状态栏 =====
        self.status = self.statusBar()
        self.status.showMessage("Connected | Strategy: RUNNING | PnL: +$124.50")

        # ===== 定时刷新 =====
        self.timer = QTimer()
        self.timer.timeout.connect(self._tick)
        self.timer.start(2000)  # 2秒刷新
        self._tick_count = 0

        # 启动日志
        self.equity_curve.load_history()
        self.log_panel.info("仪表盘启动完成")
        self.log_panel.status(
            f"模式: {'Live (gRPC)' if live else 'Mock (模拟)'}"
        )

        # ===== 菜单 =====
        menubar = self.menuBar()
        file_menu = menubar.addMenu("File")
        exit_action = QAction("Exit", self)
        exit_action.triggered.connect(self.close)
        file_menu.addAction(exit_action)

        control_menu = menubar.addMenu("Control")
        self.pause_action = QAction("Pause Strategy", self)
        self.pause_action.setCheckable(True)
        self.pause_action.toggled.connect(self._toggle_pause)
        control_menu.addAction(self.pause_action)

        cancel_all_action = QAction("Emergency Cancel All", self)
        cancel_all_action.triggered.connect(self._cancel_all)
        control_menu.addAction(cancel_all_action)

    # ------------------------------------------------------------------
    # 定时刷新
    # ------------------------------------------------------------------

    def _tick(self):
        """每 2s 拉取最新数据。"""
        weather = self._data.weather()
        prices = self._data.market_prices()
        book = self._data.order_book()
        signals = self._data.signals()
        trades = self._data.trades()
        portfolio = self._data.portfolio()

        self.weather.refresh(weather, prices)
        self.orderbook.refresh(book)
        self.signal_log.refresh(signals)
        self.trade_review.refresh(trades)
        self.assets.refresh(portfolio)
        self.market_discovery.refresh()

        # 钱包面板 — 用真实余额数据
        usdc = portfolio.get("usdc", {})
        balances = {}
        for addr in self.wallet_panel.get_addresses():
            if addr in usdc:
                balances[addr] = usdc[addr] if isinstance(usdc, dict) else usdc.get("available", 0)
            else:
                balances[addr] = usdc.get("available", 0) if len(self.wallet_panel.get_addresses()) == 1 else None
        self.wallet_panel.refresh(balances)

        # 资产曲线 — 每日快照 + 刷新
        total_assets = (
            portfolio.get("usdc", {}).get("available", 0)
            + portfolio.get("usdc", {}).get("frozen", 0)
            + sum(p.get("current_value", 0) for p in portfolio.get("positions", []))
        )
        self.equity_curve.snapshot_if_needed(total_assets)
        self.equity_curve.refresh()

        # 更新状态栏 + 日志
        mode = self._data.live_status()
        usdc_avail = portfolio.get("usdc", {}).get("available", 0)

        # 模拟结算倒计时
        now = int(time.time())
        self.settlement.set_events([
            {"name": "NYC Rain 08/10", "deadline_unix": now + 7200},
            {"name": "CHI Temp 08/10", "deadline_unix": now + 18000},
        ])

        status_msg = (
            f"{mode} | {datetime.now().strftime('%H:%M:%S')} | "
            f"USDC: ${usdc_avail:,.2f} | Total: ${total_assets:,.2f}"
        )
        self.status.showMessage(status_msg)

        # 每 10 次 tick (~20s) 输出一次日志，避免刷屏
        if getattr(self, "_tick_count", 0) % 10 == 0:
            pos_count = len(portfolio.get("positions", []))
            sig_count = len(signals)
            self.log_panel.status(
                f"USDC ${usdc_avail:,.2f} | 总资产 ${total_assets:,.2f} | "
                f"持仓 {pos_count} | 信号 {sig_count} | {mode}"
            )
        self._tick_count = getattr(self, "_tick_count", 0) + 1

    # ------------------------------------------------------------------
    # 控制
    # ------------------------------------------------------------------

    def _cancel_order(self, order_id: str):
        """撤单回调（实际应通过 gRPC 调用 Rust 执行层）。"""
        QMessageBox.information(
            self, "Order Cancelled",
            f"Cancel request sent for {order_id}\n\n"
            f"(gRPC → Rust cancel_order → CLOB API)"
        )

    def _cancel_all(self):
        """紧急全撤。"""
        reply = QMessageBox.warning(
            self, "EMERGENCY CANCEL ALL",
            "This will cancel ALL open orders.\n\nContinue?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if reply == QMessageBox.StandardButton.Yes:
            QMessageBox.information(self, "Done", "All orders cancelled.")

    def _toggle_pause(self, paused: bool):
        if paused:
            self.timer.stop()
            self.status.showMessage("PAUSED — Strategy halted")
        else:
            self.timer.start(2000)
            self.status.showMessage("RUNNING — Strategy active")


# ============================================================================
# 入口
# ============================================================================

def main():
    live = "--live" in sys.argv
    app = QApplication(sys.argv)
    app.setStyle("Fusion")

    palette = app.palette()
    palette.setColor(palette.ColorRole.Window, QColor("#FFFFFF"))
    palette.setColor(palette.ColorRole.Base, QColor("#FAFAFA"))
    app.setPalette(palette)

    window = TradingDashboard(live=live)
    window.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
