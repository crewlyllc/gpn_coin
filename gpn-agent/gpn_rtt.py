#!/usr/bin/env python3
"""
gpn_rtt.py — client-side RTT helpers for GPN access-point routing (roadmap item 3).

The access-point scheduler natively scores providers by its OWN round-trip to
each provider (`AP → provider`). That is not the latency a *user* experiences:
the real path is `user → access point → provider`, and the user↔AP leg is paid
on every call regardless of which provider the AP picks.

The fix is to let the **client** measure the path it actually cares about and
hand those numbers to the access point, which then prefers the provider closest
to the user. This module provides that measurement:

  * `measure_rtt(url)`          — how many ms is `url` from *here*?
  * `pick_nearest_ap(aps)`      — hit the closest access point (multi-AP setups)
  * `provider_rtts(ap)`         — measure the user→provider RTT for every provider
                                  the AP knows about, keyed by address/status URL
  * `ask(...)`                  — POST a job with those RTT hints attached

No third-party deps (stdlib urllib) so a Windows user can run it as-is.

    python gpn_rtt.py "What is the capital of Italy? Answer in one word."
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.request

# Access points to consider. In a multi-AP network, list them all; the client
# picks the lowest-RTT one. Override with GPN_APS (comma-separated) or the `aps`/
# `ap` argument to `ask()`.
DEFAULT_APS = [
    a.strip()
    for a in os.environ.get("GPN_APS", "http://gpn-testnet.duckdns.org:8092").split(",")
    if a.strip()
]


def _get_json(url: str, timeout: float = 3.0):
    req = urllib.request.Request(
        url, headers={"Accept": "application/json", "X-Api-Key": _ap_token()}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def _ap_token() -> str:
    """Access-point bearer token, if the deployment requires one.

    Non-loopback POSTs to a GPN access point need `Authorization: Bearer <token>`
    (or `X-Api-Key`). Set `GPN_AP_TOKEN` in the environment; empty = none (loopback
    / open access points).
    """
    return os.environ.get("GPN_AP_TOKEN", "")


def measure_rtt(url: str, samples: int = 3, timeout: float = 3.0) -> int | None:
    """Return the best (lowest) round-trip time in ms for `url`, or None.

    `url` may be an access point root (`...:8092`) or a provider status URL
    (`...:8093/status`); a `/ping` (for an AP) or `/status` (for a provider) is
    appended as needed. We take the *minimum* of a few samples to strip out
    jitter — routing wants the achievable latency, not the noisiest one.
    """
    base = url.rstrip("/")
    if base.endswith("/ping") or base.endswith("/status"):
        probe = base
    elif base.count(":") >= 2 and base.split(":")[-1].split("/")[0] in ("8092", "8093"):
        # known GPN ports: 8093 is a provider heartbeat, 8092 an access point
        probe = base + ("/status" if ":8093" in base else "/ping")
    else:
        probe = base + "/ping"
    best: int | None = None
    for _ in range(max(1, samples)):
        t0 = time.perf_counter()
        try:
            _get_json(probe, timeout=timeout)
        except Exception:
            continue
        dt = int((time.perf_counter() - t0) * 1000)
        best = dt if best is None else min(best, dt)
    return best


def pick_nearest_ap(aps: list[str]) -> tuple[str, int] | None:
    """Choose the access point with the lowest client-measured RTT."""
    scored = [(ap, r) for ap in aps if (r := measure_rtt(ap)) is not None]
    if not scored:
        return None
    return min(scored, key=lambda x: x[1])


def provider_rtts(ap: str, samples: int = 2) -> dict[str, int]:
    """Measure the user→provider RTT for every provider `ap` reports.

    Returns `{ "<address-or-status_url>": ms }` suitable for the `rtt` field of
    a job request. Providers that look unhealthy (no live heartbeat) are skipped.
    """
    out: dict[str, int] = {}
    try:
        reg = _get_json(f"{ap.rstrip('/')}/providers")
    except Exception:
        return out
    for p in reg.get("providers", []):
        if not p.get("healthy", True):
            continue
        url = p.get("status_url")
        if not url:
            continue
        r = measure_rtt(url, samples=samples)
        if r is None:
            continue
        key = p.get("address") or url
        out[key] = r
    return out


def ask(prompt: str, model: str = "llama3.2:1b", ap: str | None = None,
        aps: list[str] | None = None, wait_secs: int = 180, use_rtt: bool = True) -> dict:
    """Submit a job to the nearest access point, with client-RTT routing hints."""
    if ap is None:
        aps = aps or DEFAULT_APS
        picked = pick_nearest_ap(aps)
        if picked is None:
            raise RuntimeError(f"no reachable access point among {aps}")
        ap, ap_rtt = picked
    else:
        ap_rtt = measure_rtt(ap)

    body = {
        "job_type": "LLMInference",
        "prompt": prompt,
        "model": model,
        "price_gpn": 10,
        "min_gflops": 150,
        "wait": True,
        "wait_secs": wait_secs,
    }
    if use_rtt:
        rtts = provider_rtts(ap)
        if rtts:
            body["rtt"] = rtts  # user→provider measurements -> AP routes to the nearest
        if ap_rtt is not None:
            body["client_rtt_ms"] = ap_rtt  # telemetry: the user→AP leg

    req = urllib.request.Request(
        f"{ap.rstrip('/')}/jobs",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "X-Api-Key": _ap_token()},
    )
    with urllib.request.urlopen(req, timeout=wait_secs + 30) as r:
        job = json.loads(r.read().decode())
    if not job.get("ok"):
        raise RuntimeError(f"job failed: {job}")
    return job


if __name__ == "__main__":
    q = " ".join(sys.argv[1:]) or "What is the capital of Japan? Answer in one word."
    job = ask(q)
    ans = job.get("answer", {})
    print(f"job #{job['job_id']}  routed_to={job.get('routed_to')}  "
          f"latency_ms={job.get('latency_ms')}  status={ans.get('status')}")
    print(f"answer: {ans.get('response')}")
