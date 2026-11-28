"""Writes the shared files as an earlier keyboard left them, with the app not running.
usage: left-behind.py <group folder> <typed|waiting|unanswered>

typed, waiting  a build before the ledger: one finished take in state.json, one draft, and
                one bookmark, at the take or before it
unanswered      this build, killed after it sent a take and before the host answered"""
import base64, hashlib, json, os, sys, time, unicodedata, uuid

group, case = sys.argv[1], sys.argv[2]


def fingerprint(text):
    return base64.b64encode(hashlib.sha256(unicodedata.normalize("NFC", text).encode()).digest()).decode()


def write(name, value):
    json.dump(value, open(os.path.join(group, name), "w"))


now = time.time() - 978307200
take_id = int(time.time() * 1000)
start = {
    "id": str(uuid.uuid4()).upper(),
    "asked": now - 20,
    "target": {"field": str(uuid.uuid4()).upper(), "before": fingerprint("Alpha."), "hasText": True},
}
for name in ("ledger.json", "draft.json", "manual-edit.json", "inserted.json"):
    try:
        os.remove(os.path.join(group, name))
    except FileNotFoundError:
        pass
if case == "unanswered":
    take = {"id": take_id, "text": "Sent just before the keyboard went away.", "finished": now, "start": start}
    write("state.json", {"phase": "armed", "takes": [take], "model": "ready"})
    sent = {"id": start["id"], "touched": now, "standing": "sent", "field": start["target"]["field"]}
    write("ledger.json", {"entries": [sent]})
else:
    take = {"id": take_id, "text": "Legacy words from before the update, now complete.", "finished": now, "start": start}
    write("state.json", {"phase": "armed", "since": now - 20, "start": start, "take": take})
    write("draft.json", {"spacing": " ", "words": "Legacy words from before the update", "start": start})
    write("inserted.json", take_id if case == "typed" else take_id - 1000)
