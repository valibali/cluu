# test_plugin.py — registers Ctrl-B → insert "hello", :hello → view.status("world")
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

send({"type": "register", "keymap": {"Ctrl-B": "on_ctrl_b"}, "commands": {"hello": "on_hello"}})

while True:
    line = sys.stdin.readline()
    if not line:
        break
    msg = json.loads(line)
    if msg.get("type") == "event":
        cb = msg.get("callback_id", "")
        if cb == "on_ctrl_b":
            pos = rpc("cursor.pos")
            if pos:
                rpc("buffer.insert", {"pos": pos[1] if len(pos) >= 2 else 0, "text": "hello"})
            send({"type": "done"})
        elif cb == "on_hello":
            rpc("view.status", {"text": "world"})
            send({"type": "done"})
        else:
            send({"type": "done"})
