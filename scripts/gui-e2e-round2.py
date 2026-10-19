# Round-2 deep verification via `pilo-server call`:
#  1. full agent loop (prompt -> real assistant text in events/transcript)
#  2. R6 session.activity live vs after stop
#  3. shim --session passthrough lands on real pi
#  4. deployed runtime binary exposes `call`
import json, os, subprocess, sys, threading, time

SERVER = r"D:\pilo-src\target\debug\pilo-server.exe"
DEPLOYED = r"D:\software\pilo\runtime\pilo-server-windows-x86_64.exe"
PROJECT = r"D:\pi-bidding-agent\.worktrees\pi-1.3-native-tui\ad-hoc\pilo-e2e"
SHIM_JS = r"D:\pi-bidding-agent\.worktrees\pi-1.3-native-tui\scripts\pilo-shim\pi.js"
SHIM_CMD = r"D:\pi-bidding-agent\.worktrees\pi-1.3-native-tui\scripts\pilo-shim\pi.cmd"
STREAM = "r2"

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

def payloads():
    out = []
    for line in events:
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        for text in ev.get("binaryText", []):
            try:
                out.append(json.loads(text))
            except json.JSONDecodeError:
                pass
    return out

def wait_for(pred, timeout=120):
    deadline = time.time() + timeout
    while time.time() < deadline:
        hit = pred(payloads())
        if hit:
            return hit
        time.sleep(0.5)
    return None

print("== 4. deployed binary sanity ==")
r = subprocess.run([DEPLOYED, "call", "server.ping", "{}"], capture_output=True, text=True, timeout=30)
print("deployed call ping rc:", r.returncode, r.stdout[:80].strip() or r.stderr[:120].strip())

print("== 1. pi.start -> prompt -> full agent loop ==")
r = call("pi.start", {"streamId": STREAM, "project": PROJECT, "piExecutable": SHIM_CMD})
assert r["ok"], r
print("started")

print("== 2. R6: session.activity while locked ==")
r = call("session.activity", {"project": PROJECT})
for e in r.get("result", []):
    print("   activity:", json.dumps(e, ensure_ascii=False)[:140])

call("pi.send", {"streamId": STREAM, "command": {"id": "p1", "type": "prompt", "message": "Reply with exactly one word: ok"}})
ack = wait_for(lambda ps: next((p for p in ps if p.get("id") == "p1"), None), 30)
print("prompt ack:", json.dumps(ack or "TIMEOUT")[:120])

end = wait_for(lambda ps: next((p for p in ps if p.get("type") == "agent_end"), None), 120)
print("agent_end seen:", bool(end))
texts = [p for p in payloads() if p.get("type") in ("message_end", "message_update")
         and p.get("message", {}).get("role") == "assistant"]
joined = " ".join(json.dumps(t.get("message", {}).get("content", "")) for t in texts)
print("assistant text contains 'ok':", "ok" in joined.lower(), "| assistant msgs:", len(texts))

print("== stop + post-stop activity ==")
call("pi.stop", {"streamId": STREAM})
time.sleep(2)
r = call("session.activity", {"project": PROJECT})
for e in r.get("result", []):
    print("   activity:", json.dumps(e, ensure_ascii=False)[:140])

print("== 3. shim --session passthrough ==")
fake = os.path.expanduser(r"~\.pi\agent\sessions\fake\fake.jsonl")
r = subprocess.run(
    ["node", SHIM_JS, "--mode", "rpc", "--session", fake],
    cwd=PROJECT, capture_output=True, text=True, timeout=15,
    input='{"id":"x","type":"get_state"}\n')
pi_reply = [l for l in r.stdout.splitlines() if '"response"' in l]
print("stderr:", r.stderr.strip()[:120] or "(empty)")
print("rpc replies:", len(pi_reply))
if pi_reply:
    d = json.loads(pi_reply[0])
    name = d.get("data", {}).get("sessionName")
    print("sessionName:", name, "| real pi (no [purpose] prefix):", not (name or "").startswith("["))

proc.stdin.close(); time.sleep(1); proc.terminate()
print("DONE")
