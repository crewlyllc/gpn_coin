//! gpn_keygen — generate a fresh sr25519 keypair (mnemonic + seed + ss58),
//! fully offline. Mirrors the format used in `governance-keys.txt`:
//!   label<TAB>secret_phrase<TAB>secret_seed<TAB>ss58_address
//!
//! Usage: gpn_keygen [--words N]
//! (sp-core emits a 12-word phrase; `--words` accepted for forward-compat.)

use sp_core::crypto::{Pair as _, Ss58Codec};
use sp_core::sr25519;

fn main() {
    let words: usize = std::env::args()
        .position(|a| a == "--words")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|w| w.parse().ok())
        .unwrap_or(12);
    let _ = words; // non-12 fall back to sp-core's 12-word generation

    let (pair, phrase, seed) = sr25519::Pair::generate_with_phrase(None);
    let ss58 = pair.public().to_ss58check();
    let hex_pub = hex::encode(pair.public().0);

    println!("phrase_text\t{phrase}");
    println!("seed\t0x{}", hex::encode(seed));
    println!("public_hex\t0x{hex_pub}");
    println!("ss58\t{ss58}");
    // Tab-separated governance-keys.txt line (label filled by caller):
    println!(
        "keyline\t<label>\t{phrase}\t0x{}\t{ss58}",
        hex::encode(seed)
    );
}
