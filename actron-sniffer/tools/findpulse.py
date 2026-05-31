#!/usr/bin/env python3
"""Find the command pulse for an Actron control change.

The indoor board owns authoritative state and ignores *reported values* from a wall controller;
it acts on a transient **command pulse**. A setpoint change, for example, is: new value written to
reg 12/56 (a STEP that holds) + reg 14 pulsed to 4 for one cycle (a PULSE that returns to idle).

This decodes the **0x66 controller responses** (not the broadcast) frame-by-frame and classifies
every changing register:
  PULSE  - deviates then returns to its starting value  -> the command/event (e.g. reg14=4)
  STEP   - changes once (or twice) and holds            -> the value/state that changed (e.g. reg12)
  DRIFT  - changes almost every frame                   -> a counter / heartbeat / live sensor (noise)

Workflow per control:
  curl .../clear            # clear the ring
  <make ONE change on the NEO>
  findpulse.py              # pulls /log, prints PULSE + STEP registers

Usage:
  findpulse.py [--ip IP]    # pull live
  findpulse.py FILE         # a saved /log capture
"""
import sys, re, argparse, urllib.request

DEVICE = "REDACTED-IP"


def modbus_crc(d: bytes) -> int:
    c = 0xFFFF
    for b in d:
        c ^= b
        for _ in range(8):
            c = (c >> 1) ^ 0xA001 if c & 1 else c >> 1
    return c


def fetch(ip, path):
    return urllib.request.urlopen(f"http://{ip}{path}", timeout=8).read().decode("utf-8", "replace")


def responses(text):
    """-> (page1_list, page2_list); each a list of {reg: value(BE)} in capture order.

    Handles both log formats: the old single-bus format (lines start with seq), and the new
    MITM dual-bus format (lines start with `A ` or `B ` source tag). In MITM mode the NEO's
    responses appear on the B side; the A side carries the board's polls/broadcasts plus
    echoes of our forwarded frames — we filter A out so we only analyse fresh NEO data."""
    p1, p2 = [], []
    for ln in text.splitlines():
        m = re.match(r"(?:([AB])\s+)?\d+\s+[\d.]+\s+\+\d+us\s+(\d+):\s+([0-9A-Fa-f ]+?)\s+\|", ln)
        if not m:
            continue
        side = m.group(1) or ""  # "" = old format, "A"/"B" = new MITM format
        if side == "A":
            continue
        b = bytes(int(x, 16) for x in m.group(3).split())
        if b[:1] != b"\x66" or len(b) < 5 or b[1] != 0x03:
            continue
        if modbus_crc(b[:-2]) != (b[-2] | b[-1] << 8):
            continue
        if b[2] == 0xF8 and len(b) == 253:
            data = b[3:3 + 248]
            p1.append({2 + i: (data[2 * i] << 8) | data[2 * i + 1] for i in range(124)})
        elif b[2] == 0xF4 and len(b) == 249:
            data = b[3:3 + 244]
            p2.append({126 + i: (data[2 * i] << 8) | data[2 * i + 1] for i in range(122)})
    return p1, p2


def classify(vals):
    distinct = set(vals)
    if len(distinct) == 1:
        return None
    transitions = sum(1 for i in range(1, len(vals)) if vals[i] != vals[i - 1])
    if vals[0] == vals[-1]:
        return ("PULSE", vals)
    if transitions <= 2:
        return ("STEP", vals)
    if transitions >= max(3, len(vals) * 0.5):
        return ("DRIFT", vals)
    return ("STEP?", vals)


def seq_str(vals):
    out = []
    for v in vals:
        if not out or out[-1] != v:
            out.append(v)
    s = " ".join(str(v) for v in out[:14])
    return s + (" ..." if len(out) > 14 else "")


def analyse(p1, p2):
    for label, frames in (("PAGE1 (regs 2-125)", p1), ("PAGE2 (regs 126-247)", p2)):
        if len(frames) < 2:
            print(f"# {label}: only {len(frames)} response(s) — need >=2 (capture longer)")
            continue
        print(f"# {label}: {len(frames)} responses")
        pulses, steps = [], []
        for r in sorted(frames[0]):
            vals = [f[r] for f in frames if r in f]
            c = classify(vals)
            if not c:
                continue
            kind, v = c
            if kind == "PULSE":
                pulses.append((r, v))
            elif kind.startswith("STEP"):
                steps.append((r, v))
        if pulses:
            print("  >>> PULSE (command/event):")
            for r, v in pulses:
                print(f"      reg {r:3d} (0x{r:02X}): {seq_str(v)}")
        if steps:
            print("  >>> STEP (value/state that changed):")
            for r, v in steps:
                tv = lambda x: f"{x/10:.1f}C" if 150 <= x <= 320 else str(x)
                print(f"      reg {r:3d} (0x{r:02X}): {tv(v[0])} -> {tv(v[-1])}")
        if not pulses and not steps:
            print("  (no clear pulse/step — only drift/counters; try recapturing around the change)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("file", nargs="?")
    ap.add_argument("--ip", default=DEVICE)
    a = ap.parse_args()
    text = open(a.file).read() if a.file else fetch(a.ip, "/log?n=400")
    analyse(*responses(text))


if __name__ == "__main__":
    main()
