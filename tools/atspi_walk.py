#!/usr/bin/env python3
"""Walk an app's full AT-SPI tree, using busctl as the transport.

GetChildren returns a(so) — (bus_name, object_path) pairs — so each child is
addressed with ITS OWN bus name. busctl prints these as a flat token stream:
  a(so) 2 ":1.18" "/org/.../accessible/1" ":1.18" "/org/.../accessible/2"
"""
import re, shlex, subprocess, sys

TARGET = sys.argv[1] if len(sys.argv) > 1 else "screenshot_probe"
ACC = "org.a11y.atspi.Accessible"
TEXT = "org.a11y.atspi.Text"

addr = None
out = subprocess.run(["dbus-send", "--session", "--dest=org.a11y.Bus", "--print-reply",
                      "/org/a11y/bus", "org.a11y.Bus.GetAddress"],
                     capture_output=True, text=True).stdout
for tok in out.split('"'):
    if tok.startswith("unix:path="):
        addr = tok
if not addr:
    raise SystemExit("no a11y bus")


def bc(*args):
    r = subprocess.run(["busctl", f"--address={addr}", "--no-pager", *args],
                       capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else None


def call(dest, path, iface, method, *args):
    return bc("call", dest, path, iface, method, *args)


def prop(dest, path, iface, name):
    v = bc("get-property", dest, path, iface, name)
    if not v:
        return None
    parts = shlex.split(v)
    return parts[-1] if parts else None


def children(dest, path):
    v = call(dest, path, ACC, "GetChildren")
    if not v:
        return []
    toks = shlex.split(v)          # ['a(so)', '2', ':1.18', '/path', ...]
    toks = toks[2:] if len(toks) >= 2 else []
    return list(zip(toks[0::2], toks[1::2]))


def role(dest, path):
    v = call(dest, path, ACC, "GetRoleName")
    if not v:
        return "?"
    p = shlex.split(v)
    return p[-1] if p else "?"


def text_of(dest, path):
    n = prop(dest, path, TEXT, "CharacterCount")
    try:
        n = int(n)
    except (TypeError, ValueError):
        return None
    if n <= 0:
        return None
    v = call(dest, path, TEXT, "GetText", "i", "0", "i", str(min(n, 200)))
    if not v:
        return None
    p = shlex.split(v)
    return p[-1] if p else None


roles, texts, seen = {}, [], set()
total = 0


def walk(dest, path, depth=0):
    global total
    key = (dest, path)
    if key in seen or total > 3000:
        return
    seen.add(key)
    total += 1
    r = role(dest, path)
    roles[r] = roles.get(r, 0) + 1
    n = prop(dest, path, ACC, "Name") or ""
    t = text_of(dest, path)
    if t and t.strip():
        texts.append(t.strip())
    if depth <= 5:
        extra = f" text={t[:60]!r}" if t and t.strip() else ""
        nm = f" name={n!r}" if n else ""
        print(f"{'  ' * depth}- {r}{nm}{extra}")
    for cd, cp in children(dest, path):
        walk(cd, cp, depth + 1)


root_kids = children("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root")
target = None
for d, p in root_kids:
    if (prop(d, p, ACC, "Name") or "") == TARGET:
        target = (d, p)
if not target:
    print("NOT REGISTERED; apps:", [prop(d, p, ACC, "Name") for d, p in root_kids])
    raise SystemExit(1)

print(f"=== full AT-SPI tree for {TARGET!r} ===")
walk(*target)
print("\n=== SUMMARY ===")
print("nodes visited :", total)
print("roles         :", dict(sorted(roles.items(), key=lambda kv: -kv[1])))
print("text nodes    :", len(texts))
for t in texts[:10]:
    print("   ", repr(t[:90]))
