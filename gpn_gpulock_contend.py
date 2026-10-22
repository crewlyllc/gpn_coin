import sys, json, threading, time, urllib.request
sys.path.insert(0, "/home/jeremyg/.openclaw/workspace/projectmanager07")
from gpn_gpulock_live import chat, image, norm, AP

def infer(max_tokens=3000, prompt="Write an extremely long, detailed 1500 word story about a lighthouse keeper and every ship he saw."):
    t0 = time.time()
    req = urllib.request.Request(
        "http://127.0.0.1:8094/infer",
        data=json.dumps({"model": "llama3.2:1b", "prompt": prompt, "max_tokens": max_tokens}).encode(),
        headers={"Content-Type": "application/json"})
    r = json.load(urllib.request.urlopen(req, timeout=600))
    return round(time.time() - t0, 2), len(r.get("text") or ""), r.get("ok")

mode = sys.argv[1]

if mode == "A":
    print("=== A: chat (direct :8094/infer) in flight -> image via AP 1.5s later ===")
    out = {}
    def ct():
        out['chat'] = infer()
    t = threading.Thread(target=ct); t.start()
    time.sleep(1.0)
    t0 = time.time()
    res = image()
    print("image:", res, "wall", round(time.time() - t0, 2))
    t.join()
    print("chat :", out['chat'])

elif mode == "B":
    print("=== B: image via AP in flight -> chat (direct :8094/infer) 2s later ===")
    out = {}
    def it():
        out['image'] = image()
    t = threading.Thread(target=it); t.start()
    time.sleep(2.0)
    t0 = time.time()
    r = infer(48, "Say the single word: alpha")
    print("chat :", r, "wall", round(time.time() - t0, 2))
    t.join()
    print("image:", out['image'])
