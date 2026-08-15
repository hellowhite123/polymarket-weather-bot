"""回测框架数据模型。

定义回测过程中使用的所有数据结构:
  - BacktestConfig:      回测配置
  - HistoricalEvent:     历史天气事件 (含预报、市场快照、真实结果)
  - TradeRecord:         交易记录 (开仓/平仓)
  - Position:            持仓快照
  - EquityPoint:         权益曲线数据点
  - BacktestResult:      回测最终结果
"""

from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from enum import Enum
from typing import Dict, List, Optional


# ============================================================
# 枚举
# ============================================================


class EventOutcome(str, Enum):
    """天气事件真实结果。"""

    YES = "yes"     # 事件发生 (YES token 赢)
    NO = "no"       # 事件未发生 (NO token 赢)
    PENDING = "pending"  # 尚未结算


class TradeAction(str, Enum):
    """交易动作。"""

    OPEN = "open"       # 开仓
    SETTLE = "settle"   # 结算 (事件结果已知)


# ============================================================
# 配置
# ============================================================


@dataclass
class BacktestConfig:
    """回测配置参数。"""

    # 初始资金 (USDC)
    initial_capital: float = 10000.0

    # 回测时间步长 (小时)
    step_hours: float = 6.0

    # 滑点 (bps, 基点)
    slippage_bps: float = 50.0  # 0.5%

    # 手续费率 (bps)
    fee_bps: float = 20.0  # 0.2%

    # 最大持仓数量 (同时持有的不同 token 数)
    max_open_positions: int = 20

    # 单笔最大仓位 (USDC)
    max_position_size: float = 1000.0

    # 是否使用聚合器 (True: 多策略聚合; False: 单策略直出)
    use_aggregator: bool = True

    # 策略名称列表 (选择哪些策略参与回测)
    strategies: List[str] = field(
        default_factory=lambda: ["forecast_deviation"]
    )

    # Mock 市场参数
    mock_market_bias: float = 0.03
    mock_market_noise: float = 0.05

    # 随机种子
    seed: int = 42

    # 输出路径
    output_path: str = "./data/backtest"

    def to_dict(self) -> dict:
        return asdict(self)


# ============================================================
# 历史事件
# ============================================================


@dataclass
class HistoricalEvent:
    """历史天气事件 -- 回测的基本单元。

    一个事件 = 一个天气问题 (如 "纽约 8月1日 是否下雨"),
    包含:
      - 事件元信息 (token_id, location, event_type, deadline)
      - 时间序列预报数据 (多个时间点的预报)
      - 时间序列市场快照 (多个时间点的市场价格)
      - 真实结果 (YES/NO)
    """

    token_id: str
    location: str
    event_type: str              # rain / temperature / snow / wind / humidity
    deadline: datetime           # 事件结算时间
    outcome: EventOutcome = EventOutcome.PENDING

    # 预报数据 (JSON 文件路径列表, 按时间排序)
    forecast_files: List[str] = field(default_factory=list)

    # 市场快照时间序列 [(timestamp, yes_price, no_price, volume, spread)]
    market_history: List[dict] = field(default_factory=list)

    # as-of 预报概率时间序列 [(timestamp, prob)]
    # 每个点代表「在该时刻可获得的模型预报概率」，用于回测信号生成。
    # 禁止用 actual_data 直接作为模型概率（那会造成 oracle / future leakage）。
    forecast_history: List[dict] = field(default_factory=list)

    # 真实气象数据 (仅用于最终结算 / PnL 计算，不得用于信号生成)
    actual_data: dict = field(default_factory=dict)

    def get_forecast_probability_at(self, at_time: datetime) -> Optional[float]:
        """返回 <= at_time 的最后一个 as-of 预报概率（禁止未来数据）。

        Args:
            at_time: 当前回测时间点

        Returns:
            预报概率 (0~1)，若无可用预报返回 None
        """
        if at_time.tzinfo is None:
            at_time = at_time.replace(tzinfo=timezone.utc)

        best: Optional[tuple] = None  # (snap_time, prob)
        for f in self.forecast_history:
            ts = datetime.fromisoformat(f["timestamp"])
            if ts.tzinfo is None:
                ts = ts.replace(tzinfo=timezone.utc)
            # 跳过未来预报
            if ts > at_time:
                continue
            prob = float(f.get("prob", 0.5))
            if best is None or ts > best[0]:
                best = (ts, prob)

        return best[1] if best is not None else None

    def to_dict(self) -> dict:
        d = asdict(self)
        d["deadline"] = self.deadline.isoformat()
        d["outcome"] = self.outcome.value
        return d


# ============================================================
# 交易记录
# ============================================================


@dataclass
class TradeRecord:
    """单笔交易记录。"""

    timestamp: datetime
    action: TradeAction
    token_id: str
    direction: str              # BUY_YES / BUY_NO
    quantity: float             # 交易数量 (token 数)
    price: float                # 成交价格
    cost: float                 # 交易成本 (含手续费+滑点)
    fee: float = 0.0
    slippage: float = 0.0
    pnl: float = 0.0            # 结算时的盈亏 (仅 SETTLE 有值)
    strategy: str = ""          # 来源策略
    confidence: float = 0.0
    metadata: dict = field(default_factory=dict)

    def to_dict(self) -> dict:
        d = asdict(self)
        d["timestamp"] = self.timestamp.isoformat()
        d["action"] = self.action.value
        return d


# ============================================================
# 持仓
# ============================================================


@dataclass
class Position:
    """单个 token 的持仓。"""

    token_id: str
    direction: str               # BUY_YES / BUY_NO
    quantity: float              # 持有数量
    entry_price: float           # 开仓均价
    cost: float                  # 总成本 (含手续费)
    open_time: datetime
    strategy: str = ""
    confidence: float = 0.0
    event_type: str = ""
    location: str = ""
    deadline: Optional[datetime] = None
    metadata: dict = field(default_factory=dict)

    @property
    def notional(self) -> float:
        """名义价值 = 数量 * 开仓价。"""
        return self.quantity * self.entry_price

    def to_dict(self) -> dict:
        d = asdict(self)
        d["open_time"] = self.open_time.isoformat()
        d["deadline"] = self.deadline.isoformat() if self.deadline else None
        return d


# ============================================================
# 权益曲线
# ============================================================


@dataclass
class EquityPoint:
    """权益曲线数据点。"""

    timestamp: datetime
    cash: float                  # 可用现金
    positions_value: float       # 持仓市值 (按当前市场价)
    equity: float                # 总权益 = cash + positions_value
    open_positions: int          # 持仓数量
    realized_pnl: float          # 累计已实现盈亏
    unrealized_pnl: float        # 未实现盈亏

    def to_dict(self) -> dict:
        d = asdict(self)
        d["timestamp"] = self.timestamp.isoformat()
        return d


# ============================================================
# 回测结果
# ============================================================


@dataclass
class BacktestResult:
    """回测最终结果。"""

    # 配置
    config: dict = field(default_factory=dict)

    # 时间范围
    start_time: Optional[str] = None
    end_time: Optional[str] = None
    total_steps: int = 0

    # 交易统计
    total_trades: int = 0
    open_trades: int = 0
    settle_trades: int = 0
    winning_trades: int = 0
    losing_trades: int = 0

    # 盈亏
    initial_capital: float = 0.0
    final_equity: float = 0.0
    total_pnl: float = 0.0
    total_fees: float = 0.0
    total_slippage: float = 0.0
    max_drawdown: float = 0.0

    # 绩效指标
    roi: float = 0.0                    # 收益率
    annualized_roi: float = 0.0         # 年化收益率
    sharpe_ratio: float = 0.0           # 夏普比率
    sortino_ratio: float = 0.0          # 索提诺比率
    max_drawdown_pct: float = 0.0       # 最大回撤百分比
    win_rate: float = 0.0               # 胜率
    profit_factor: float = 0.0          # 盈亏比
    avg_win: float = 0.0                # 平均盈利
    avg_loss: float = 0.0               # 平均亏损
    avg_hold_hours: float = 0.0         # 平均持仓时间

    # 时间序列
    equity_curve: List[dict] = field(default_factory=list)
    trades: List[dict] = field(default_factory=list)
    open_positions: List[dict] = field(default_factory=list)

    # 按策略分组统计
    strategy_stats: Dict[str, dict] = field(default_factory=dict)

    def to_dict(self) -> dict:
        return asdict(self)
