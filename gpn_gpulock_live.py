import json, time, urllib.request, threading

AP = "http://127.0.0.1:8092"

def post(path, obj, timeout=300):
    req = urllib.request.Request(AP + path, data=json.dumps(obj).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))

import urllib.parse

def norm(u):
    """poll_url may be absolute (public host) — keep only the path."""
    if u.startswith("http://") or u.startswith("https://"):
        pr = urllib.parse.urlsplit(u)
        u = pr.path + (("?" + pr.query) if pr.query else "")
    # The public URL is /api/v1/...; the local AP serves /v1/...
    if u.startswith("/api/"):
        u = u[len("/api"):]
    return u

def get(path, timeout=300):
    return json.load(urllib.request.urlopen(AP + norm(path), timeout=timeout))

def chat(max_tokens, prompt="Write a detailed 300 word story about a lighthouse."):
    t0 = time.time()
    r = post("/v1/chat/completions", {"model": "llama3.2:1b",
             "messages": [{"role": "user", "content": prompt}],
             "max_tokens": max_tokens})
    dt = time.time() - t0
    txt = r["choices"][0]["message"]["content"]
    return dt, len(txt), txt[:40]

def image(prompt="a red bicycle leaning on a brick wall, photorealistic"):
    t0 = time.time()
    r = post("/v1/images/generations", {"prompt": prompt, "width": 512, "height": 512,
                                        "steps": 20, "sync": False})
    jid = r.get("id") or r.get("job_id")
    url = r.get("poll_url") or f"/v1/images/generations/{jid}"
    while True:
        s = get(url)
        st = s.get("status")
        if st and st not in ("queued", "running", "pending"):
            break
        if time.time() - t0 > 280:
            return time.time() - t0, "TIMEOUT", s
        time.sleep(0.5)
    dt = time.time() - t0
    u = (s.get("data") or [{}])[0].get("url")
    png_ok = None
    if u:
        raw = urllib.request.urlopen(AP + norm(u), timeout=120).read()
        png_ok = raw[:8] == b"\x89PNG\r\n\x1a\n"
        return dt, st, {"bytes": len(raw), "png": png_ok}
    return dt, st, s
