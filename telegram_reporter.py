"""
Telegram Reporter for Trading Bot
Sends formatted trading session reports via Telegram Bot API.
"""

import logging
from urllib.request import urlopen, Request
from urllib.parse import quote
from urllib.error import URLError

logger = logging.getLogger("telegram")


class TelegramReporter:
    """Send trading reports to Telegram."""

    API_URL = "https://api.telegram.org/bot{token}/sendMessage"

    def __init__(self, bot_token: str, chat_id: str):
        self.bot_token = bot_token
        self.chat_id = chat_id

    def _format_report(self, report: dict) -> str:
        """Format report as a readable Telegram message."""
        pnl = report.get("pnl", 0)
        pnl_emoji = "+" if pnl >= 0 else ""
        end_bal = report.get("end_balance", {})

        lines = [
            f"--- Trading Report ---",
            f"Symbol: {report.get('symbol', 'N/A')}",
            f"Time: {report.get('start_time', 'N/A')}",
            f"Duration: {report.get('duration_seconds', 0)}s",
            "",
            f"Orders: {report.get('total_orders', 0)} "
            f"({report.get('buy_orders', 0)} buys, {report.get('sell_orders', 0)} sells)",
            f"Open positions: {report.get('positions_open', 0)}",
            "",
            f"Balance: ${end_bal.get('USD', 0):.2f} USD | {end_bal.get('BTC', 0):.6f} BTC",
            f"PnL: {pnl_emoji}${pnl:.2f} ({pnl_emoji}{report.get('pnl_pct', 0):.2f}%)",
        ]
        return "\n".join(lines)

    def send_report(self, report: dict) -> bool:
        """Send a formatted report to Telegram."""
        message = self._format_report(report)
        return self.send_message(message)

    def send_message(self, text: str) -> bool:
        """Send a text message via Telegram Bot API."""
        url = self.API_URL.format(token=self.bot_token)
        params = f"chat_id={quote(self.chat_id)}&text={quote(text)}&parse_mode=HTML"
        full_url = f"{url}?{params}"

        try:
            req = Request(full_url, method="GET")
            with urlopen(req, timeout=10) as resp:
                if resp.status == 200:
                    logger.info("Telegram report sent successfully")
                    return True
                else:
                    logger.error(f"Telegram API returned status {resp.status}")
                    return False
        except URLError as e:
            logger.error(f"Failed to send Telegram message: {e}")
            return False
        except Exception as e:
            logger.error(f"Unexpected error sending Telegram message: {e}")
            return False
