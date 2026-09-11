//! Standalone GPU Ethash miner for GPN.
//!
//! Builds the epoch-0 Ethash DAG on the GPU, then mines a seal below the
//! target, printing the 40-byte `nonce(8 LE) ++ mix_hash(32)` seal in hex —
//! exactly what gpn-node's `verify_seal` accepts.
//!
//! Usage:
//!   gpn_gpu_mine [header_hex] [target_hex] [batch] [nonce_start]
//!
//! Defaults: header = 0x11.., target = 2^245 (GPN dev genesis difficulty).

use gpn_agent::gpu::ethash_miner::EthashGpuMiner;

fn parse_hex(s: &str, expect: usize) -> [u8; 32] {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let clean = clean.trim_start_matches("0x");
    let mut out = [0u8; 32];
    let bytes = hex::decode(clean).expect("invalid hex");
    assert_eq!(bytes.len(), expect, "expected {expect} bytes");
    out.copy_from_slice(&bytes);
    out
}

fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let header = if args.len() > 1 {
        parse_hex(&args[1], 32)
    } else {
        [0x11u8; 32]
    };
    let target = if args.len() > 2 {
        parse_hex(&args[2], 32)
    } else {
        // 2^245, little-endian: byte 30 = 0x20
        let mut t = [0u8; 32];
        t[30] = 0x20;
        t
    };
    let batch: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1 << 20);
    let nonce_start: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);

    println!("== GPN GPU Ethash miner ==");
    for line in EthashGpuMiner::device_summary() {
        println!("  device: {line}");
    }

    let miner = EthashGpuMiner::new(0).expect("open GPU miner");
    println!("using GPU: {}", miner.device_name());

    let epoch = 0usize;
    let cache_size = ethash::get_cache_size(epoch);
    let full_size = ethash::get_full_size(epoch);
    let seed = ethash::get_seedhash(epoch);
    let mut cache = vec![0u8; cache_size];
    ethash::make_cache(&mut cache, seed);
    println!("light cache: {} KiB", cache_size / 1024);

    let dag = miner.build_dag(&cache, full_size).expect("build DAG");
    println!(
        "target: 0x{}",
        target
            .iter()
            .rev()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );

    let start = std::time::Instant::now();
    let seal = miner
        .mine(&dag, &header, &target, nonce_start, batch)
        .expect("mine");
    let elapsed = start.elapsed();

    // Self-verify exactly like gpn-node's `verify_seal`:
    //   recompute ethash::hashimoto_light, require mix match AND result < target.
    let ref_h = ethereum_types::H256::from_slice(&header);
    let ref_n = ethereum_types::H64::from_low_u64_be(seal.nonce);
    let (ref_mix, _ref_result) = ethash::hashimoto_light(ref_h, ref_n, full_size, &cache);
    let mix_ok = seal.mix.as_slice() == ref_mix.as_bytes();
    let result_le = sp_core::U256::from_little_endian(seal.result.as_slice());
    let target_le = sp_core::U256::from_little_endian(&target);
    let target_ok = result_le < target_le;
    println!("✅ FOUND seal (nonce={:#x})", seal.nonce);
    println!("  mix:    0x{}", hex::encode(seal.mix));
    println!("  result: 0x{}", hex::encode(seal.result));
    println!("  seal (40B): 0x{}", hex::encode(seal.to_seal_bytes()));
    println!("  verify: mix_match={mix_ok} result_below_target={target_ok}");
    if !(mix_ok && target_ok) {
        eprintln!("FAILED node verify_seal check");
        std::process::exit(1);
    }
    println!("  ✅ VERIFIED: this seal passes gpn-node's verify_seal");
    println!("  elapsed (search): {:?}", elapsed);
}
