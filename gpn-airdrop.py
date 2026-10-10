#!/usr/bin/env python3
"""GPN airdrop allocation calculator (GPN-TASKS item #20).

Reads a snapshot produced by `gpn-snapshot.py` and applies a **documented,
auditable** participation-and-uptime weighting policy to split a fixed token
pool. Deterministic: same inputs -> same numbers.

Policy (see `shared/artifacts/project7/airdrop/ALLOCATION-SPEC.md`):
    score = Σ_over_roles  role_weight × activity_factor(role) × uptime_factor
    allocation_i = pool × score_i / Σ score

role weights (relative):
    miner      = 3.0   (proven GPU work: blocks authored)
    provider   = 4.0   (serving AI compute: attested GFLOPS × tier multiplier)
    requestor  = 2.0   (driving demand: lifetime jobs created)
    fullnode   = 3.0   (network service: on-chain liveness uptime)
    staker     = 1.0   (economic commitment)

activity factors are sub-linear (sqrt-based) so whales can't linearly dominate:
    miner     -> sqrt(blocks_authored / 100)      (normalised, see spec)
    provider  -> (attested_gflops / 1000) capped, × tier mult {U:0,B:1,S:1.5,G:2}
    requestor -> sqrt(jobs_created)
    fullnode  -> uptime_bps/10000
    staker    -> 1

uptime factor (multiplies the whole score):
    full nodes use on-chain uptime_bps; everyone else uses observed_uptime_bps
    (fraction of snapshots the account was seen in), floored at 0.10 so a
    one-off appearance still counts a little.

Usage:
  python3 gpn-airdrop.py [--snapshot FILE] [--pool TOKENS] [--out DIR]
"""
import argparse
import json
import math
import os
import sys

ROLE_WEIGHTS = {
    "miner": 3.0,
    "provider": 4.0,
    "requestor": 2.0,
    "fullnode": 3.0,
    "staker": 1.0,
}
TIER_MULT = {"Unattested": 0.0, "Bronze": 1.0, "Silver": 1.5, "Gold": 2.0}
UPTIME_FLOOR = 0.10
MIN_SCORE = 1e-9


def role_score(role, p):
    if role == "miner":
        return ROLE_WEIGHTS[role] * math.sqrt(p["blocks_authored"] / 100.0)
    if role == "provider":
        gflops = p.get("attested_gflops", 0)
        mult = TIER_MULT.get(p.get("attestation_tier", "Unattested"), 0.0)
        return ROLE_WEIGHTS[role] * (gflops / 1000.0) * mult
    if role == "requestor":
        return ROLE_WEIGHTS[role] * math.sqrt(max(p.get("jobs_created", 0), 0))
    if role == "fullnode":
        return ROLE_WEIGHTS[role] * (p.get("fullnode_uptime_bps", p.get("observed_uptime_bps", 0)) / 10000.0)
    if role == "staker":
        return ROLE_WEIGHTS[role]
    return 0.0


def uptime_factor(p):
    fn = p.get("fullnode_uptime_bps")
    bps = fn if (fn is not None and p.get("fullnode_uptime_windows_confirmed", 0) > 0) \
        else p.get("observed_uptime_bps", 0)
    return max(UPTIME_FLOOR, bps / 10000.0)


def selftest():
    """Deterministic checks of the scoring policy (no node/fixture needed)."""
    p = {"roles": ["miner", "provider", "staker"], "blocks_authored": 400,
         "attested_gflops": 30000, "attestation_tier": "Bronze",
         "observed_uptime_bps": 10000}
    sm = role_score("miner", p)
    assert abs(sm - 3.0 * math.sqrt(4.0)) < 1e-9, sm
    sp = role_score("provider", p)
    assert abs(sp - 4.0 * 30.0 * 1.0) < 1e-9, sp
    assert role_score("staker", p) == 1.0
    assert role_score("provider", {"attested_gflops": 30000, "attestation_tier": "Unattested"}) == 0.0
    assert abs(uptime_factor(p) - 1.0) < 1e-9
    assert abs(uptime_factor({"observed_uptime_bps": 0}) - UPTIME_FLOOR) < 1e-9
    # monotonicity: more blocks authored -> strictly higher miner score
    assert role_score("miner", {"blocks_authored": 100}) > role_score("miner", {"blocks_authored": 99})
    print("selftest OK")
    return 0


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser()
    ap.add_argument("--snapshot", default=os.path.join(here, "shared", "artifacts",
                                                       "project7", "airdrop", "latest.json"))
    ap.add_argument("--pool", type=float, default=10_000_000.0,
                    help="total airdrop pool in GPN")
    ap.add_argument("--out", default=os.path.join(here, "shared", "artifacts",
                                                  "project7", "airdrop"))
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()

    if args.selftest:
        return selftest()

    with open(args.snapshot) as f:
        snap = json.load(f)

    scored = []
    for p in snap["participants"]:
        if p.get("is_pallet_account"):
            continue
        roles = p.get("roles", [])
        if not roles:
            continue
        per_role = {r: role_score(r, p) for r in roles}
        base = sum(per_role.values())
        s = base * uptime_factor(p)
        if s > MIN_SCORE:
            scored.append((p, s, per_role))

    total = sum(s for _, s, _ in scored)
    if total <= 0:
        print("no eligible participants with positive score", file=sys.stderr)
        return 1

    allocations = []
    for p, s, per_role in sorted(scored, key=lambda x: -x[1]):
        amount = args.pool * s / total
        allocations.append({
            "ss58": p["ss58"],
            "hex": p["hex"],
            "roles": p["roles"],
            "score": round(s, 6),
            "score_by_role": {k: round(v, 6) for k, v in per_role.items()},
            "share": round(s / total, 8),
            "allocation_gpn": round(amount, 6),
            "uptime_bps": p.get("observed_uptime_bps", 0),
            "first_seen": p.get("first_seen"),
            "last_seen": p.get("last_seen"),
            "blocks_authored": p.get("blocks_authored", 0),
            "jobs_created": p.get("jobs_created", 0),
            "attestation_tier": p.get("attestation_tier"),
            "attested_gflops": p.get("attested_gflops", 0),
        })

    result = {
        "tool": "gpn-airdrop",
        "task": "GPN-TASKS #20 (launch airdrop allocation)",
        "policy_version": "v1",
        "snapshot_source": os.path.basename(args.snapshot),
        "snapshot_best_block": snap.get("chain", {}).get("best_block"),
        "snapshot_count": snap.get("snapshot_count", 1),
        "pool_gpn": args.pool,
        "eligible_count": len(allocations),
        "total_score": round(total, 6),
        "role_weights": ROLE_WEIGHTS,
        "tier_multipliers": TIER_MULT,
        "uptime_floor": UPTIME_FLOOR,
        "allocations": allocations,
    }

    # integrity checks: allocations must conserve the pool exactly
    assert abs(sum(a["allocation_gpn"] for a in allocations) - args.pool) < 1e-3
    assert abs(sum(a["share"] for a in allocations) - 1.0) < 1e-6

    os.makedirs(args.out, exist_ok=True)
    jpath = os.path.join(args.out, "airdrop-allocation.json")
    cpath = os.path.join(args.out, "airdrop-allocation.csv")
    with open(jpath, "w") as f:
        json.dump(result, f, indent=2)
    with open(cpath, "w") as f:
        f.write("ss58,roles,score,share,allocation_gpn\n")
        for a in allocations:
            f.write("%s,%s,%s,%s,%s\n" % (a["ss58"], "|".join(a["roles"]),
                                          a["score"], a["share"], a["allocation_gpn"]))

    print("airdrop -> %s" % jpath)
    print("  pool=%.0f GPN  eligible=%d  total_score=%.4f" % (args.pool, len(allocations), total))
    for a in allocations[:20]:
        print("  %-48s %-22s %8.4f GPN  (%.4f%%)"
              % (a["ss58"], ",".join(a["roles"]), a["allocation_gpn"], 100 * a["share"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
