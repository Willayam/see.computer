"""Put Apple's keyboard and ours in the same state, screenshot both and compare them.

Called by keyboard-parity.sh. Taps land where Apple's keys are, read from the host's
accessibility tree, and the same points are replayed on ours, so both get the same input.
Held keys are captured with `simctl io screenshot`, because agent-device finishes a gesture
before it takes a screenshot.
"""
import json
import os
import plistlib
import subprocess
import sys
import threading
import time

AGENT = os.environ.get("AGENT_DEVICE", "")
UDID, OUT, COMPARE = sys.argv[1], sys.argv[2], sys.argv[3]
WANTED = sys.argv[4:]


def state(name, lang="sv", field="", taps=(), hold=None, delay=0, dark=False):
    return dict(name=name, lang=lang, field=field, taps=list(taps), hold=hold, delay=delay, dark=dark)


STATES = []
for lang, right in (("sv", "ä"), ("en", "l")):
    STATES += [
        state(f"{lang}-lower", lang, "cap=none"),
        state(f"{lang}-shift", lang),
        state(f"{lang}-caps", lang, "cap=none", taps=[("double", "shift")]),
        state(f"{lang}-numbers", lang, taps=[("tap", "more")]),
        state(f"{lang}-symbols", lang, taps=[("tap", "more"), ("tap", "shift")]),
        state(f"{lang}-hold-g", lang, "cap=none", hold="g"),
        state(f"{lang}-hold-a", lang, "cap=none", hold="a"),
        state(f"{lang}-hold-{right}", lang, "cap=none", hold=right),
        state(f"{lang}-hold-y", lang, "cap=none", hold="y"),
    ]
STATES += [
    state("sv-hold-space", hold="space"),
    state("sv-hold-shift", hold="shift"),
    state("sv-hold-delete", "sv", "text=Hej", hold="delete"),
    state("sv-hold-return", "sv", "text=Hej", hold="Return"),
    state("sv-hold-123", hold="more"),
]
for kind in ("default", "go", "search", "send", "done", "next", "join"):
    STATES.append(state(f"sv-return-{kind}", "sv", f"return={kind}&text=Hej"))
    STATES.append(state(f"sv-return-{kind}-off", "sv", f"return={kind}&enables=1"))
STATES += [
    state("sv-strip-three", "sv", taps=[("tap", "h"), ("tap", "e"), ("tap", "j")]),
    state("sv-strip-quoted", "sv", taps=[("tap", c) for c in "hejsn"]),
    state("sv-strip-few", "sv", taps=[("tap", c) for c in "xqzv"]),
    state("sv-strip-empty", "sv", "autocorrect=0"),
    state("en-type-email", "en", "type=email"),
    state("en-type-url", "en", "type=url"),
    state("en-type-websearch", "en", "type=websearch"),
    state("en-type-numberpad", "en", "type=numberpad"),
]
for base in ("sv-lower", "sv-shift", "sv-numbers", "sv-symbols", "sv-hold-g", "sv-hold-shift", "sv-return-send",
             "sv-return-send-off", "sv-strip-three", "en-lower"):
    s = dict(next(x for x in STATES if x["name"] == base))
    s.update(name="dark-" + base, dark=True)
    STATES.append(s)


if WANTED == ["list"]:
    print("\n".join(s["name"] for s in STATES))
    sys.exit()


def agent(*args):
    return subprocess.run([AGENT, *args], capture_output=True, text=True, stdin=subprocess.DEVNULL)


def container():
    return subprocess.run(["xcrun", "simctl", "get_app_container", UDID, "computer.see.ios", "data"],
                          capture_output=True, text=True, check=True).stdout.strip()


def group_prefs():
    root = os.path.join(os.path.dirname(container()), "..", "..", "Shared", "AppGroup")
    for name in os.listdir(root):
        meta = os.path.join(root, name, ".com.apple.mobile_container_manager.metadata.plist")
        if os.path.exists(meta) and plistlib.load(open(meta, "rb")).get("MCMMetadataIdentifier") == "group.computer.see.ios":
            return os.path.realpath(os.path.join(root, name, "Library", "Preferences", "group.computer.see.ios"))
    raise SystemExit("open the keyboard once so its App Group exists")


FIELD = os.path.join(container(), "Documents", "field.txt")
PREFS = group_prefs()
os.makedirs(os.path.dirname(FIELD), exist_ok=True)
counter = [0]


def show(kb, field):
    counter[0] += 1
    with open(FIELD, "w") as f:
        f.write(f"kb={kb}&n={counter[0]}&{field}")
    time.sleep(2.5)


def snapshot():
    out = agent("snapshot", "--json").stdout
    return json.loads(out)["data"]["nodes"]


def find(nodes, name):
    keys = [n for n in nodes if n.get("role") == "UIAccessibilityElementKBKey" and n["rect"]["width"] > 0]
    for n in keys:
        if n.get("identifier") == name:
            return n["rect"]
    for n in keys:
        if (n.get("label") or "").lower() == name.lower():
            return n["rect"]
    raise SystemExit(f"no key {name!r} on Apple's keyboard")


def center(rect):
    return round(rect["x"] + rect["width"] / 2, 1), round(rect["y"] + rect["height"] / 2, 1)


def act(points, hold, delay, path):
    for kind, (x, y) in points:
        if kind == "double":
            agent("press", str(x), str(y), "--double-tap")
        else:
            agent("press", str(x), str(y))
        time.sleep(0.6)
    if hold is None:
        time.sleep(0.4)
        agent("screenshot", path, "--pixel-density", "3")
        return
    # The moment a touch lands is not known here, so take a burst and keep the first frame in
    # which the key changed. Apple's accent menu replaces the preview about half a second later.
    x, y = hold
    before = path + ".before.png"
    subprocess.run(["xcrun", "simctl", "io", UDID, "screenshot", before], capture_output=True)
    press = threading.Thread(target=agent, args=("longpress", str(x), str(y), str(int(delay * 1000) + 3000)))
    press.start()
    shots = []
    started = time.time()
    while time.time() - started < 2.5:
        shot = f"{path}.{len(shots)}.png"
        subprocess.run(["xcrun", "simctl", "io", UDID, "screenshot", shot], capture_output=True)
        shots.append(shot)
        if subprocess.run([COMPARE, "changed", before, shot, str(x - 20), str(y - 80), "40", "100"]).returncode == 0:
            break
    if delay > 0.3:
        time.sleep(delay)
        subprocess.run(["xcrun", "simctl", "io", UDID, "screenshot", shot], capture_output=True)
    os.replace(shot, path)
    press.join()
    for leftover in shots[:-1] + [before]:
        if os.path.exists(leftover):
            os.remove(leftover)


def run(s):
    kb = "apple-sv" if s["lang"] == "sv" else "apple-en"
    show(kb, s["field"])
    nodes = snapshot()
    points = []
    for kind, name in s["taps"]:
        points.append((kind, center(find(nodes, name))))
        x, y = points[-1][1]
        if kind == "double":
            agent("press", str(x), str(y), "--double-tap")
        else:
            agent("press", str(x), str(y))
        time.sleep(0.6)
        nodes = snapshot()
    hold = center(find(nodes, s["hold"])) if s["hold"] else None
    with open(f"{OUT}/{s['name']}-apple.json", "w") as f:
        json.dump({"data": {"nodes": nodes}}, f)
    act([], hold, s["delay"], f"{OUT}/{s['name']}-apple.png")
    # The user types with both languages on, which gives the Swedish rows.
    subprocess.run(["xcrun", "simctl", "spawn", UDID, "defaults", "write", PREFS, "typingLanguages",
                    "bilingual" if s["lang"] == "sv" else "english"], check=True)
    show("ours", s["field"])
    act(points, hold, s["delay"], f"{OUT}/{s['name']}-ours.png")
    result = subprocess.run([COMPARE, f"{OUT}/{s['name']}-apple.png", f"{OUT}/{s['name']}-ours.png",
                             f"{OUT}/{s['name']}-apple.json", OUT, s["name"]], capture_output=True, text=True)
    print(result.stdout.strip() or result.stderr.strip(), flush=True)


chosen = [s for s in STATES if not WANTED or any(s["name"] == w or (w.endswith("*") and s["name"].startswith(w[:-1]))
                                                for w in WANTED)]
agent("open", "computer.see.ios", "--relaunch")
time.sleep(2)
# The first keyboard after a launch animates in late, so show each once before measuring.
show("ours", "")
show("apple-sv", "")
dark = None
for s in sorted(chosen, key=lambda s: s["dark"]):
    if s["dark"] != dark:
        dark = s["dark"]
        agent("settings", "appearance", "dark" if dark else "light")
        time.sleep(1.5)
    run(s)
if dark:
    agent("settings", "appearance", "light")
