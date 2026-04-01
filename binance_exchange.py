"""
Binance Exchange integration implementing BaseExchange interface.

Environment variables:
    BINANCE_API_KEY     - Your Binance API key
    BINANCE_API_SECRET  - Your Binance API secret
    BINANCE_TESTNET     - Set to "true" to use testnet (recommended for testing)
"""

import os
import logging
from datetime import datetime
from typing import Dict

from binance.client import Client
from binance.exceptions import BinanceAPIException

from trading_bot import BaseExchange, MarketData, Order, OrderType, OrderStatus

logger = logging.getLogger("binance_exchange")


class BinanceExchange(BaseExchange):
    """Live Binance exchange connection."""

    def __init__(self):
        api_key = os.getenv("BINANCE_API_KEY", "")
        api_secret = os.getenv("BINANCE_API_SECRET", "")
        use_testnet = os.getenv("BINANCE_TESTNET", "false").lower() == "true"

        if not api_key or not api_secret:
            raise ValueError("BINANCE_API_KEY and BINANCE_API_SECRET must be set")

        self.client = Client(api_key, api_secret, testnet=use_testnet)
        self.orders = []

        env_label = "TESTNET" if use_testnet else "LIVE"
        logger.info(f"Binance client initialized ({env_label})")

    @staticmethod
    def _normalize_symbol(symbol: str) -> str:
        """Convert 'BTC/USDT' to 'BTCUSDT' for Binance API."""
        return symbol.replace("/", "")

    def get_market_data(self, symbol: str) -> MarketData:
        """Fetch current market data from Binance."""
        binance_symbol = self._normalize_symbol(symbol)
        ticker = self.client.get_ticker(symbol=binance_symbol)
        book = self.client.get_order_book(symbol=binance_symbol, limit=5)

        price = float(ticker["lastPrice"])
        return MarketData(
            symbol=symbol,
            price=price,
            volume=float(ticker["volume"]),
            timestamp=datetime.utcnow(),
            bid=float(book["bids"][0][0]) if book["bids"] else price,
            ask=float(book["asks"][0][0]) if book["asks"] else price,
        )

    def place_order(self, order: Order) -> bool:
        """Place a real order on Binance."""
        binance_symbol = self._normalize_symbol(order.symbol)
        side = "BUY" if order.type == OrderType.BUY else "SELL"

        try:
            result = self.client.create_order(
                symbol=binance_symbol,
                side=side,
                type="MARKET",
                quantity=self._format_quantity(binance_symbol, order.quantity),
            )
            order.status = OrderStatus.FILLED
            order.price = float(result.get("fills", [{}])[0].get("price", order.price))
            self.orders.append(order)
            logger.info(
                f"{side} order filled: {order.quantity} {order.symbol} @ {order.price}"
            )
            return True
        except BinanceAPIException as e:
            logger.error(f"Binance order failed: {e.message}")
            order.status = OrderStatus.CANCELLED
            return False

    def _format_quantity(self, binance_symbol: str, quantity: float) -> str:
        """Round quantity to the symbol's allowed step size."""
        info = self.client.get_symbol_info(binance_symbol)
        if info:
            for f in info["filters"]:
                if f["filterType"] == "LOT_SIZE":
                    step = float(f["stepSize"])
                    precision = len(f["stepSize"].rstrip("0").split(".")[-1])
                    quantity = round(quantity - (quantity % step), precision)
                    break
        return f"{quantity}"

    def get_balance(self) -> Dict[str, float]:
        """Get account balances from Binance."""
        account = self.client.get_account()
        balances = {}
        for asset in account["balances"]:
            free = float(asset["free"])
            if free > 0:
                balances[asset["asset"]] = free
        return balances
