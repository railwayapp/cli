"""
Simple Trading Bot Framework
A modular, easy-to-modify trading bot with clear separation of concerns
"""

import time
import json
import logging
from datetime import datetime
from typing import Dict, List, Optional, Tuple
from dataclasses import dataclass
from enum import Enum
from abc import ABC, abstractmethod

# Configure logging

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


# Data structures

class OrderType(Enum):
    BUY = "BUY"
    SELL = "SELL"


class OrderStatus(Enum):
    PENDING = "PENDING"
    FILLED = "FILLED"
    CANCELLED = "CANCELLED"


@dataclass
class MarketData:
    """Market data structure"""
    symbol: str
    price: float
    volume: float
    timestamp: datetime
    bid: float
    ask: float


@dataclass
class Order:
    """Order structure"""
    id: str
    symbol: str
    type: OrderType
    quantity: float
    price: float
    status: OrderStatus
    timestamp: datetime


@dataclass
class Position:
    """Position tracking"""
    symbol: str
    quantity: float
    entry_price: float
    current_price: float
    unrealized_pnl: float


class BaseExchange(ABC):
    """Abstract base class for exchange connections"""

    @abstractmethod
    def get_market_data(self, symbol: str) -> MarketData:
        """Fetch current market data"""
        pass

    @abstractmethod
    def place_order(self, order: Order) -> bool:
        """Place an order"""
        pass

    @abstractmethod
    def get_balance(self) -> Dict[str, float]:
        """Get account balance"""
        pass


class MockExchange(BaseExchange):
    """Mock exchange for testing"""

    def __init__(self, initial_balance: float = 10000):
        self.balance = {"USD": initial_balance, "BTC": 0}
        self.orders = []

    def get_market_data(self, symbol: str) -> MarketData:
        """Simulate market data"""
        import random
        base_price = 50000
        price = base_price + random.uniform(-100, 100)
        return MarketData(
            symbol=symbol,
            price=price,
            volume=random.uniform(100, 1000),
            timestamp=datetime.now(),
            bid=price - 1,
            ask=price + 1
        )

    def place_order(self, order: Order) -> bool:
        """Simulate order placement"""
        if order.type == OrderType.BUY:
            cost = order.quantity * order.price
            if self.balance["USD"] >= cost:
                self.balance["USD"] -= cost
                self.balance["BTC"] += order.quantity
                order.status = OrderStatus.FILLED
                self.orders.append(order)
                logger.info(f"BUY order filled: {order.quantity} @ {order.price}")
                return True
        elif order.type == OrderType.SELL:
            if self.balance["BTC"] >= order.quantity:
                self.balance["BTC"] -= order.quantity
                self.balance["USD"] += order.quantity * order.price
                order.status = OrderStatus.FILLED
                self.orders.append(order)
                logger.info(f"SELL order filled: {order.quantity} @ {order.price}")
                return True
        return False

    def get_balance(self) -> Dict[str, float]:
        return self.balance.copy()


class BaseStrategy(ABC):
    """Abstract base class for trading strategies"""

    @abstractmethod
    def should_buy(self, market_data: MarketData, portfolio: Dict) -> Tuple[bool, float]:
        """Returns (should_buy, quantity)"""
        pass

    @abstractmethod
    def should_sell(self, market_data: MarketData, portfolio: Dict) -> Tuple[bool, float]:
        """Returns (should_sell, quantity)"""
        pass


class SimpleMovingAverageStrategy(BaseStrategy):
    """Simple MA crossover strategy"""

    def __init__(self, short_window: int = 10, long_window: int = 30):
        self.short_window = short_window
        self.long_window = long_window
        self.price_history = []

    def update_price_history(self, price: float):
        """Update price history"""
        self.price_history.append(price)
        if len(self.price_history) > self.long_window:
            self.price_history.pop(0)

    def calculate_ma(self, window: int) -> Optional[float]:
        """Calculate moving average for the given window"""
        if len(self.price_history) < window:
            return None
        return sum(self.price_history[-window:]) / window

    def should_buy(self, market_data: MarketData, portfolio: Dict) -> Tuple[bool, float]:
        """Buy when short MA crosses above long MA"""
        self.update_price_history(market_data.price)
        short_ma = self.calculate_ma(self.short_window)
        long_ma = self.calculate_ma(self.long_window)

        if short_ma is None or long_ma is None:
            return False, 0

        if short_ma > long_ma and portfolio.get("USD", 0) > 0:
            quantity = (portfolio["USD"] * 0.1) / market_data.price
            return True, quantity

        return False, 0

    def should_sell(self, market_data: MarketData, portfolio: Dict) -> Tuple[bool, float]:
        """Sell when short MA crosses below long MA"""
        short_ma = self.calculate_ma(self.short_window)
        long_ma = self.calculate_ma(self.long_window)

        if short_ma is None or long_ma is None:
            return False, 0

        if short_ma < long_ma and portfolio.get("BTC", 0) > 0:
            quantity = portfolio["BTC"] * 0.5
            return True, quantity

        return False, 0


class RiskManager:
    """Simple risk management"""

    def __init__(self, max_position_pct: float = 0.2, stop_loss_pct: float = 0.05):
        self.max_position_pct = max_position_pct
        self.stop_loss_pct = stop_loss_pct

    def check_order(self, order: Order, portfolio: Dict, market_data: MarketData) -> bool:
        """Validate order against risk rules"""
        if order.type == OrderType.BUY:
            cost = order.quantity * order.price
            total_value = portfolio.get("USD", 0) + portfolio.get("BTC", 0) * market_data.price
            if total_value == 0:
                return False
            if cost / total_value > self.max_position_pct:
                logger.warning(f"Order rejected: exceeds max position size ({self.max_position_pct:.0%})")
                return False
        return True

    def check_stop_loss(self, position: Position) -> bool:
        """Check if stop loss is triggered"""
        if position.entry_price == 0:
            return False
        loss_pct = (position.entry_price - position.current_price) / position.entry_price
        if loss_pct >= self.stop_loss_pct:
            logger.warning(f"Stop loss triggered for {position.symbol}: {loss_pct:.2%} loss")
            return True
        return False


class TradingBot:
    """Main trading bot that ties everything together"""

    def __init__(self, exchange: BaseExchange, strategy: BaseStrategy, risk_manager: RiskManager):
        self.exchange = exchange
        self.strategy = strategy
        self.risk_manager = risk_manager
        self.positions: Dict[str, Position] = {}
        self.order_count = 0
        self.running = False

    def create_order(self, symbol: str, order_type: OrderType, quantity: float, price: float) -> Order:
        """Create a new order"""
        self.order_count += 1
        return Order(
            id=f"ORD-{self.order_count:06d}",
            symbol=symbol,
            type=order_type,
            quantity=quantity,
            price=price,
            status=OrderStatus.PENDING,
            timestamp=datetime.now()
        )

    def update_position(self, symbol: str, market_data: MarketData):
        """Update position with current market data"""
        if symbol in self.positions:
            pos = self.positions[symbol]
            pos.current_price = market_data.price
            pos.unrealized_pnl = (market_data.price - pos.entry_price) * pos.quantity

    def run_tick(self, symbol: str):
        """Run one trading cycle"""
        market_data = self.exchange.get_market_data(symbol)
        portfolio = self.exchange.get_balance()

        self.update_position(symbol, market_data)

        # Check stop losses
        if symbol in self.positions:
            if self.risk_manager.check_stop_loss(self.positions[symbol]):
                pos = self.positions[symbol]
                order = self.create_order(symbol, OrderType.SELL, pos.quantity, market_data.price)
                if self.exchange.place_order(order):
                    del self.positions[symbol]
                    logger.info(f"Stop loss executed for {symbol}")
                return

        # Check buy signal
        should_buy, buy_qty = self.strategy.should_buy(market_data, portfolio)
        if should_buy and buy_qty > 0:
            order = self.create_order(symbol, OrderType.BUY, buy_qty, market_data.price)
            if self.risk_manager.check_order(order, portfolio, market_data):
                if self.exchange.place_order(order):
                    self.positions[symbol] = Position(
                        symbol=symbol,
                        quantity=buy_qty,
                        entry_price=market_data.price,
                        current_price=market_data.price,
                        unrealized_pnl=0
                    )

        # Check sell signal
        should_sell, sell_qty = self.strategy.should_sell(market_data, portfolio)
        if should_sell and sell_qty > 0:
            order = self.create_order(symbol, OrderType.SELL, sell_qty, market_data.price)
            if self.exchange.place_order(order):
                if symbol in self.positions:
                    self.positions[symbol].quantity -= sell_qty
                    if self.positions[symbol].quantity <= 0:
                        del self.positions[symbol]

    def run(self, symbol: str, interval: float = 1.0, max_ticks: Optional[int] = None):
        """Run the bot"""
        self.running = True
        tick = 0
        logger.info(f"Starting trading bot for {symbol}")
        logger.info(f"Initial balance: {self.exchange.get_balance()}")

        try:
            while self.running:
                self.run_tick(symbol)
                tick += 1

                if tick % 10 == 0:
                    balance = self.exchange.get_balance()
                    logger.info(f"Tick {tick} | Balance: {balance}")

                if max_ticks and tick >= max_ticks:
                    logger.info(f"Reached max ticks ({max_ticks}), stopping")
                    break

                time.sleep(interval)
        except KeyboardInterrupt:
            logger.info("Bot stopped by user")
        finally:
            self.running = False
            balance = self.exchange.get_balance()
            logger.info(f"Final balance: {balance}")

    def stop(self):
        """Stop the bot"""
        self.running = False


if __name__ == "__main__":
    exchange = MockExchange(initial_balance=10000)
    strategy = SimpleMovingAverageStrategy(short_window=5, long_window=15)
    risk_manager = RiskManager(max_position_pct=0.2, stop_loss_pct=0.05)

    bot = TradingBot(exchange, strategy, risk_manager)
    bot.run("BTC/USD", interval=0.5, max_ticks=100)
