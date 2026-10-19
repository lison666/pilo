# Automated replay of the Pilo GUI flow through `pilo-server call` (R9).
# Same dispatch layer the Tauri frontend uses. Usage: python gui-e2e.py
import json, queue, subprocess, sys, threading, time

SERVER = r"D:\pilo-src\target\debug\pilo-server.exe"
PROJECT = r"D:\pi-bidding-agent\.worktrees\pi-1.3-native-tui\ad-hoc\pilo-e2e"
SHIM = r"D:\pi-bidding-agent\.worktrees\pi-1.3-native-tui\scripts\pilo-shim\pi.cmd"
STREAM = "gui-e2e"

proc = subprocess.Popen([SERVER, "call"], stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
events = []
threading.Thread(target=lambda: [events.append(l) for l in proc.stderr], daemon=True).start()

def call(method, params=None):
    req = {"method": method}
    if params is not None:
        req["params"] = params
    proc.stdin.write(json.dumps(req) + "\n"); proc.stdin.flush()
    return json.loads(proc.stdout.readline())

def rpc_reply(cmd_id):
    """Find a pi.rpc_json event whose decoded payload is the reply to cmd_id."""
    for line in events:
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("event") != "pi.rpc_json":
            continue
        for text in ev.get("binaryText", []):
            try:
                payload = json.loads(text)
            except json.JSONDecodeError:
                continue
            if payload.get("id") == cmd_id:
                return payload
    return None

def wait_reply(cmd_id, timeout=90):
    deadline = time.time() + timeout
    while time.time() < deadline:
        payload = rpc_reply(cmd_id)
        if payload is not None:
            return payload
        time.sleep(0.3)
    return None

def show(r): print(json.dumps(r, ensure_ascii=False)[:220])

# 1. ping
r = call("server.ping"); show(r); assert r["ok"]

# 2. pi.start — real session like GUI open (no noSession flag)
r = call("pi.start", {"streamId": STREAM, "project": PROJECT, "piExecutable": SHIM})
show(r); assert r["ok"] and r["result"]["started"]

# 3. get_state
call("pi.send", {"streamId": STREAM, "command": {"id": "s1", "type": "get_state"}})
r = wait_reply("s1")
assert r, "no get_state reply"
print("get_state ok:", r.get("success"), "| session:", r.get("data", {}).get("sessionName"))

# 4. real prompt (needs the model proxy; tolerate failure)
call("pi.send", {"streamId": STREAM, "command": {"id": "p1", "type": "prompt", "message": "reply with just: ok"}})
r = wait_reply("p1", timeout=120)
print("prompt reply:", json.dumps(r or "TIMEOUT", ensure_ascii=False)[:200])

# 5. session.scan -> find the live transcript path
r = call("session.scan", {"project": PROJECT})
entries = r.get("result", [])
print("scan entries:", len(entries))
for e in entries:
    hdr = e.get("header", {})
    print("  ", e.get("path", "?")[:110], "|", hdr.get("cwd", "")[-40:])
path = None
for e in entries:
    p = e.get("path", "")
    if p.endswith(".jsonl") and e.get("header", {}).get("cwd") == PROJECT:
        path = p  # last match wins — entries come mtime-sorted
print("transcript:", path)

# 6. delete while locked -> must FAIL (R4 guard)
if path:
    r = call("session.delete", {"path": path})
    show(r)
    assert not r["ok"], "delete succeeded while lock held!"
    print("R4 refusal:", r.get("error", "")[:160])

# 7. stop
r = call("pi.stop", {"streamId": STREAM}); show(r)

# 8. delete after unlock -> must SUCCEED
if path:
    time.sleep(2)
    r = call("session.delete", {"path": path})
    show(r)
    print("post-stop delete ok:", r.get("ok"))

proc.stdin.close(); time.sleep(1); proc.terminate()
print("DONE")
