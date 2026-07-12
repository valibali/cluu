# auto_indent.py — on Enter, copy leading whitespace from current line
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

send({"type": "register", "keymap": {"Enter": "on_enter"}, "commands": {}})

while True:
    line = sys.stdin.readline()
    if not line:
        break
    msg = json.loads(line)
    if msg.get("type") == "event":
        cb = msg.get("callback_id", "")
        if cb == "on_enter":
            pos = rpc("cursor.pos")
            if pos and len(pos) >= 2:
                row = pos[0]
                lines = rpc("buffer.read", {"start": row, "end": row + 1})
                if lines:
                    indent = ""
                    for ch in lines[0]:
                        if ch in (" ", "\t"):
                            indent += ch
                        else:
                            break
                    if indent:
                        col = pos[1]
                        rpc("buffer.insert", {"pos": col, "text": "\n" + indent})
                    else:
                        rpc("buffer.insert", {"pos": pos[1], "text": "\n"})
            send({"type": "done"})
        else:
            send({"type": "done"})
