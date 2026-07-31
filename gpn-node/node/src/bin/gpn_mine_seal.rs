//! Minimal reference external miner — CPU light-cache Ethash seal generator.
//!
//! Usage:
//!   gpn_mine_seal <pre_hash_hex> <target_hex>
//!
//! Mines an Ethash solution (8-byte LE nonce ++ 32-byte mix hash) whose hash
//! is strictly below `target` (the `sc_consensus_pow` convention: lower is
//! harder). Prints the 40-byte seal as hex. This is the exact seal format the
//! `gpn_submitWork` RPC expects.
//!
//! This is the light-cache (CPU) analogue of the GPU miner — same algorithm,
//! same accepted seal. Useful as a reference and for testing the external
//! mining RPC without a GPU.

use sp_core::U256;
use ethereum_types::H64;
const DEV_EPOCH: usize = 0;

fn main() {
    let mut args = std::env::args().skip(1);
    let pre_hash_hex = args.next().expect("usage: gpn_mine_seal <pre_hash_hex> <target_hex>");
    let target_hex = args.next().expect("usage: gpn_mine_seal <pre_hash_hex> <target_hex>");

    let pre_hash = hex::decode(pre_hash_hex.trim_start_matches("0x"))
        .expect("invalid pre_hash hex");
    assert_eq!(pre_hash.len(), 32, "pre_hash must be 32 bytes");

    let target = U256::from_str_radix(target_hex.trim_start_matches("0x"), 16)
        .expect("invalid target hex");

    let seed = ethash::get_seedhash(DEV_EPOCH);
    let cache_size = ethash::get_cache_size(DEV_EPOCH);
    let full_size = ethash::get_full_size(DEV_EPOCH);
    let mut cache = vec![0u8; cache_size];
    ethash::make_cache(&mut cache, seed);

    let header_hash = ethereum_types::H256::from_slice(&pre_hash);
    let mut nonce: u64 = 0;
    loop {
        let (mix, result) = ethash::hashimoto_light(
            header_hash,
            H64::from_low_u64_be(nonce),
            full_size,
            &cache,
        );
        if U256::from_little_endian(result.as_bytes()) < target {
            let mut seal = Vec::with_capacity(40);
            seal.extend_from_slice(&nonce.to_le_bytes());
            seal.extend_from_slice(mix.as_bytes());
            println!("0x{}", hex::encode(seal));
            return;
        }
        nonce = nonce.wrapping_add(1);
    }
}
