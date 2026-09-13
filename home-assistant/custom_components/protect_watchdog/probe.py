"""Measure how long HA's UniFi Protect event websocket has been silent.

HA core's `unifiprotect` integration talks to the NVR through `uiprotect`, whose
websocket loop is::

    msg = await self._ws_connection.receive(self.receive_timeout)

`receive_timeout` defaults to `None`, and `ws_connect()` is called without an
aiohttp `heartbeat`, so neither side of the connection is probed for liveness.
If the NVR stops sending, that `await` blocks forever: no error, no
`WebsocketState` change, no reconnect, and nothing logged. Every event-driven
detection binary_sensor silently freezes while the 15-minute REST bootstrap poll
keeps the cameras looking perfectly healthy.

That is not a hypothetical: on 2026-09-10 the stream stopped at 03:03:50 AEST and
was not noticed for 3 days 19 hours, during which every camera automation in the
estate was dead (ledger shq-suite-0054).

No entity in HA reflects this — the integration receives the stream but only
writes state when a value actually changes, so `last_reported` does not tick
either. The only honest signal is the socket, so measure that. sock_diag reports
our own sockets with no privilege, so this needs nothing that a plain HA process
does not already have.

The hass container runs with `--network host`, so /proc/net/tcp is the WHOLE
host's socket table, not just ours. Filtering on "peer is the NVR" alone would
therefore be true only by luck. We intersect with the inodes behind our own
/proc/self/fd instead, which is exact: this module runs inside the HA process,
so those fds are the integration's own sockets.
"""

from __future__ import annotations

import os
import socket
import struct
from typing import NamedTuple

SOCK_DIAG_BY_FAMILY = 20
NETLINK_INET_DIAG = 4
NLM_F_REQUEST = 0x01
NLM_F_DUMP = 0x300
NLMSG_ERROR = 2
NLMSG_DONE = 3
INET_DIAG_INFO = 2
TCP_ESTABLISHED = 1

# struct inet_diag_msg is 72 bytes, then the rtattrs.
_INET_DIAG_MSG_LEN = 72

# struct tcp_info field offsets (include/uapi/linux/tcp.h). Guarded by a length
# check before use, so a kernel that grows the struct is harmless.
_OFF_LAST_DATA_RECV = 52   # __u32 tcpi_last_data_recv, milliseconds
_OFF_BYTES_RECEIVED = 128  # __u64 tcpi_bytes_received
_OFF_SEGS_IN = 140         # __u32 tcpi_segs_in
_MIN_TCP_INFO_LEN = _OFF_SEGS_IN + 4


class ProbeResult(NamedTuple):
    """One measurement of the event websocket."""

    age: float | None      # seconds since the last byte arrived, None if no socket
    sockets: int           # established connections to the NVR
    port: int | None       # local port of the socket we measured
    bytes_received: int | None


def _own_socket_inodes() -> set[int]:
    """Inodes of every socket this process holds open."""
    inodes: set[int] = set()
    try:
        fds = os.listdir("/proc/self/fd")
    except OSError:
        return inodes
    for fd in fds:
        try:
            target = os.readlink(f"/proc/self/fd/{fd}")
        except OSError:
            continue  # the fd closed underneath us; nothing to do about it
        if target.startswith("socket:["):
            inodes.add(int(target[8:-1]))
    return inodes


def _established_to(host: str, port: int) -> set[int]:
    """Local ports of OUR ESTABLISHED sockets to host:port."""
    want_ip = struct.unpack("<I", socket.inet_aton(host))[0]
    ours = _own_socket_inodes()
    found: set[int] = set()
    for path in ("/proc/net/tcp", "/proc/net/tcp6"):
        try:
            with open(path, encoding="ascii") as handle:
                lines = handle.read().splitlines()[1:]
        except OSError:
            continue
        for line in lines:
            fields = line.split()
            if len(fields) < 10 or int(fields[3], 16) != TCP_ESTABLISHED:
                continue
            rem_ip, rem_port = fields[2].split(":")
            # tcp6 carries an IPv4-mapped peer in the final 8 hex digits, and on
            # this core every listener is a dual-stack AF_INET6 socket, so an
            # IPv4 peer legitimately shows up in /proc/net/tcp6.
            if int(rem_ip[-8:], 16) != want_ip or int(rem_port, 16) != port:
                continue
            # Host networking means this table is not ours alone. Anything we do
            # not hold an fd for belongs to another process on the box.
            if ours and int(fields[9]) not in ours:
                continue
            found.add(int(fields[1].split(":")[1], 16))
    return found


def _tcp_info_by_port() -> dict[int, tuple[int, int, int]]:
    """{local_port: (last_data_recv_ms, bytes_received, segs_in)} via sock_diag."""
    # nlmsghdr(16) + inet_diag_req_v2(8) + inet_diag_sockid(48) = 72
    request = struct.pack(
        "=IHHII" + "BBBBI" + "HH" + "4I" + "4I" + "I" + "2I",
        72, SOCK_DIAG_BY_FAMILY, NLM_F_REQUEST | NLM_F_DUMP, 1, 0,
        socket.AF_INET, socket.IPPROTO_TCP, 1 << (INET_DIAG_INFO - 1), 0,
        1 << TCP_ESTABLISHED,
        0, 0,              # idiag_sport, idiag_dport
        0, 0, 0, 0,        # idiag_src[4]
        0, 0, 0, 0,        # idiag_dst[4]
        0,                 # idiag_if
        0, 0,              # idiag_cookie[2]
    )

    out: dict[int, tuple[int, int, int]] = {}
    with socket.socket(socket.AF_NETLINK, socket.SOCK_DGRAM, NETLINK_INET_DIAG) as nl:
        nl.settimeout(5)
        nl.sendall(request)
        while True:
            buf = nl.recv(65535)
            offset = 0
            while offset + 16 <= len(buf):
                nl_len, nl_type = struct.unpack_from("=IH", buf, offset)
                if nl_type == NLMSG_DONE or nl_len < 16:
                    return out
                if nl_type == NLMSG_ERROR:
                    raise OSError("sock_diag returned NLMSG_ERROR")
                msg = buf[offset + 16: offset + nl_len]
                if len(msg) >= _INET_DIAG_MSG_LEN:
                    sport = struct.unpack_from("!H", msg, 4)[0]
                    attr = _INET_DIAG_MSG_LEN
                    while attr + 4 <= len(msg):
                        rta_len, rta_type = struct.unpack_from("=HH", msg, attr)
                        if rta_len < 4:
                            break
                        if rta_type == INET_DIAG_INFO:
                            info = msg[attr + 4: attr + rta_len]
                            if len(info) >= _MIN_TCP_INFO_LEN:
                                out[sport] = (
                                    struct.unpack_from("=I", info, _OFF_LAST_DATA_RECV)[0],
                                    struct.unpack_from("=Q", info, _OFF_BYTES_RECEIVED)[0],
                                    struct.unpack_from("=I", info, _OFF_SEGS_IN)[0],
                                )
                        attr += (rta_len + 3) & ~3
                offset += (nl_len + 3) & ~3


def probe(host: str, port: int) -> ProbeResult:
    """Measure the silence on the Protect event websocket. Blocking; sub-millisecond."""
    ports = _established_to(host, port)
    if not ports:
        return ProbeResult(None, 0, None, None)

    info = _tcp_info_by_port()
    socks = [(p, *info[p]) for p in ports if p in info]
    if not socks:
        return ProbeResult(None, len(ports), None, None)

    # The integration holds TWO websockets open: the private event stream, which
    # carries everything, and the public devices stream, a ~342 B/min NVR
    # heartbeat. Only the first one matters here, and it is the one with the
    # bytes — within a minute of startup it outweighs the other by orders of
    # magnitude. Do not key on the fd or the port: both are reassigned on every
    # reload, and the two streams have swapped fds between reloads in practice.
    local_port, last_recv_ms, rx_bytes, _segs_in = max(socks, key=lambda s: s[2])
    return ProbeResult(last_recv_ms / 1000, len(socks), local_port, rx_bytes)
