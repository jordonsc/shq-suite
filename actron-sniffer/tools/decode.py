#!/usr/bin/env python3
"""Actron NEO RS485 (Modbus RTU) decode tool — the workhorse for the mapping exercise.

Context: the bus is Modbus RTU @ 9600/8N1. The indoor board (master) reads controllers
(func 03) at 0x66 (NEO) / 0x67 / 0x68 (empty slots) and broadcasts the consolidated state
(func 0x10 write-multiple to addr 0x00). The func-0x10 broadcasts are self-describing
(start reg + qty in the header), so we decode *those* into a register map. Register values
are 16-bit BIG-ENDIAN (standard Modbus, high byte first); temperatures are 0.1 deg C absolute
(e.g. 0x00EB = 235 = 23.5C). NB the CRC trailer is the Modbus exception: appended low byte
first. See ../FINDINGS.md + ../MAPPING-PLAN.md.

Device HTTP API: GET /log?n=<N> returns frame lines `<seq> <t_s> +<gap>us <len>: HEX  |ascii|`.

Usage:
  decode.py snapshot [--ip IP] [--save FILE]   # pull live, print the register map
  decode.py regs FILE                          # decode a saved /log capture
  decode.py diff BEFORE.log AFTER.log          # registers that changed (stable in both, differ)
  decode.py crc "66 03 00 02 00 7C"            # compute/verify Modbus CRC-16
"""
import sys, re, argparse, collections, urllib.request

DEVICE = "REDACTED-IP"


def modbus_crc(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc  # appears on the wire low-byte-first


def fetch(ip: str, path: str) -> str:
    return urllib.request.urlopen(f"http://{ip}{path}", timeout=8).read().decode("utf-8", "replace")


def parse_log(text: str):
    """-> list of (gap_us, data_bytes)."""
    out = []
    for ln in text.splitlines():
        if not ln or ln.startswith("#"):
            continue
        m = re.match(r"\d+\s+[\d.]+\s+\+(\d+)us\s+\d+:\s+([0-9A-Fa-f ]+?)\s+\|", ln)
        if not m:
            continue
        out.append((int(m.group(1)), bytes(int(x, 16) for x in m.group(2).split())))
    return out


def rejoin(frames, gap_us=2000):
    """Rejoin sub-2ms continuation splits (harmless now FRAME_MAX=512 captures whole msgs)."""
    msgs, cur = [], []
    for gap, d in frames:
        if cur and gap > gap_us:
            msgs.append(b"".join(cur))
            cur = []
        cur.append(d)
    if cur:
        msgs.append(b"".join(cur))
    return msgs


def registers_from_writes(msgs):
    """Decode func-0x10 broadcast writes (addr 0x00) into {reg: 16-bit LE value}.

    Frame: addr(1) 0x10 start(2,BE) qty(2,BE) bytecount(1) data(2*qty) crc(2).
    """
    regs = {}
    for d in msgs:
        if len(d) < 9 or d[1] != 0x10:
            continue
        if modbus_crc(d[:-2]) != (d[-2] | d[-1] << 8):
            continue  # bad CRC -> skip (mis-framed)
        start = (d[2] << 8) | d[3]
        qty = (d[4] << 8) | d[5]
        data = d[7:7 + 2 * qty]
        for i in range(min(qty, len(data) // 2)):
            regs[start + i] = (data[2 * i] << 8) | data[2 * i + 1]  # big-endian (standard Modbus)
    return regs


def fmt_reg(reg, val):
    extra = f"  = {val / 10:.1f}C" if 150 <= val <= 300 else ""
    return f"  reg {reg:3d} (0x{reg:02X}): {val:5d}  0x{val:04X}{extra}"


def get_regs(source, ip):
    text = fetch(ip, "/log?n=400") if source == "live" else open(source).read()
    return registers_from_writes(rejoin(parse_log(text)))


def cmd_snapshot(args):
    text = fetch(args.ip, "/log?n=400")
    if args.save:
        open(args.save, "w").write(text)
        print(f"# saved raw capture -> {args.save}")
    regs = registers_from_writes(rejoin(parse_log(text)))
    print(f"# {len(regs)} registers (from func-0x10 broadcasts), LE; temps flagged")
    for r in sorted(regs):
        print(fmt_reg(r, regs[r]))


def cmd_regs(args):
    regs = get_regs(args.file, args.ip)
    for r in sorted(regs):
        print(fmt_reg(r, regs[r]))


def cmd_diff(args):
    a, b = get_regs(args.before, args.ip), get_regs(args.after, args.ip)
    common = sorted(set(a) & set(b))
    changed = [(r, a[r], b[r]) for r in common if a[r] != b[r]]
    print(f"# {len(common)} common regs, {len(changed)} changed")
    for r, av, bv in changed:
        ta = f"{av/10:.1f}" if 150 <= av <= 300 else str(av)
        tb = f"{bv/10:.1f}" if 150 <= bv <= 300 else str(bv)
        print(f"  reg {r:3d} (0x{r:02X}): {av} -> {bv}   ({ta} -> {tb})")


def cmd_crc(args):
    d = bytes(int(x, 16) for x in args.frame.replace(",", " ").split())
    crc = modbus_crc(d if len(d) < 2 else d[:-2]) if len(d) >= 2 else modbus_crc(d)
    full = modbus_crc(d)
    print(f"CRC of all bytes      = 0x{full:04X} (wire: {full & 0xFF:02X} {full >> 8:02X})")
    if len(d) >= 3:
        body = modbus_crc(d[:-2])
        rx = d[-2] | (d[-1] << 8)
        print(f"CRC of body (ex last2)= 0x{body:04X}  vs trailing 0x{rx:04X}  {'MATCH' if body == rx else 'no'}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--ip", default=DEVICE)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("snapshot"); s.add_argument("--save"); s.set_defaults(fn=cmd_snapshot)
    s = sub.add_parser("regs"); s.add_argument("file"); s.set_defaults(fn=cmd_regs)
    s = sub.add_parser("diff"); s.add_argument("before"); s.add_argument("after"); s.set_defaults(fn=cmd_diff)
    s = sub.add_parser("crc"); s.add_argument("frame"); s.set_defaults(fn=cmd_crc)
    args = p.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
