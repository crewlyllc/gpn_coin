#!/usr/bin/env python3
"""
GPN end-to-end extrinsic test.

Exercises the full RPC -> tx pool -> PoW block authoring -> execution -> state
change loop against a running GPN dev node, covering:

  1. Balances.transfer_allow_death        (core currency move)
  2. ProfitabilityOracle.update_feed      (oracle operators, Alice is genesis operator)
  3. ComputeMarketplace.register_provider + create_job (escrow transfer)
  4. GPUAttestation.deposit_stake         (PoSG staking)

Notes:
  - The dev chain self-tunes to ~15 s blocks, so this test POLLS for the
    expected state change (up to a timeout) instead of sleeping a fixed time.
  - wait_for_inclusion is avoided (a scale-encoder quirk in some
    substrate-interface versions); execution is proven by state assertions.

Prereqs:
  - A GPN dev node running on ws://127.0.0.1:9944 with --force-authoring:
        ./target/release/gpn-node --dev --tmp --force-authoring --rpc-port 9944
  - pip3 install --user substrate-interface

Usage:
  python3 gpn_extrinsic_test.py
"""
import sys
import time

from substrateinterface import SubstrateInterface, Keypair

URL = "ws://127.0.0.1:9944"
ALICE = Keypair.create_from_uri("//Alice")
BOB_SS58 = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty"  # //Bob

PASS = 0
FAIL = 0
TIMEOUT = 60  # seconds to wait for a state change (blocks are ~15 s)


def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  ✅ {name} {detail}")
    else:
        FAIL += 1
        print(f"  ❌ {name} {detail}")


def query_balance(sub, who):
    r = sub.query("System", "Account", [who])
    return int(r.value["data"]["free"])


def submit(sub, call, keypair, expected_nonce):
    """Submit an extrinsic with an explicit nonce; return True if accepted."""
    ext = sub.create_signed_extrinsic(call=call, keypair=keypair, nonce=expected_nonce)
    try:
        receipt = sub.submit_extrinsic(ext, wait_for_inclusion=False, wait_for_finalization=False)
        print(f"    submitted (nonce={expected_nonce}): hash={getattr(receipt, 'extrinsic_hash', None)}")
        return True
    except Exception as e:
        print(f"    submit failed (nonce={expected_nonce}): {repr(e)}")
        return False


def wait_for(fn, name, timeout=TIMEOUT):
    """Poll fn() until it returns a truthy value or the timeout elapses."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            v = fn()
            if v:
                return v
        except Exception as e:
            print(f"    (poll err: {e})")
        time.sleep(3)
    return None


def account_nonce(sub, who):
    return int(sub.query("System", "Account", [who]).value["nonce"])


def wait_for_nonce(sub, who, expected, timeout=TIMEOUT):
    """Wait until the account's on-chain nonce reaches `expected`, proving the
    previous extrinsic was included in a block. Returns True on success."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if account_nonce(sub, who) >= expected:
                return True
        except Exception:
            pass
        time.sleep(3)
    return False


def main():
    sub = SubstrateInterface(url=URL, type_registry_preset="substrate-node-template")
    print("Connected. Chain:", sub.chain, "| SS58:", sub.ss58_format)

    # Track Alice's nonce so we never collide on the tx pool (blocks are ~15 s).
    nonce = account_nonce(sub, ALICE.ss58_address)
    print(f"Alice starting nonce: {nonce}")

    # 1. Balance transfer
    print("\n[1] Balances.transfer_allow_death (Alice -> Bob, 1.23 GPN)")
    before = query_balance(sub, ALICE.ss58_address)
    call = sub.compose_call("Balances", "transfer_allow_death", {
        "dest": BOB_SS58, "value": 1234567890123,
    })
    if submit(sub, call, ALICE, nonce):
        nonce += 1
        after = wait_for(lambda: query_balance(sub, ALICE.ss58_address)
                         if query_balance(sub, ALICE.ss58_address) == before - 1234567890123 else None,
                         "transfer included")
        check("balance debited by exact amount", after is not None,
              f"before={before} expected_after={before - 1234567890123}")
    else:
        check("balance debited by exact amount", False)

    # 2. Profitability oracle feed update (Alice is a genesis operator)
    print("\n[2] ProfitabilityOracle.update_feed")
    call = sub.compose_call("ProfitabilityOracle", "update_feed", {
        "token_price_usd": 150, "avg_gpu_hour_price": 200,
        "utilization_rate_bps": 5000, "staking_apr_bps": 1200,
    })
    if submit(sub, call, ALICE, nonce):
        nonce += 1
        feed = wait_for(lambda: sub.query("ProfitabilityOracle", "CurrentFeed", []).value
                        if int(sub.query("ProfitabilityOracle", "CurrentFeed", []).value["token_price_usd"]) == 150
                        else None, "feed update included")
        check("feed persisted", feed is not None, f"feed={feed}")
    else:
        check("feed persisted", False)

    # 3. Compute marketplace: register provider + create job (escrow)
    print("\n[3] ComputeMarketplace.register_provider + create_job")
    call = sub.compose_call("ComputeMarketplace", "register_provider", {
        "requirements": {
            "min_vram_mb": 8000, "min_compute_capability": 75,
            "min_gpu_count": 1, "min_cuda_cores": 0,
        },
    })
    if submit(sub, call, ALICE, nonce):
        nonce += 1
        # Wait for this tx to be included before the next (shared account).
        wait_for_nonce(sub, ALICE.ss58_address, nonce)

    escrow_before = query_balance(sub, ALICE.ss58_address)
    price = 5000000000000  # 0.005 GPN
    call = sub.compose_call("ComputeMarketplace", "create_job", {
        "gpu_requirements": {
            "min_vram_mb": 8000, "min_compute_capability": 75,
            "min_gpu_count": 1, "min_cuda_cores": 0,
        },
        "job_type": "LLMInference",
        "tier": "Spot",
        "price": price,
        "max_duration": 1000,
        "workload_hash": "0x" + "ab" * 32,
    })
    if submit(sub, call, ALICE, nonce):
        nonce += 1
        escrow_after = wait_for(
            lambda: query_balance(sub, ALICE.ss58_address)
            if query_balance(sub, ALICE.ss58_address) <= escrow_before - price else None,
            "create_job included")
        check("job price escrowed from requestor", escrow_after is not None,
              f"before={escrow_before} after={escrow_after}")
    else:
        check("job price escrowed from requestor", False)

    # 4. GPU attestation staking (PoSG) — storage map is `ProviderStake`
    print("\n[4] GPUAttestation.deposit_stake (MinAttestationStake = 1,000 GPN)")
    stake = 1000000000000000000000  # 1,000 GPN
    call = sub.compose_call("GPUAttestation", "deposit_stake", {"amount": stake})
    if submit(sub, call, ALICE, nonce):
        nonce += 1
        st = wait_for(
            lambda: int(sub.query("GPUAttestation", "ProviderStake", [ALICE.ss58_address]).value)
            if sub.query("GPUAttestation", "ProviderStake", [ALICE.ss58_address]).value is not None
            else None,
            "deposit_stake included")
        check("stake recorded", st is not None and st >= stake, f"stake={st}")
    else:
        check("stake recorded", False)

    sub.close()
    print(f"\n==== RESULT: {PASS} passed, {FAIL} failed ====")
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
