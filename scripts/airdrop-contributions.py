#!/usr/bin/env python3
"""Airdrop contribution producer (GPN-LIST row 252).

Writes `airdrop-contributions.json` — schema `gpn-airdrop-contributions-v1` —
beside the referral registry so `gpn-referrals --export` pays measured
contribution instead of counting registrations.

Per account, all of it read from chain (plus the access point's own job ledger,
which is our server's record of which provider was actually assigned a job):

  blocks_authored    GPUPoW::BlocksAuthored — permanent, proof-of-work, not
                     self-reported and not resettable by re-registering.
  jobs_served        How many *completed* jobs the access point brokered to this
                     provider. Completion is confirmed on chain
                     (ComputeMarketplace::Jobs[id].state == Completed), so a job
                     the AP merely queued does not count.
  jobs_settled_gpn   The provider's net payout for those jobs, in planck
                     (12 decimals). Mirrors `settle_escrow`:
                     charge = JobBillingV2 rates, else JobBilling
                     price_per_token × tokens, else the quoted `price`, capped by
                     the quoted `price`; net = charge − charge × MarketplaceFeeBps
                     / 10_000 (500 bps = 5%). The pallet's sub-ED dust rule is not
                     modelled — it only moves amounts below the existential
                     deposit.

Why the AP ledger and not pure chain state: `JobProvider`/`JobEscrow` are
`take`n when a job completes (compute-marketplace `complete_job`), so on-chain
state cannot attribute a *finished* job to a provider. The AP's `/jobs` endpoint
keeps `id → provider → state` for every job it brokered; the price and the
completion state still come from chain.

A missing AP or missing chain entry never falls back to counting registrations:
an account with nothing measured gets zeros, which is what the export already
treats as "no contribution, no payout".

Usage:
  python3 scripts/airdrop-contributions.py [--rpc URL] [--ap URL] [--out FILE]
                                           [--registry FILE] [--min-blocks N]
"""

import argparse
import hashlib
import importlib.util
import json
import os
import struct
import sys
import urllib.error
import urllib.request

SCHEMA = "gpn-airdrop-contributions-v1"
PLANCK = 10 ** 12            # 12 decimals (gpn-token)
MARKETPLACE_FEE_BPS = 500    # runtime/src/lib.rs MarketplaceFeeBps (5%)

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _load_snapshot_helpers():
    """Reuse the proven ss58 / xxh64 / twox128 / Rpc helpers instead of a copy."""
    path = os.path.join(REPO, "gpn-snapshot.py")
    spec = importlib.util.spec_from_file_location("gpn_snapshot_helpers", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


snap = _load_snapshot_helpers()
twox128 = snap.twox128
ss58 = snap.ss58
Rpc = snap.Rpc


# ---------------------------------------------------------------- storage keys
def map_key(pallet, item, key_bytes=b""):
    """StorageMap key = twox128(pallet) ‖ twox128(item) ‖ blake2_128(k) ‖ k."""
    head = twox128(pallet.encode()) + twox128(item.encode())
    if not key_bytes:
        return "0x" + head.hex()
    return "0x" + (head + hashlib.blake2b(key_bytes, digest_size=16).digest() + key_bytes).hex()


def u64_key(job_id):
    return struct.pack("<Q", job_id)


# ---------------------------------------------------------------- SCALE decode
def decode_job(b):
    """ComputeJob prefix: id, GPURequirements, job_type, tier, state, price.

    Layout (compute-marketplace/src/lib.rs): id u64 ‖ min_vram_mb u32 ‖
    min_compute_capability u16 ‖ min_gpu_count u8 ‖ min_cuda_cores u32 ‖
    min_gflops u64 ‖ job_type u8 ‖ tier u8 ‖ state u8 ‖ price u128.
    """
    if len(b) < 46:
        return None
    return {
        "id": struct.unpack_from("<Q", b, 0)[0],
        "state": b[29],
        "price": struct.unpack_from("<Q", b, 30)[0]
        + (struct.unpack_from("<Q", b, 38)[0] << 64),
    }


JOB_STATE_COMPLETED = 4  # Pending 0, Accepted 1, Running 2, AwaitingVerification 3


def decode_billing(b):
    """JobBilling = (u128 price_per_token, u32 max_tokens, u32 actual_tokens)."""
    if len(b) < 24:
        return None
    return {
        "price_per_token": int.from_bytes(b[0:16], "little"),
        "max_tokens": struct.unpack_from("<I", b, 16)[0],
        "actual_tokens": struct.unpack_from("<I", b, 20)[0],
    }


def decode_billing_v2(b):
    """TwoRateBilling: in u128 ‖ out u128 ‖ max_out u32 ‖ actual_out u32 ‖ actual_in u32."""
    if len(b) < 40:
        return None
    return {
        "input_price_per_token": int.from_bytes(b[0:16], "little"),
        "output_price_per_token": int.from_bytes(b[16:32], "little"),
        "max_output_tokens": struct.unpack_from("<I", b, 32)[0],
        "actual_output_tokens": struct.unpack_from("<I", b, 36)[0],
        "actual_input_tokens": struct.unpack_from("<I", b, 40)[0],
    }


def charge_for(price, billing, billing_v2, fee_bps=MARKETPLACE_FEE_BPS):
    """The settled charge and the provider's net payout, per `settle_escrow`."""
    cap = price
    if billing_v2 and (
        billing_v2["input_price_per_token"] > 0 or billing_v2["output_price_per_token"] > 0
    ):
        out_toks = min(billing_v2["actual_output_tokens"], billing_v2["max_output_tokens"])
        charge = (
            billing_v2["input_price_per_token"] * billing_v2["actual_input_tokens"]
            + billing_v2["output_price_per_token"] * out_toks
        )
        charge = min(charge, cap)
    elif billing and billing["price_per_token"] > 0 and billing["max_tokens"] > 0:
        toks = min(billing["actual_tokens"], billing["max_tokens"])
        charge = min(billing["price_per_token"] * toks, cap)
    else:
        charge = cap  # flat-price job: the whole escrow was the charge
    fee = charge * fee_bps // 10_000
    return charge, charge - fee


# ---------------------------------------------------------------- inputs
def fetch_ap_jobs(ap_url, timeout=10):
    """`/jobs` from the access point: [{id, provider, state}, ...] (hex provider)."""
    try:
        with urllib.request.urlopen(ap_url.rstrip("/") + "/jobs", timeout=timeout) as r:
            doc = json.load(r)
    except (OSError, ValueError, urllib.error.URLError) as e:
        print("warn: access point %s unreadable (%s) — job counts are zero" % (ap_url, e),
              file=sys.stderr)
        return []
    jobs = doc.get("jobs") if isinstance(doc, dict) else None
    return jobs if isinstance(jobs, list) else []


def load_registry_addresses(path):
    """Address strings from the AP referral registry (ss58 or hex), if present."""
    try:
        with open(path) as f:
            doc = json.load(f)
    except (OSError, ValueError):
        return []
    return [a for a in doc if isinstance(a, str)] if isinstance(doc, dict) else []


def norm_addr(addr):
    """Canonical key: hex is case-insensitive, ss58 is NOT (base58)."""
    if addr.lower().startswith("0x"):
        return addr.lower()
    return addr


def pubkey_hex(addr):
    """32-byte account id as `0x…`, from a hex address or any ss58 spelling.

    An ss58 string is prefix byte + 32-byte pubkey + checksum, and different
    tools checksum differently (the access point's strings carry a checksum that
    differs from ours for the same pubkey) — so identity is the pubkey, never
    the string.
    """
    a = norm_addr(addr)
    if a.startswith("0x"):
        return a
    n = 0
    for c in a:
        n = n * 58 + snap.B58.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return "0x" + raw[1:33].hex()


def hex_to_ss58(addr):
    a = norm_addr(addr)
    if a.startswith("0x") and len(a) == 66:
        return ss58(bytes.fromhex(a[2:]))
    return a


def ss58_to_hex(addr):
    """Pubkey hex for an ss58 address (any prefix/checksum)."""
    return pubkey_hex(addr)
# ---------------------------------------------------------------- collect
def collect(rpc, ap_url):
    """{pubkey hex: {blocks_authored, jobs_served, jobs_settled_gpn}} from chain + AP."""
    rows = {}

    def row(addr):
        key = pubkey_hex(addr)
        return rows.setdefault(key, {"blocks_authored": 0, "jobs_served": 0,
                                     "jobs_settled_gpn": 0})

    # 1. blocks authored — permanent on-chain PoW
    prefix = map_key("GPUPoW", "BlocksAuthored")
    keys = rpc.call("state_getKeys", [prefix]) or []
    vals = {}
    if keys:
        for blk in rpc.call("state_queryStorageAt", [keys]) or []:
            for k, v in blk.get("changes", []):
                vals[k] = v
    for k in keys:
        v = vals.get(k) or rpc.call("state_getStorage", [k])
        if not v or v == "0x":
            continue
        acct = "0x" + bytes.fromhex(k[2:])[-32:].hex()
        row(acct)["blocks_authored"] = int.from_bytes(bytes.fromhex(v[2:]), "little")

    # 2. completed jobs the AP brokered, confirmed completed on chain, priced on chain
    completed = {}
    for j in fetch_ap_jobs(ap_url):
        if not isinstance(j, dict):
            continue
        jid = j.get("id")
        prov = j.get("provider") or j.get("lease_provider")
        if not isinstance(jid, int) or not isinstance(prov, str):
            continue
        completed.setdefault(norm_addr(prov), []).append(jid)

    ids = sorted({i for lst in completed.values() for i in lst})
    jobs_vals, billing_vals, billing2_vals = {}, {}, {}
    if ids:
        for item, dest in (("Jobs", jobs_vals), ("JobBilling", billing_vals),
                           ("JobBillingV2", billing2_vals)):
            ks = [map_key("ComputeMarketplace", item, u64_key(i)) for i in ids]
            for blk in rpc.call("state_queryStorageAt", [ks]) or []:
                for k, v in blk.get("changes", []):
                    dest[k] = v

    for prov, jids in sorted(completed.items()):
        r = row(prov)
        for jid in jids:
            raw = jobs_vals.get(map_key("ComputeMarketplace", "Jobs", u64_key(jid)))
            if not raw or raw == "0x":
                continue
            job = decode_job(bytes.fromhex(raw[2:]))
            if not job or job["state"] != JOB_STATE_COMPLETED:
                continue
            b1 = billing_vals.get(map_key("ComputeMarketplace", "JobBilling", u64_key(jid)))
            b2 = billing2_vals.get(map_key("ComputeMarketplace", "JobBillingV2", u64_key(jid)))
            _, net = charge_for(
                job["price"],
                decode_billing(bytes.fromhex(b1[2:])) if b1 and b1 != "0x" else None,
                decode_billing_v2(bytes.fromhex(b2[2:])) if b2 and b2 != "0x" else None,
            )
            r["jobs_served"] += 1
            r["jobs_settled_gpn"] += net

    return rows


def build_doc(rows, addresses, head, rpc_url, ap_url):
    """Sidecar document, keyed by the address spelling the export will look up.

    The Rust export matches participants by the address string in the referral
    registry, so a registry address is emitted exactly as the registry spells it;
    an account the registry does not know is emitted as prefix-42 ss58.
    """
    name_by_pubkey = {}
    for a in addresses:
        try:
            name_by_pubkey[pubkey_hex(a)] = a
        except (ValueError, KeyError):
            continue  # not an address we can decode — never guessed at
    accounts = {}
    for pk, r in sorted(rows.items()):
        if not any(r.values()):
            continue
        accounts[name_by_pubkey.get(pk, hex_to_ss58(pk))] = r
    missing = [a for a in addresses if pubkey_hex(a) not in rows]
    return {
        "schema": SCHEMA,
        "generated_at_block": head,
        "sources": {"rpc": rpc_url, "ap_jobs": ap_url + "/jobs",
                    "fee_bps": MARKETPLACE_FEE_BPS, "planck_per_gpn": PLANCK},
        "accounts": accounts,
    }, missing


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default=os.environ.get("GPN_RPC", "http://127.0.0.1:9944"))
    ap.add_argument("--ap", default=os.environ.get("GPN_AP", "http://127.0.0.1:8092"))
    ap.add_argument("--registry", default=os.environ.get(
        "GPN_REFERRAL_REGISTRY",
        os.path.join(os.environ.get("GPN_PREFIX") or os.path.expanduser("~/.gpn"),
                     "referral-registry.json")))
    ap.add_argument("--out", default=None,
                    help="default: airdrop-contributions.json beside --registry")
    args = ap.parse_args()

    out = args.out or os.path.join(os.path.dirname(os.path.abspath(args.registry)),
                                   "airdrop-contributions.json")
    rpc = Rpc(args.rpc)
    head = rpc.call("chain_getHeader", [])
    height = int(head["number"], 16) if head else 0

    rows = collect(rpc, args.ap)
    doc, missing = build_doc(rows, load_registry_addresses(args.registry), height,
                             args.rpc, args.ap)

    tmp = out + ".tmp"
    with open(tmp, "w") as f:
        json.dump(doc, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, out)

    paid = [a for a, r in doc["accounts"].items() if r["blocks_authored"] or r["jobs_served"]]
    print("wrote %s — %d contributing account(s) of %d chain contributors, block #%d"
          % (out, len(doc["accounts"]), len(rows), height))
    for a, r in list(doc["accounts"].items())[:10]:
        print("  %s  blocks=%-6d jobs=%-4d settled=%.6f GPN"
              % (a, r["blocks_authored"], r["jobs_served"], r["jobs_settled_gpn"] / PLANCK))
    if missing:
        print("  note: %d registry address(es) have no measured contribution" % len(missing))
    return 0 if paid else 1


if __name__ == "__main__":
    sys.exit(main())
