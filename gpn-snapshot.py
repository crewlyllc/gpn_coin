#!/usr/bin/env python3
"""GPN testnet-participation SNAPSHOT tool (GPN-TASKS item #20).

Read-only: queries the local GPN node over JSON-RPC and emits an auditable
snapshot of *who is on the testnet and how active they are*. Nothing is signed
or written to chain.

Signals read (all on-chain, no off-chain state required):
  * System::Account              -> every account that exists (nonce, free)
  * GPUPoW::BlocksAuthored       -> proven GPU mining work per account (miner)
  * ComputeMarketplace::RegisteredProviders / ProviderEndpoints   (provider)
  * ComputeMarketplace::RequestorJobs / ProviderJobs              (job counts)
  * GPUAttestation::Attestations / ProviderStake                  (attested/staked)
  * gpn-liveness::FullNodes / Endpoints / Bonds                   (full node)
  * gpn-liveness::FullNodes.uptime_bits                           (full-node uptime)

`first_seen` is *observed by this monitor*: it is the first snapshot time at
which an account appeared, and is preserved across runs in the snapshot index
(`<out>/index.json`). `last_seen` is refreshed every run. uptime for full nodes
comes from the on-chain `uptime_bits` bitmask (bit 0 = most recent window,
UptimeLookback = 10 windows -> 10_000 bps = 100%).

Usage:
  python3 gpn-snapshot.py [--rpc URL] [--out DIR]
"""
import argparse
import hashlib
import json
import os
import struct
import sys
import time
import urllib.request
from datetime import datetime, timezone

# ---------------------------------------------------------------- xxhash (twox64)
P1 = 0x9E3779B185EBCA87
P2 = 0xC2B2AE3D27D4EB4F
P3 = 0x165667B19E3779F9
P4 = 0x85EBCA77C2B2AE63
P5 = 0x27D4EB2F165667C5
M = 0xFFFFFFFFFFFFFFFF


def _rotl(x, r):
    return ((x << r) | (x >> (64 - r))) & M


def xxh64(data, seed=0):
    n = len(data)
    i = 0
    if n >= 32:
        v1 = (seed + P1 + P2) & M
        v2 = (seed + P2) & M
        v3 = seed & M
        v4 = (seed - P1) & M
        while i + 32 <= n:
            v1 = (_rotl((v1 + int.from_bytes(data[i:i + 8], 'little') * P2) & M, 31) * P1) & M
            v2 = (_rotl((v2 + int.from_bytes(data[i + 8:i + 16], 'little') * P2) & M, 31) * P1) & M
            v3 = (_rotl((v3 + int.from_bytes(data[i + 16:i + 24], 'little') * P2) & M, 31) * P1) & M
            v4 = (_rotl((v4 + int.from_bytes(data[i + 24:i + 32], 'little') * P2) & M, 31) * P1) & M
            i += 32
        h = (_rotl(v1, 1) + _rotl(v2, 7) + _rotl(v3, 12) + _rotl(v4, 18)) & M
        for v in (v1, v2, v3, v4):
            v = (_rotl((v * P2) & M, 31) * P1) & M
            h = ((h ^ v) * P1 + P4) & M
    else:
        h = (seed + P5) & M
    h = (h + n) & M
    while i + 8 <= n:
        h = (_rotl(h ^ ((_rotl((int.from_bytes(data[i:i + 8], 'little') * P2) & M, 31) * P1) & M), 27) * P1 + P4) & M
        i += 8
    if i + 4 <= n:
        h = (_rotl(h ^ ((int.from_bytes(data[i:i + 4], 'little') * P1) & M), 23) * P2 + P3) & M
        i += 4
    while i < n:
        h = (_rotl(h ^ (data[i] * P5 & M), 11) * P1) & M
        i += 1
    h ^= h >> 33
    h = (h * P2) & M
    h ^= h >> 29
    h = (h * P3) & M
    h ^= h >> 32
    return h


def twox128(d):
    return struct.pack('<Q', xxh64(d, 0)) + struct.pack('<Q', xxh64(d, 1))


# ---------------------------------------------------------------- ss58
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58encode(b):
    n = int.from_bytes(b, 'big')
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    pad = 0
    for c in b:
        if c == 0:
            pad += 1
        else:
            break
    return B58[0] * pad + s


def ss58(pub, prefix=42):
    data = bytes([prefix]) + pub
    chk = hashlib.blake2b(data, digest_size=64).digest()[:2]
    return b58encode(data + chk)


# ---------------------------------------------------------------- rpc
class Rpc:
    def __init__(self, url):
        self.url = url
        self._id = 0

    def call(self, method, params):
        self._id += 1
        req = urllib.request.Request(
            self.url,
            data=json.dumps({"jsonrpc": "2.0", "id": self._id, "method": method, "params": params}).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=20) as r:
            return json.load(r).get("result")


def map_prefix(pallet, item):
    return "0x" + (twox128(pallet.encode()) + twox128(item.encode())).hex()


def _batched_values(rpc, keys, chunk=1000):
    """Fetch many storage values in big chunks via state_queryStorageAt.
    Returns {key_hex: value_hex}."""
    out = {}
    for i in range(0, len(keys), chunk):
        batch = keys[i:i + chunk]
        res = rpc.call("state_queryStorageAt", [batch]) or []
        for blk in res:
            for k, v in blk.get("changes", []):
                out[k] = v
    return out


def iter_map(rpc, pallet, item):
    """Yield (raw_key_bytes, value_hex) for every entry of a StorageMap
    keyed with Blake2_128Concat (values fetched in batches)."""
    prefix = map_prefix(pallet, item)
    keys = rpc.call("state_getKeys", [prefix]) or []
    values = _batched_values(rpc, keys)
    for k in keys:
        yield bytes.fromhex(k[2:]), values.get(k, "0x")


def le(val_hex, default=0):
    if not val_hex or val_hex == "0x":
        return default
    return int.from_bytes(bytes.fromhex(val_hex[2:]), 'little')


def vec_len(val_hex):
    """SCALE Vec<T>: first 4 bytes (LE compact-ish) = element count."""
    if not val_hex or val_hex == "0x":
        return 0
    b = bytes.fromhex(val_hex[2:])
    # Vec length is a compact-encoded u32; testnet job counts are < 2^14 so the
    # single-byte compact form applies. Fall back to a full compact decode.
    return _compact_u32(b)


def _compact_u32(b):
    fb = b[0]
    mode = fb & 0b11
    if mode == 0:
        return fb >> 2
    if mode == 1:
        return int.from_bytes(b[0:2], 'little') >> 2
    if mode == 2:
        return int.from_bytes(b[0:4], 'little') >> 2
    # big-integer mode
    nbytes = (fb >> 2) + 4
    return int.from_bytes(b[1:1 + nbytes], 'little')


def popcount(x):
    return bin(x).count("1")


class Participant:
    __slots__ = ("ss58", "hex", "balance_free", "nonce", "roles",
                 "blocks_authored", "jobs_served", "jobs_created",
                 "attested_gflops", "tier", "missed_count", "stake",
                 "uptime_bits", "uptime_windows", "uptime_bps", "fullnode_active",
                 "is_pallet_account")

    def __init__(self, ss58_, hex_):
        self.ss58 = ss58_
        self.hex = hex_
        self.balance_free = 0
        self.nonce = 0
        self.roles = set()
        self.blocks_authored = 0
        self.jobs_served = 0
        self.jobs_created = 0
        self.attested_gflops = 0
        self.tier = "Unattested"
        self.missed_count = 0
        self.stake = 0
        self.uptime_bits = 0
        self.uptime_windows = 0
        self.uptime_bps = 0
        self.fullnode_active = False
        self.is_pallet_account = False

    def as_dict(self):
        return {
            "ss58": self.ss58,
            "hex": self.hex,
            "roles": sorted(self.roles),
            "is_pallet_account": self.is_pallet_account,
            "balance_free_planck": str(self.balance_free),
            "nonce": self.nonce,
            "blocks_authored": self.blocks_authored,
            "jobs_served": self.jobs_served,
            "jobs_created": self.jobs_created,
            "attested_gflops": self.attested_gflops,
            "attestation_tier": self.tier,
            "attestation_missed_count": self.missed_count,
            "stake_planck": str(self.stake),
            "fullnode_uptime_bits": self.uptime_bits,
            "fullnode_uptime_windows_confirmed": self.uptime_windows,
            "fullnode_uptime_bps": self.uptime_bps,
            "fullnode_active": self.fullnode_active,
        }


TIERS = ["Unattested", "Bronze", "Silver", "Gold"]
UPTIME_LOOKBACK = 10  # gpn-liveness::UptimeLookback (runtime constant, windows)


def collect(rpc):
    parts = {}

    def get(acct):
        if acct is None:
            return None
        key = acct.hex()
        p = parts.get(key)
        if p is None:
            p = Participant(ss58(acct), "0x" + key)
            parts[key] = p
        return p

    # 1. every existing account (balance + nonce)
    for kb, val in iter_map(rpc, "System", "Account"):
        acct = kb[-32:] if len(kb) >= 32 else None
        p = get(acct)
        if p is None or not val or val == "0x":
            continue
        if acct[:4] == b"modl":          # pallet/derived account, not a person
            p.is_pallet_account = True
        b = bytes.fromhex(val[2:])
        # AccountInfo { nonce u32, consumers u32, providers u32, sufficients u32,
        #               AccountData { free u128, reserved u128, frozen u128, flags u128 } }
        if len(b) >= 32:
            p.nonce = int.from_bytes(b[0:4], 'little')
            p.balance_free = int.from_bytes(b[16:32], 'little')

    # 2. miners (proven GPU work)
    for kb, val in iter_map(rpc, "GPUPoW", "BlocksAuthored"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is not None:
            p.blocks_authored = le(val)
            p.roles.add("miner")

    # 3. providers (on-chain advertised endpoint)
    for kb, val in iter_map(rpc, "ComputeMarketplace", "ProviderEndpoints"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is not None:
            p.roles.add("provider")
    # requestor accounts (active requestor assignment vectors)
    for kb, val in iter_map(rpc, "ComputeMarketplace", "RequestorJobs"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is not None:
            p.roles.add("requestor")

    # 4. job workload.
    #    `JobRequestor[<job id>]` is written at create and NEVER removed, so it
    #    gives a LIFETIME count of jobs created per account. `JobProvider[<id>]`
    #    is `take()`n on terminal transition, so lifetime jobs *served* is not on
    #    chain; the per-provider active-assignment vectors below give what is.
    for _kb, val in iter_map(rpc, "ComputeMarketplace", "JobRequestor"):
        if val and val != "0x":
            p = get(bytes.fromhex(val[2:]))
            if p is not None:
                p.jobs_created += 1
                p.roles.add("requestor")
    for kb, val in iter_map(rpc, "ComputeMarketplace", "ProviderJobs"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is not None:
            n = vec_len(val)
            if n:
                p.jobs_served = n
                p.roles.add("provider")

    # 5. attested / staked
    for kb, val in iter_map(rpc, "GPUAttestation", "Attestations"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is None or not val or val == "0x":
            continue
        b = bytes.fromhex(val[2:])
        # AttestationRecord { submitted_at u32, gpu_info_hash [u8;32],
        #   challenge_response [u8;32], gpu_count u8, compute_gflops u64,
        #   tier enum u8, missed_count u32 }  = 4+32+32+1+8+1+4 = 82 bytes
        if len(b) >= 82:
            p.attested_gflops = int.from_bytes(b[69:77], 'little')
            t = b[77]
            p.tier = TIERS[t] if t < len(TIERS) else "Unattested"
            p.missed_count = int.from_bytes(b[78:82], 'little')
            p.roles.add("provider")
    for kb, val in iter_map(rpc, "GPUAttestation", "ProviderStake"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is not None:
            p.stake = le(val)
            if p.stake:
                p.roles.add("staker")

    # 6. full nodes
    for kb, val in iter_map(rpc, "gpn-liveness", "FullNodes"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is None or not val or val == "0x":
            continue
        b = bytes.fromhex(val[2:])
        # FullNodeRecord { last_confirmed_window u64, uptime_bits u32,
        #                  missed_count u32, active bool } = 17 bytes
        if len(b) >= 17:
            p.uptime_bits = int.from_bytes(b[8:12], 'little')
            p.uptime_windows = popcount(p.uptime_bits)
            p.fullnode_active = bool(b[16])
            p.uptime_bps = min(10000, p.uptime_windows * 10000 // UPTIME_LOOKBACK)
            p.roles.add("fullnode")
    for kb, val in iter_map(rpc, "gpn-liveness", "Endpoints"):
        p = get(kb[-32:] if len(kb) >= 32 else None)
        if p is not None:
            p.roles.add("fullnode")

    return parts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default=os.environ.get("GPN_RPC", "http://127.0.0.1:9944"))
    ap.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                  "shared", "artifacts", "project7", "airdrop"))
    args = ap.parse_args()

    rpc = Rpc(args.rpc)
    head = rpc.call("chain_getHeader", [])
    height = int(head["number"], 16)
    rv = rpc.call("state_getRuntimeVersion", [])
    now = int(time.time())

    parts = collect(rpc)

    outdir = args.out
    os.makedirs(os.path.join(outdir, "snapshots"), exist_ok=True)
    index_path = os.path.join(outdir, "index.json")
    index = {}
    if os.path.exists(index_path):
        with open(index_path) as f:
            index = json.load(f)
    prev_count = index.pop("__snapshot_count__", 0) if isinstance(index.get("__snapshot_count__"), int) else 0
    snapshot_count = prev_count + 1

    # first-seen bookkeeping (observed by this monitor)
    for key, p in parts.items():
        rec = index.get(key)
        if rec is None:
            index[key] = {"ss58": p.ss58, "first_seen": now, "last_seen": now,
                          "snapshots_seen": 1}
        else:
            rec["last_seen"] = now
            rec["ss58"] = p.ss58
            rec["snapshots_seen"] = rec.get("snapshots_seen", 1) + 1

    records = []
    for key, p in sorted(parts.items()):
        d = p.as_dict()
        idx = index.get(key, {})
        d["first_seen"] = idx.get("first_seen", now)
        d["last_seen"] = idx.get("last_seen", now)
        d["active_seconds"] = d["last_seen"] - d["first_seen"]
        d["snapshots_seen"] = idx.get("snapshots_seen", 1)
        d["observed_uptime_bps"] = (10000 * d["snapshots_seen"] // snapshot_count) if snapshot_count else 0
        records.append(d)

    humans = [r for r in records if not r["is_pallet_account"]]
    snapshot = {
        "tool": "gpn-snapshot",
        "task": "GPN-TASKS #20 (testnet-participation snapshot)",
        "generated_at": now,
        "generated_at_iso": datetime.fromtimestamp(now, tz=timezone.utc).isoformat(),
        "rpc": args.rpc,
        "chain": {
            "runtime_spec_version": rv.get("specVersion"),
            "best_block": height,
        },
        "participant_count": len(humans),
        "pallet_account_count": len(records) - len(humans),
        "snapshot_count": snapshot_count,
        "participants": records,
    }

    snap_path = os.path.join(outdir, "snapshots", "snapshot-%d-%d.json" % (height, now))
    with open(snap_path, "w") as f:
        json.dump(snapshot, f, indent=2)
    index["__snapshot_count__"] = snapshot_count
    with open(index_path, "w") as f:
        json.dump(index, f, indent=2, sort_keys=True)
    with open(os.path.join(outdir, "latest.json"), "w") as f:
        json.dump(snapshot, f, indent=2)

    roles = {}
    for r in humans:
        for role in r["roles"]:
            roles[role] = roles.get(role, 0) + 1
    print("snapshot -> %s" % snap_path)
    print("  spec=%s best=#%d participants=%d (+%d pallet accounts)"
          % (rv.get("specVersion"), height, len(humans), len(records) - len(humans)))
    print("  roles: %s" % ", ".join("%s=%d" % kv for kv in sorted(roles.items())))


if __name__ == "__main__":
    sys.exit(main())
