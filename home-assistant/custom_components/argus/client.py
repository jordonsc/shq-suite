"""WebSocket client for the Argus control surface."""
import asyncio
import json
import logging
from typing import Any, Dict
import websockets

_LOGGER = logging.getLogger(__name__)


class ArgusClient:
    """Client for communicating with the Argus daemon's control WebSocket.

    The protocol is push-only for state: the server emits a ``status`` message on
    connect and on every change. Commands (``acknowledge`` / ``standdown``) are
    fire-and-forget — the daemon may or may not reply with an ``ack``; the next
    ``status`` push reflects the change, so we never depend on a reply.
    """

    def __init__(self, host: str, port: int = 8770):
        """Initialize the client."""
        self.host = host
        self.port = port
        self.uri = f"ws://{host}:{port}/control"
        self._websocket = None
        self._connected = False

    async def connect(self) -> bool:
        """Connect to the server."""
        try:
            self._websocket = await websockets.connect(self.uri)
            self._connected = True
            _LOGGER.info(f"Connected to {self.uri}")
            return True
        except Exception as e:
            _LOGGER.error(f"Failed to connect to {self.uri}: {e}")
            self._connected = False
            return False

    async def disconnect(self):
        """Disconnect from the server."""
        if self._websocket:
            await self._websocket.close()
            self._connected = False

    async def send_command(self, command: str) -> bool:
        """Send a control command (fire-and-forget).

        We do not wait for a reply — the daemon reflects the change in the next
        ``status`` push.
        """
        if not self._connected or not self._websocket:
            _LOGGER.warning("Not connected, cannot send command")
            return False

        try:
            await self._websocket.send(json.dumps({
                "type": "command",
                "command": command,
            }))
            _LOGGER.debug(f"Sent command: {command}")
            return True
        except websockets.exceptions.ConnectionClosed:
            _LOGGER.error("Connection closed while sending command")
            self._connected = False
            return False
        except Exception as e:
            _LOGGER.error(f"Error sending command: {e}")
            self._connected = False
            return False

    async def acknowledge(self) -> bool:
        """Acknowledge the active case."""
        return await self.send_command("acknowledge")

    async def standdown(self) -> bool:
        """Stand down the active case."""
        return await self.send_command("standdown")

    async def start_receiving(self, callback):
        """Start receiving messages and call callback for each message."""
        if not self._websocket:
            _LOGGER.error("Not connected to server")
            return

        try:
            async for message in self._websocket:
                try:
                    data = json.loads(message)
                    _LOGGER.debug(f"Received message: {data}")
                    callback(data)
                except json.JSONDecodeError:
                    _LOGGER.error(f"Invalid JSON received: {message}")
                except Exception as e:
                    _LOGGER.error(f"Error processing message: {e}")
        except Exception as e:
            _LOGGER.error(f"Error in receive loop: {e}")
            self._connected = False
