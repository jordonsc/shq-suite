#!/usr/bin/env python3
"""
Actron NEO local-control integration test suite.

Runs every recipe from LOCAL-CONTROL-RECIPES.md against the live in-wall MITM bridge and
verifies each commit by polling the board's broadcast. Sequenced so the operator can watch
the NEO display update live between steps.

Usage:
    python3 tools/integration_test.py [--device REDACTED-IP]

Each step:
1. Sets the inject/pulse rule(s)
2. Polls the board's broadcast for up to 60s for the expected value
3. Reports PASS / FAIL with timing
4. Moves on regardless (so a single failure doesn't abort the whole run)

At the end: restores the bridge to passthru and prints a summary table.
"""

import argparse
import re
import sys
import time
import urllib.request

DEFAULT_HOST = "REDACTED-IP"
STEP_TIMEOUT_S = 60     # max wait per step
POLL_INTERVAL_S = 3     # how often we re-poll the board broadcast


# ---- HTTP helpers --------------------------------------------------------

def http_get(host: str, path: str, timeout: float = 10.0) -> str:
    return urllib.request.urlopen(f"http://{host}{path}", timeout=timeout).read().decode()


def set_bridge_mode(host: str, mode: str) -> None:
    http_get(host, f"/bridge?mode={mode}")


def set_inject_rules(host: str, rules: str) -> None:
    """Persistent rules (cleared by passing empty string)."""
    http_get(host, f"/inject?rules={rules}")


def fire_pulse(host: str, rules: str, n: int = 2) -> None:
    """One-shot rules that expire after `n` recognised B frames."""
    http_get(host, f"/pulse?rules={rules}&n={n}")


# ---- log parsing ---------------------------------------------------------

def fetch_log(host: str) -> str:
    return http_get(host, "/log?since=0&n=200")


def latest_board_broadcast(host: str, page: str) -> list[int] | None:
    """Return the data bytes of the most recent board broadcast for page 1 or 2.

    Page-1 broadcast header: `00 10 00 04 00 7A F4` (7 bytes); data area = next 244 bytes.
    Page-2 broadcast header: `00 10 00 7E 00 7B F6` (7 bytes); data area = next 246 bytes.
    Returns None if no matching frame in the ring.
    """
    log = fetch_log(host)
    header = "00 10 00 04 00 7A F4" if page == "p1" else "00 10 00 7E 00 7B F6"
    expected_n = 244 if page == "p1" else 246
    candidates = []
    for line in log.split("\n"):
        m = re.match(rf"^A \d+ [\d.]+ \+\d+us \d+: {header}((?: [0-9A-F]{{2}}){{{expected_n}}})", line)
        if m:
            candidates.append([int(b, 16) for b in m.group(1).split()])
    return candidates[-1] if candidates else None


def read_register(host: str, reg: int, page: str = "p2") -> int | None:
    """Decode register N from the latest board broadcast for the given page."""
    data = latest_board_broadcast(host, page)
    if data is None:
        return None
    start_addr = 4 if page == "p1" else 126   # broadcast page-1 starts at reg 4
    offset = (reg - start_addr) * 2
    if offset < 0 or offset + 1 >= len(data):
        return None
    return (data[offset] << 8) | data[offset + 1]


# ---- test step definition + runner --------------------------------------

class Step:
    """One scenario test: apply rules, then poll for the expected value."""

    def __init__(self, name: str, *, recipe: str, kind: str, verify_reg: int,
                 expected: int, page: str = "p2", n: int = 2,
                 mask: int | None = None):
        self.name = name
        self.recipe = recipe         # rules string, e.g. "10:0x8081,14:1"
        self.kind = kind             # "pulse" | "inject"
        self.verify_reg = verify_reg
        self.expected = expected
        self.page = page
        self.n = n                   # pulse frame count
        self.mask = mask             # optional bitmask for partial-match checks
        self.elapsed: float = 0.0
        self.result: str = "—"

    def matches(self, value: int) -> bool:
        if self.mask is None:
            return value == self.expected
        return (value & self.mask) == (self.expected & self.mask)

    def run(self, host: str) -> bool:
        print(f"\n[{self.name}]")
        print(f"  recipe: {self.kind} rules={self.recipe} (verify reg {self.verify_reg} = 0x{self.expected:04X})")
        if self.kind == "pulse":
            fire_pulse(host, self.recipe, n=self.n)
        elif self.kind == "inject":
            set_inject_rules(host, self.recipe)
        else:
            raise ValueError(f"unknown kind: {self.kind}")

        t0 = time.time()
        deadline = t0 + STEP_TIMEOUT_S
        last_seen = None
        while time.time() < deadline:
            time.sleep(POLL_INTERVAL_S)
            v = read_register(host, self.verify_reg, self.page)
            if v is not None and v != last_seen:
                last_seen = v
                masked = (v & self.mask) if self.mask is not None else v
                print(f"  t+{time.time() - t0:.0f}s  reg {self.verify_reg} = 0x{v:04X}  (masked 0x{masked:04X})")
            if v is not None and self.matches(v):
                self.elapsed = time.time() - t0
                self.result = f"PASS in {self.elapsed:.0f}s"
                # For inject (persistent), clear rules now that we've validated
                if self.kind == "inject":
                    set_inject_rules(host, "")
                return True
        self.elapsed = STEP_TIMEOUT_S
        self.result = f"FAIL (timeout, last value 0x{last_seen:04X})" if last_seen is not None else "FAIL (no data)"
        # Always clear inject rules on failure too
        if self.kind == "inject":
            set_inject_rules(host, "")
        return False


def make_suite() -> list[Step]:
    """Sequence of test steps. State flows through them — each step assumes the previous
    one committed (or at least left the system in a state where the next step still makes
    sense). Designed for live observation on the NEO display."""
    return [
        # ------- Master mode and on/off (recipe §1) -------
        Step("Mode → COOL (turn on)",
             recipe="10:0x8081,14:1", kind="pulse",
             verify_reg=10, expected=0x8081, page="p1", mask=0x000F),

        # ------- Fan speed (recipe §2) — UNVALIDATED PATH, expected to pass -------
        Step("Fan → med",
             recipe="11:0x5902,14:2", kind="pulse",
             verify_reg=11, expected=0x0002, page="p1", mask=0x000F),

        # ------- Master setpoint (recipe §3) — UNVALIDATED via MITM, expected to pass -------
        Step("Main setpoint → 23.0",
             recipe="12:0x00E6,55:0x00E6,14:4", kind="pulse",
             verify_reg=12, expected=0x00E6, page="p1"),

        # ------- Zone enable (recipe §5) -------
        Step("Zones → Living+Entry+Gym (0x000B)",
             recipe="85:0x000B,14:0x40", kind="pulse",
             verify_reg=85, expected=0x000B, page="p1"),

        # ------- Cool-array setpoints (recipe §4) -------
        Step("Living COOL → 22.0",
             recipe="127:0x00DC,126:0x0001", kind="inject",
             verify_reg=127, expected=0x00DC, page="p2"),

        Step("Entry COOL → 23.0",
             recipe="128:0x00E6,126:0x0001", kind="inject",
             verify_reg=128, expected=0x00E6, page="p2"),

        Step("Gym COOL → 21.5",
             recipe="130:0x00D7,126:0x0001", kind="inject",
             verify_reg=130, expected=0x00D7, page="p2"),

        # ------- Switch to HEAT mode -------
        Step("Mode → HEAT",
             recipe="10:0x8082,14:1", kind="pulse",
             verify_reg=10, expected=0x8082, page="p1", mask=0x000F),

        # ------- Heat-array setpoints (recipe §4 with reg 126 = 0x0002) -------
        Step("Living HEAT → 21.0",
             recipe="135:0x00D2,126:0x0002", kind="inject",
             verify_reg=135, expected=0x00D2, page="p2"),

        Step("Gym HEAT → 22.0",
             recipe="138:0x00DC,126:0x0002", kind="inject",
             verify_reg=138, expected=0x00DC, page="p2"),

        # ------- Switch to AUTO mode and exercise both arrays -------
        Step("Mode → AUTO",
             recipe="10:0x8084,14:1", kind="pulse",
             verify_reg=10, expected=0x8084, page="p1", mask=0x000F),

        Step("(Auto) Gym COOL → 22.0",
             recipe="130:0x00DC,126:0x0001", kind="inject",
             verify_reg=130, expected=0x00DC, page="p2"),

        Step("(Auto) Gym HEAT → 23.0",
             recipe="138:0x00E6,126:0x0002", kind="inject",
             verify_reg=138, expected=0x00E6, page="p2"),

        # ------- Master OFF (clean shutdown) -------
        Step("Mode → OFF",
             recipe="10:0x8080,14:1", kind="pulse",
             verify_reg=10, expected=0x8080, page="p1", mask=0x000F),
    ]


# ---- main ---------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--device", default=DEFAULT_HOST, help="bridge IP/host (default: %(default)s)")
    args = ap.parse_args()
    host = args.device

    print(f"Actron NEO integration test suite vs {host}")
    print(f"Step timeout: {STEP_TIMEOUT_S}s   Poll interval: {POLL_INTERVAL_S}s")

    # Engage inject mode; ensure no stale rules from a previous run
    set_bridge_mode(host, "inject")
    set_inject_rules(host, "")

    suite = make_suite()
    t_start = time.time()
    passed = 0
    for step in suite:
        ok = step.run(host)
        if ok:
            passed += 1

    # Restore passthru regardless of outcome
    set_inject_rules(host, "")
    set_bridge_mode(host, "passthru")
    total = time.time() - t_start

    print(f"\n\n=== Summary ({passed}/{len(suite)} passed, {total:.0f}s) ===")
    for s in suite:
        marker = "✓" if s.result.startswith("PASS") else "✗"
        print(f"  {marker}  {s.name:42s}  {s.result}")

    sys.exit(0 if passed == len(suite) else 1)


if __name__ == "__main__":
    main()
