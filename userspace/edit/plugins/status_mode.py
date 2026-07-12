# status_mode.py — on mode change, update status bar
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def rpc(method, params=None):
    rid = rpc._id
    rpc._id += 1
    send({"type": "call", "method": method, "params": params or {}, "id": rid})
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        msg = json.loads(line)
        if msg.get("type") == "result" and msg.get("id") == rid:
            return msg.get("result")
rpc._id = 1

send({"type": "register", "keymap": {}, "commands": {}})

while True:
    line = sys.stdin.readline()
    if not line:
        break
    msg = json.loads(line)
    if msg.get("type") == "event":
        ev = msg.get("event", "")
        if ev == "mode":
            mode = msg.get("mode", "NORMAL")
            rpc("view.status", {"text": mode.upper()})
        send({"type": "done"})
