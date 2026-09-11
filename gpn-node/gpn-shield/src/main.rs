//! gpn_shield CLI — GPN shielded-pool client prover (Step 5).
//!
//! Commands:
//!   gpn_shield keygen   Print the on-chain verification key (matches pallet).
//!   gpn_shield prove    Produce + print the honest proof bytes for verify_proof.
//!   gpn_shield verify   Read a proof + public inputs, verify locally (pallet-identical).
//!
//! The proof bytes printed by `prove` are exactly what the pallet's
//! `verify_proof` dispatchable accepts (G1/G2 compressed + Fr big-endian).

use bls12_381::{G1Affine, G2Affine, Scalar};
use ff::Field;

use gpn_shield::{groth16, keygen, prove, qap, ser, verify, VerifyError};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("prove");

    match cmd {
        "keygen" => {
            let vk = keygen();
            println!("alpha_g1 = 0x{}", hex(&vk.alpha_g1.to_compressed()));
            println!("beta_g2  = 0x{}", hex(&vk.beta_g2.to_compressed()));
            println!("gamma_g2 = 0x{}", hex(&vk.gamma_g2.to_compressed()));
            println!("delta_g2 = 0x{}", hex(&vk.delta_g2.to_compressed()));
            println!(
                "gamma_abc[0] = 0x{}",
                hex(&vk.gamma_abc_g1[0].to_compressed())
            );
            println!("public_inputs = 0 (trivial l=0 circuit)");
        }
        "prove" => {
            let proof = prove();
            let (a, b, c) = ser::proof_to_bytes(&proof);
            println!("proof_a (48) = 0x{}", hex(&a));
            println!("proof_b (96) = 0x{}", hex(&b));
            println!("proof_c (48) = 0x{}", hex(&c));
            println!("public_inputs = []");
            println!("verify locally: {}", verify(&keygen(), &[], &proof).is_ok());
        }
        "verify" => {
            let a = parse_hex(&args.get(2).expect("proof_a hex")).expect("bad proof_a");
            let b = parse_hex(&args.get(3).expect("proof_b hex")).expect("bad proof_b");
            let c = parse_hex(&args.get(4).expect("proof_c hex")).expect("bad proof_c");
            let a: [u8; 48] = a.try_into().expect("proof_a must be 48 bytes");
            let b: [u8; 96] = b.try_into().expect("proof_b must be 96 bytes");
            let c: [u8; 48] = c.try_into().expect("proof_c must be 48 bytes");
            let Some(proof) = ser::proof_from_bytes(&a, &b, &c) else {
                println!("INVALID: bad point encoding");
                return;
            };
            match verify(&keygen(), &[], &proof) {
                Ok(()) => println!("VALID"),
                Err(VerifyError::InputLenMismatch) => println!("INVALID: input length mismatch"),
                Err(VerifyError::InvalidProof) => println!("INVALID: pairing equation failed"),
            }
        }
        "qap-setup" => {
            let r1cs = qap::value_balance_r1cs();
            let q = r1cs.qap();
            let seed = args.get(2).map(|s| s.as_str()).unwrap_or("testnet-crs-v0");
            let pk = groth16::setup(&q, seed);
            println!("circuit = value-balance (6 vars, 2 public, 3 constraints)");
            println!("seed = {seed}");
            println!("alpha_g1 = 0x{}", hex(&pk.vk.alpha_g1.to_compressed()));
            println!("beta_g2  = 0x{}", hex(&pk.vk.beta_g2.to_compressed()));
            println!("gamma_g2 = 0x{}", hex(&pk.vk.gamma_g2.to_compressed()));
            println!("delta_g2 = 0x{}", hex(&pk.vk.delta_g2.to_compressed()));
            println!(
                "gamma_abc[0] = 0x{}",
                hex(&pk.vk.gamma_abc_g1[0].to_compressed())
            );
            println!(
                "gamma_abc[1] = 0x{}",
                hex(&pk.vk.gamma_abc_g1[1].to_compressed())
            );
            println!("public_inputs = [fee] (1 public variable)");
        }
        "qap-prove" => {
            let r1cs = qap::value_balance_r1cs();
            let q = r1cs.qap();
            let pk = groth16::setup(&q, "testnet-crs-v0");
            // fee=2 (pub), v_in=10, v_out=8, spent=10, w=80
            let z = vec![
                Scalar::ONE,
                Scalar::from(2),
                Scalar::from(10),
                Scalar::from(8),
                Scalar::from(10),
                Scalar::from(80),
            ];
            let (proof, pub_in) = groth16::prove(&pk, &q, &z);
            println!("proof_a (48) = 0x{}", hex(&proof.a.to_compressed()));
            println!("proof_b (96) = 0x{}", hex(&proof.b.to_compressed()));
            println!("proof_c (48) = 0x{}", hex(&proof.c.to_compressed()));
            println!(
                "public_inputs = {:?}",
                pub_in.iter().map(|s| s.to_string()).collect::<Vec<_>>()
            );
            println!(
                "verify locally: {}",
                groth16::verify(&pk.vk, &pub_in, &proof).is_ok()
            );
        }
        "qap-verify" => {
            let r1cs = qap::value_balance_r1cs();
            let q = r1cs.qap();
            let pk = groth16::setup(&q, "testnet-crs-v0");
            let a = parse_hex(&args.get(2).expect("proof_a hex")).expect("bad proof_a");
            let b = parse_hex(&args.get(3).expect("proof_b hex")).expect("bad proof_b");
            let c = parse_hex(&args.get(4).expect("proof_c hex")).expect("bad proof_c");
            let a: [u8; 48] = a.try_into().expect("proof_a must be 48 bytes");
            let b: [u8; 96] = b.try_into().expect("proof_b must be 96 bytes");
            let c: [u8; 48] = c.try_into().expect("proof_c must be 48 bytes");
            let Some(pa) = G1Affine::from_compressed(&a).into_option() else {
                println!("INVALID: bad proof_a encoding");
                return;
            };
            let Some(pb) = G2Affine::from_compressed(&b).into_option() else {
                println!("INVALID: bad proof_b encoding");
                return;
            };
            let Some(pc) = G1Affine::from_compressed(&c).into_option() else {
                println!("INVALID: bad proof_c encoding");
                return;
            };
            let proof = groth16::Proof {
                a: pa,
                b: pb,
                c: pc,
            };
            // fee = 2 (the value-balance circuit's single public input)
            let pub_in = vec![Scalar::from(2)];
            match groth16::verify(&pk.vk, &pub_in, &proof) {
                Ok(()) => println!("VALID (fee=2)"),
                Err(groth16::VerifyError::InputLenMismatch) => {
                    println!("INVALID: input length mismatch")
                }
                Err(groth16::VerifyError::InvalidProof) => {
                    println!("INVALID: pairing equation failed")
                }
            }
        }
        "qap-setup-ct" => {
            // Constant-time keygen path: same key as `qap-setup`, but every
            // secret-scalar fixed-base mult uses the branchless CT lookup.
            let r1cs = qap::value_balance_r1cs();
            let q = r1cs.qap();
            let seed = args.get(2).map(|s| s.as_str()).unwrap_or("testnet-crs-v0");
            let pk = groth16::setup_constant_time(&q, seed);
            println!("circuit = value-balance (6 vars, 2 public, 3 constraints)");
            println!("seed = {seed} (constant-time keygen)");
            println!("alpha_g1 = 0x{}", hex(&pk.vk.alpha_g1.to_compressed()));
            println!("beta_g2  = 0x{}", hex(&pk.vk.beta_g2.to_compressed()));
            println!("gamma_g2 = 0x{}", hex(&pk.vk.gamma_g2.to_compressed()));
            println!("delta_g2 = 0x{}", hex(&pk.vk.delta_g2.to_compressed()));
            println!(
                "gamma_abc[0] = 0x{}",
                hex(&pk.vk.gamma_abc_g1[0].to_compressed())
            );
            println!(
                "gamma_abc[1] = 0x{}",
                hex(&pk.vk.gamma_abc_g1[1].to_compressed())
            );
            println!("public_inputs = [fee] (1 public variable)");
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: gpn_shield [keygen|prove|verify|qap-setup|qap-setup-ct|qap-prove|qap-verify]");
            std::process::exit(2);
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
