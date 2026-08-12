#!/bin/bash
# docker-entrypoint.sh — AI Weather 容器入口
# 用法:
#   docker compose run app daemon          # 常驻守护 (市场发现 + 策略)
#   docker compose run app paper-trading   # 纸面交易测试
#   docker compose run app dashboard       # GUI 仪表盘
#   docker compose run app market-status   # 查看已发现市场

set -e

MODE="${1:-daemon}"

log() { echo "[$(date '+%H:%M:%S')] $*"; }

case "$MODE" in
    daemon)
        log "启动常驻守护进程: market_discovery (10min) + strategy_engine"
        # 后台运行市场发现
        python -m market_discovery --daemon --interval 600 &
        PID_DISCOVERY=$!
        log "market_discovery PID=$PID_DISCOVERY"

        # 保持容器运行，监听信号
        trap "log 收到退出信号; kill $PID_DISCOVERY 2>/dev/null; exit 0" TERM INT
        while kill -0 $PID_DISCOVERY 2>/dev/null; do
            sleep 5
        done
        log "market_discovery 异常退出"
        exit 1
        ;;

    paper-trading)
        log "运行端到端纸面交易测试"
        exec python run_paper_trading.py
        ;;

    dashboard)
        log "启动 GUI 仪表盘 (Xvfb headless)"
        Xvfb :99 -screen 0 1280x800x24 &
        export DISPLAY=:99
        exec python -m gui.dashboard
        ;;

    market-status)
        exec python -m market_discovery --status
        ;;

    evaluate)
        log "扫描新盘 + 策略评估 (单次)"
        exec python -m market_discovery --run-once --evaluate
        ;;

    shell)
        exec /bin/bash
        ;;

    *)
        echo "用法: docker compose run app {daemon|paper-trading|dashboard|market-status|evaluate|shell}"
        exit 1
        ;;
esac
