"""
Trading Bot Scheduler
Runs automatically every day at 08:00 UTC (15:00 Bangkok) via APScheduler.
Analyzes market, executes trades, sends Telegram report.
Deploys on Railway as a background worker.

Usage:
    python main.py

Environment variables:
    BINANCE_API_KEY     - Binance API key (if set, uses live Binance)
    BINANCE_API_SECRET  - Binance API secret
    BINANCE_TESTNET     - "true" to use Binance testnet (recommended first)
    TELEGRAM_BOT_TOKEN  - Telegram bot token from @BotFather
    TELEGRAM_CHAT_ID    - Your Telegram chat ID
    TRADING_SYMBOL      - Trading pair (default: BTC/USDT)
    MAX_TICKS           - Analysis ticks per session (default: 50)
"""

import os
import signal
import sys
import logging
from datetime import datetime

from dotenv import load_dotenv

load_dotenv()

from apscheduler.schedulers.blocking import BlockingScheduler
from apscheduler.triggers.cron import CronTrigger

from trading_bot import (
    MockExchange,
    SimpleMovingAverageStrategy,
    RiskManager,
    TradingBot,
)
from telegram_reporter import TelegramReporter

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
)
logger = logging.getLogger("scheduler")

# Configuration from environment
TELEGRAM_BOT_TOKEN = os.getenv("TELEGRAM_BOT_TOKEN", "")
TELEGRAM_CHAT_ID = os.getenv("TELEGRAM_CHAT_ID", "")
TRADING_SYMBOL = os.getenv("TRADING_SYMBOL", "BTC/USDT")
MAX_TICKS = int(os.getenv("MAX_TICKS", "50"))


def _create_exchange():
    """Create exchange: Binance if credentials set, otherwise Mock."""
    if os.getenv("BINANCE_API_KEY") and os.getenv("BINANCE_API_SECRET"):
        from binance_exchange import BinanceExchange
        logger.info("Using LIVE Binance exchange")
        return BinanceExchange()
    else:
        logger.info("No Binance credentials - using MockExchange")
        return MockExchange(initial_balance=10000)


def run_trading_session():
    """Execute one full trading session: analyze, trade, report."""
    logger.info("=== Trading session started ===")
    start_time = datetime.utcnow()

    # Initialize components
    exchange = _create_exchange()
    strategy = SimpleMovingAverageStrategy(short_window=5, long_window=15)
    risk_manager = RiskManager(max_position_pct=0.2, stop_loss_pct=0.05)
    bot = TradingBot(exchange, strategy, risk_manager)

    # Record starting balance
    start_balance = exchange.get_balance()

    # Run analysis and trading
    bot.run(TRADING_SYMBOL, interval=0.1, max_ticks=MAX_TICKS)

    # Collect results
    end_balance = exchange.get_balance()
    end_time = datetime.utcnow()
    duration = (end_time - start_time).total_seconds()

    total_orders = len(exchange.orders)
    buy_orders = sum(1 for o in exchange.orders if o.type.value == "BUY")
    sell_orders = sum(1 for o in exchange.orders if o.type.value == "SELL")

    # Support both MockExchange (USD) and Binance (USDT) balance keys
    start_stable = start_balance.get("USDT", start_balance.get("USD", 0))
    end_stable = end_balance.get("USDT", end_balance.get("USD", 0))
    end_btc = end_balance.get("BTC", 0)
    last_price = strategy.price_history[-1] if strategy.price_history else 0

    start_value = start_stable
    end_value = end_stable + end_btc * last_price
    pnl = end_value - start_value
    pnl_pct = (pnl / start_value * 100) if start_value > 0 else 0

    report = {
        "symbol": TRADING_SYMBOL,
        "start_time": start_time.strftime("%Y-%m-%d %H:%M UTC"),
        "duration_seconds": round(duration, 1),
        "start_balance": start_balance,
        "end_balance": end_balance,
        "total_orders": total_orders,
        "buy_orders": buy_orders,
        "sell_orders": sell_orders,
        "pnl": round(pnl, 2),
        "pnl_pct": round(pnl_pct, 2),
        "positions_open": len(bot.positions),
    }

    logger.info(f"Session complete: PnL ${pnl:.2f} ({pnl_pct:.2f}%)")
    logger.info(f"Orders: {total_orders} total ({buy_orders} buys, {sell_orders} sells)")

    # Send Telegram report
    if TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID:
        reporter = TelegramReporter(TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID)
        reporter.send_report(report)
    else:
        logger.warning("Telegram not configured - skipping report. Set TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID.")

    logger.info("=== Trading session finished ===")
    return report


def main():
    """Start the scheduler. Runs trading session daily at 08:00 UTC."""
    logger.info("Trading Bot Scheduler starting up")
    logger.info(f"Symbol: {TRADING_SYMBOL} | Max ticks: {MAX_TICKS}")
    logger.info("Scheduled: daily at 08:00 UTC (15:00 Bangkok)")

    scheduler = BlockingScheduler()

    # Daily at 08:00 UTC
    scheduler.add_job(
        run_trading_session,
        trigger=CronTrigger(hour=8, minute=0, timezone="UTC"),
        id="daily_trading_session",
        name="Daily Trading Session",
        misfire_grace_time=3600,  # allow 1h late execution if missed
    )

    # Run once immediately on startup for verification
    logger.info("Running initial session on startup...")
    try:
        run_trading_session()
    except Exception as e:
        logger.error(f"Initial session failed: {e}")

    # Graceful shutdown
    def shutdown(signum, frame):
        logger.info("Shutting down scheduler...")
        scheduler.shutdown(wait=False)
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)

    logger.info("Scheduler running. Next session at 08:00 UTC. Press Ctrl+C to stop.")
    try:
        scheduler.start()
    except (KeyboardInterrupt, SystemExit):
        logger.info("Scheduler stopped.")


if __name__ == "__main__":
    main()
