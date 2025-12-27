"""WebSocket client for SHQ Display server."""
import asyncio
import json
import logging
from typing import Optional, Dict, Any
import websockets

_LOGGER = logging.getLogger(__name__)


class SHQDisplayClient:
    """Client for communicating with SHQ Display server."""

    def __init__(self, host: str, port: int = 8765):
        """Initialize the client."""
        self.host = host
        self.port = port
        self.uri = f"ws://{host}:{port}"
        self._websocket = None
        self._connected = False
        self._keepalive_task = None
        self._response_queue: asyncio.Queue = asyncio.Queue()
        self._listening = False

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
        # Stop keepalive task
        if self._keepalive_task and not self._keepalive_task.done():
            self._keepalive_task.cancel()
            try:
                await self._keepalive_task
            except Exception:
                pass

        if self._websocket:
            await self._websocket.close()
            self._connected = False

    async def _send_command(self, command: Dict[str, Any]) -> Optional[Dict[str, Any]]:
        """Send a command and wait for response."""
        if not self._connected:
            if not await self.connect():
                return None

        try:
            # Send command
            await self._websocket.send(json.dumps(command))
            _LOGGER.debug(f"Sent command: {command} (listening mode: {self._listening})")

            # If we're in listening mode, wait for response from queue
            if self._listening:
                try:
                    # Wait for a response message (not metrics broadcasts)
                    while True:
                        response = await asyncio.wait_for(
                            self._response_queue.get(),
                            timeout=10.0
                        )
                        # Make sure it's actually a response, not a metrics broadcast that snuck in
                        if response.get('type') == 'response':
                            _LOGGER.debug(f"Received response: {response}")
                            return response
                        else:
                            # This shouldn't happen, but if it does, keep waiting
                            _LOGGER.warning(f"Received non-response message in response queue: {response.get('type')}")
                except asyncio.TimeoutError:
                    _LOGGER.error("Timeout waiting for response")
                    return None
            else:
                # Not in listening mode, read directly
                # Keep reading until we get a response (skip metrics broadcasts)
                while True:
                    response = await self._websocket.recv()
                    data = json.loads(response)
                    _LOGGER.debug(f"Received message: {data}")

                    # If it's a response, return it
                    if data.get('type') == 'response':
                        return data
                    # If it's a metrics broadcast, skip it and keep reading
                    elif data.get('type') == 'metrics':
                        _LOGGER.debug("Skipping metrics broadcast, waiting for response...")
                        continue
                    else:
                        # Unknown message type, return it anyway
                        return data

        except websockets.exceptions.ConnectionClosed:
            _LOGGER.error("Connection closed")
            self._connected = False
            return None
        except Exception as e:
            _LOGGER.error(f"Error sending command: {e}")
            self._connected = False
            return None

    async def get_metrics(self) -> Optional[Dict[str, Any]]:
        """Get current metrics."""
        response = await self._send_command({'type': 'get_metrics'})
        return response if response and response.get('success') else None

    async def set_brightness(self, brightness: int) -> bool:
        """Set brightness (0-10)."""
        response = await self._send_command({
            'type': 'set_brightness',
            'brightness': brightness
        })
        return response.get('success', False) if response else False

    async def set_display_state(self, state: bool) -> bool:
        """Set display on/off state."""
        response = await self._send_command({
            'type': 'set_display',
            'state': state
        })
        return response.get('success', False) if response else False

    async def wake(self) -> bool:
        """Wake display to bright level."""
        response = await self._send_command({'type': 'wake'})
        return response.get('success', False) if response else False

    async def sleep(self) -> bool:
        """Sleep display (turn off)."""
        response = await self._send_command({'type': 'sleep'})
        return response.get('success', False) if response else False

    async def set_auto_dim_config(
        self,
        dim_level: Optional[int] = None,
        bright_level: Optional[int] = None,
        auto_dim_time: Optional[int] = None,
        auto_off_time: Optional[int] = None
    ) -> bool:
        """Set auto-dim configuration."""
        command = {'type': 'set_auto_dim_config'}
        if dim_level is not None:
            command['dim_level'] = dim_level
        if bright_level is not None:
            command['bright_level'] = bright_level
        if auto_dim_time is not None:
            command['auto_dim_time'] = auto_dim_time
        if auto_off_time is not None:
            command['auto_off_time'] = auto_off_time

        response = await self._send_command(command)
        return response.get('success', False) if response else False

    async def get_auto_dim_config(self) -> Optional[Dict[str, Any]]:
        """Get auto-dim configuration."""
        response = await self._send_command({'type': 'get_auto_dim_config'})
        return response.get('config') if response and response.get('success') else None

    async def _keepalive_loop(self):
        """Send NOOP commands every 15 seconds to keep connection alive."""
        import asyncio
        while self._connected:
            try:
                await asyncio.sleep(15)
                if self._connected and self._websocket:
                    await self._websocket.send(json.dumps({'type': 'noop'}))
                    _LOGGER.debug("Sent keepalive NOOP")
            except Exception as e:
                _LOGGER.debug(f"Keepalive error: {e}")
                break

    async def start_receiving(self, callback):
        """Start receiving messages and call callback for each message."""
        if not self._websocket:
            _LOGGER.error("Not connected to server")
            return

        self._listening = True

        # Start keepalive task
        import asyncio
        self._keepalive_task = asyncio.create_task(self._keepalive_loop())

        try:
            async for message in self._websocket:
                try:
                    data = json.loads(message)
                    _LOGGER.debug(f"Received message: {data}")

                    msg_type = data.get('type')

                    # Response messages go to the response queue for command handlers
                    if msg_type == 'response':
                        await self._response_queue.put(data)

                    # All messages also go to the callback if set
                    callback(data)

                except json.JSONDecodeError:
                    _LOGGER.error(f"Invalid JSON received: {message}")
                except Exception as e:
                    _LOGGER.error(f"Error processing message: {e}")
        except Exception as e:
            _LOGGER.error(f"Error in receive loop: {e}")
            self._connected = False
        finally:
            self._listening = False
