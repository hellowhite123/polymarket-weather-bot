# ============================================================================
# Polymarket Weather Trading System — Docker 部署
#
# 构建:
#   docker build -t ai-weather:latest .
#
# 运行:
#   docker compose up -d                    # 后台常驻 (market_discovery + strategy)
#   docker compose run paper-trading        # 纸面交易测试
#   docker compose run dashboard            # GUI (需要 Xvfb + VNC)
#   docker compose exec app python -m market_discovery --status
# ============================================================================

FROM python:3.13-slim-bookworm

LABEL maintainer="bosen@ai-weather"
LABEL description="Polymarket 天气预测市场量化交易系统"

# ── apt 镜像源 → 清华源 ──
RUN sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
    /etc/apt/sources.list.d/debian.sources 2>/dev/null || \
  sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
    /etc/apt/sources.list

# ── 系统依赖 ──
# libgl1 + libxcb-* → PyQt6 离线渲染
# xvfb              → 无头 GUI 虚拟显示
# gcc/python3-dev   → grpcio 编译
RUN apt-get update && apt-get install -y --no-install-recommends \
    libgl1-mesa-glx \
    libglib2.0-0 \
    libxcb-cursor0 \
    libxcb-icccm4 \
    libxcb-image0 \
    libxcb-keysyms1 \
    libxcb-randr0 \
    libxcb-render-util0 \
    libxcb-shape0 \
    libxcb-sync1 \
    libxcb-util1 \
    libxcb-xfixes0 \
    libxcb-xinerama0 \
    libxcb-xkb1 \
    libxcb1 \
    libxkbcommon-x11-0 \
    libegl1 \
    libfontconfig1 \
    libx11-xcb1 \
    xvfb \
    gcc \
    && rm -rf /var/lib/apt/lists/*

# ── pip 镜像源 → 清华源 (持久化) ──
RUN pip config set global.index-url https://pypi.tuna.tsinghua.edu.cn/simple && \
    pip config set global.trusted-host pypi.tuna.tsinghua.edu.cn

# ── Python 环境 ──
WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt \
    && rm requirements.txt

# ── 项目代码 ──
COPY strategy_engine/ ./strategy_engine/
COPY market_discovery/ ./market_discovery/
COPY weather_data_ingestion/ ./weather_data_ingestion/
COPY gui/ ./gui/
COPY proto/ ./proto/
COPY run_paper_trading.py .
COPY *.py ./

# ── 数据目录 (挂载卷) ──
RUN mkdir -p /app/data /app/data/raw /app/data/signals /app/data/backtest

# ── entrypoint ──
COPY docker-entrypoint.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["daemon"]