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

        # R5-009（方案 1）：市场发现子进程退出时**不退出容器**——保持容器 Up，
        # 交由 healthcheck（探测 `market_discovery` argv）在 3 次重试后转 `unhealthy`。
        # 原实现 `exit 1` 会立即触发 `restart: unless-stopped`，healthcheck 来不及反映。
        EXITED=0
        while true; do
            if [ "$EXITED" -eq 0 ] && ! kill -0 "$PID_DISCOVERY" 2>/dev/null; then
                log "market_discovery 异常退出；容器保持运行，healthcheck 将转 unhealthy（等待人工/编排处置）"
                EXITED=1
            fi
            sleep 5
        done
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
