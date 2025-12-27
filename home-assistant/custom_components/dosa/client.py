"""WebSocket client for DOSA server."""
import asyncio
import json
import logging
from typing import Optional, Dict, Any
import websockets

_LOGGER = logging.getLogger(__name__)


class DosaClient:
    """Client for communicating with DOSA server."""

    def __init__(self, host: str, port: int = 8766):
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
        # Close existing connection if any
        if self._websocket:
            try:
                await self._websocket.close()
            except Exception:
                pass
            self._websocket = None

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
            _LOGGER.info(f"Sent command: {command} (listening mode: {self._listening})")

            # If we're in listening mode, wait for response from queue
            if self._listening:
                try:
                    # Wait for a response message (status, response, or error)
                    response = await asyncio.wait_for(
                        self._response_queue.get(),
                        timeout=10.0
                    )
                    _LOGGER.debug(f"Received response: {response}")
                    return response
                except asyncio.TimeoutError:
                    _LOGGER.error("Timeout waiting for response")
                    return None
            else:
                # Not in listening mode, read directly
                response = await self._websocket.recv()
                data = json.loads(response)
                _LOGGER.info(f"Received message: {data}")
                return data

        except websockets.exceptions.ConnectionClosed:
            _LOGGER.error("Connection closed")
            self._connected = False
            return None
        except Exception as e:
            _LOGGER.error(f"Error sending command: {e}")
            self._connected = False
            return None

    async def get_status(self) -> Optional[Dict[str, Any]]:
        """Get current status."""
        response = await self._send_command({'type': 'status'})
        return response if response and response.get('type') == 'status' else None

    async def open_door(self) -> bool:
        """Open the door."""
        response = await self._send_command({'type': 'open'})
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def close_door(self) -> bool:
        """Close the door."""
        response = await self._send_command({'type': 'close'})
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def move_to_percent(self, percent: float) -> bool:
        """Move to a specific percentage (0-100)."""
        response = await self._send_command({
            'type': 'move',
            'percent': percent
        })
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def jog(self, distance: float, feed_rate: float = None) -> bool:
        """Jog the door by a relative distance in mm."""
        command = {
            'type': 'jog',
            'distance': distance
        }
        if feed_rate is not None:
            command['feed_rate'] = feed_rate

        response = await self._send_command(command)
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def home(self) -> bool:
        """Home the door."""
        response = await self._send_command({'type': 'home'})
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def zero(self) -> bool:
        """Zero the door at current position."""
        response = await self._send_command({'type': 'zero'})
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def clear_alarm(self) -> bool:
        """Clear CNC alarm."""
        response = await self._send_command({'type': 'clear_alarm'})
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def stop(self) -> bool:
        """Emergency stop."""
        response = await self._send_command({'type': 'stop'})
        if response and response.get('type') == 'response':
            return response.get('success', False)
        return False

    async def _keepalive_loop(self):
        """Send NOOP commands every 30 seconds to keep connection alive."""
        import asyncio
        while self._connected:
            try:
                await asyncio.sleep(30)
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
                    _LOGGER.debug(f"WebSocket received message: {data}")

                    msg_type = data.get('type')

                    # Response, error, and status messages go to the response queue for command handlers
                    # Status broadcasts also trigger callbacks for real-time updates
                    if msg_type in ('response', 'error', 'status'):
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
            # Close websocket when listening stops
            if self._websocket:
                try:
                    await self._websocket.close()
                except Exception:
                    pass
                self._websocket = None
