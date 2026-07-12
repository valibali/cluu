# syntax_highlight.py — highlights Rust keywords, strings, comments
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

KEYWORDS = {"fn", "let", "mut", "pub", "impl", "struct", "enum", "match",
            "if", "else", "for", "while", "use", "mod", "trait", "type",
            "const", "static", "return", "self", "Self", "as", "where"}

send({"type": "register", "keymap": {}, "commands": {}})

while True:
    line = sys.stdin.readline()
    if not line:
        break
    msg = json.loads(line)
    if msg.get("type") == "event":
        lines = rpc("buffer.read", {"start": 0, "end": 100})
        if lines:
            for i, l in enumerate(lines):
                words = l.replace("(", " ").replace(")", " ").replace("{", " ").replace("}", " ").split()
                for w in words:
                    clean = w.strip("();,")
                    if clean in KEYWORDS:
                        rpc("view.syntax", {"range": [i, 0, len(clean)], "style": "keyword"})
        send({"type": "done"})
