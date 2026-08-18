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
            # Anything still queued belongs to the socket we just replaced. Carrying it
            # over would make the next command read a reply to a request that no longer
            # exists, so start every connection with an empty queue.
            self._drain_queue()
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

    def _drain_queue(self):
        """Discard anything sitting in the response queue."""
        dropped = 0
        while True:
            try:
                self._response_queue.get_nowait()
                dropped += 1
            except asyncio.QueueEmpty:
                break
        if dropped:
            _LOGGER.debug(f"Discarded {dropped} stale queued message(s)")

    @staticmethod
    def _is_reply_to(message: Dict[str, Any], expect_type: str, cmd_name: str) -> bool:
        """Is this message the reply to the command we just sent?

        The queue also carries unsolicited status broadcasts and the replies to the
        15 s keep-alive NOOPs, so the head of the queue is usually NOT our reply.
        The server tags every Response with the originating command, which lets us
        match precisely; a Status satisfies a status request (an unsolicited
        broadcast is still a valid current status).
        """
        msg_type = message.get('type')
        if msg_type == 'error':
            # Errors carry no command tag, but they do terminate a request — surface
            # it rather than blocking until the timeout.
            return True
        if expect_type == 'status':
            return msg_type == 'status'
        return msg_type == 'response' and message.get('command') == cmd_name

    async def _send_command(
        self,
        command: Dict[str, Any],
        expect_type: str = 'response',
    ) -> Optional[Dict[str, Any]]:
        """Send a command and wait for the reply that matches it."""
        if not self._connected:
            if not await self.connect():
                return None

        cmd_name = command.get('type')

        try:
            # Send command
            await self._websocket.send(json.dumps(command))
            _LOGGER.debug(f"Sent command: {command} (listening mode: {self._listening})")

            # If we're in listening mode, wait for response from queue
            if self._listening:
                loop = asyncio.get_running_loop()
                deadline = loop.time() + 10.0
                while True:
                    remaining = deadline - loop.time()
                    if remaining <= 0:
                        _LOGGER.error(f"Timeout waiting for reply to '{cmd_name}'")
                        return None
                    try:
                        message = await asyncio.wait_for(
                            self._response_queue.get(),
                            timeout=remaining
                        )
                    except asyncio.TimeoutError:
                        _LOGGER.error(f"Timeout waiting for reply to '{cmd_name}'")
                        return None
                    if self._is_reply_to(message, expect_type, cmd_name):
                        _LOGGER.debug(f"Received reply to '{cmd_name}': {message}")
                        return message
                    # Not ours (a status broadcast or a NOOP reply) — keep waiting.
                    _LOGGER.debug(f"Skipping unrelated message while awaiting '{cmd_name}'")
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
        response = await self._send_command({'type': 'status'}, expect_type='status')
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
