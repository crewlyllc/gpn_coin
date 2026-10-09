//! `gpn_ceremony` — GPN Groth16 trusted-setup ceremony CLI.
//!
//! Commands:
//!   gpn_ceremony init       --circuit note-spend --out phase2-initial.bin [--record init.json] [--seeded-from <public>] [--contributor <name>]
//!   gpn_ceremony contribute --in <crs> --out <crs'> --record <contribution.json> [--seeded-from <public>] [--contributor <name>]
//!   gpn_ceremony verify     --crs <crs> --records r1.json,r2.json,...
//!   gpn_ceremony export-vk  --crs <crs> --out vk.bin
//!
//! `--seeded-from` makes a contribution publicly un-biasable (derive entropy
//! from e.g. a drand round or a future block hash). Without it, OS entropy is
//! used. Fresh contribution scalars are wiped from memory before exit.

use std::collections::HashMap;
use std::process::exit;

use zeroize::Zeroizing;

use gpn_shield::ceremony::{
    self, contribution_entropy, genesis_seed, ContributionRecord, FreshSecrets,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let opts = parse_opts(&args[2..]);
    match cmd {
        "init" => cmd_init(&opts),
        "contribute" => cmd_contribute(&opts),
        "verify" => cmd_verify(&opts),
        "verify-chain" => cmd_verify_chain(&opts),
        "export-vk" => cmd_export_vk(&opts),
        "" | "--help" | "-h" | "help" => usage(),
        other => {
            eprintln!("unknown command: {other}\n");
            usage();
            exit(2);
        }
    }
}

fn usage() {
    eprintln!(
        "gpn_ceremony — GPN Groth16 trusted-setup ceremony\n\n\
         USAGE:\n\
         \x20 gpn_ceremony init       --circuit <name> --out <crs.bin> [--record <init.json>] [--seeded-from <public>] [--contributor <name>]\n\
         \x20 gpn_ceremony contribute --in <crs.bin> --out <crs.bin> --record <contribution.json> [--seeded-from <public>] [--contributor <name>]\n\
         \x20 gpn_ceremony verify     --crs <crs.bin> --records <r1.json,r2.json,...>\n\
         \x20 gpn_ceremony verify-chain --crs <crs0.bin,crs1.bin,...> --records <r0.json,r1.json,...>\n\
         \x20 gpn_ceremony export-vk  --crs <crs.bin> --out <vk.bin>\n\n\
         CIRCUITS: value-balance, note-spend\n\n\
         verify       : hash-chain + final-digest checks (self-consistency only)\n\
         verify-chain : ALSO recomputes every publicly-seeded step (use this for\n\
         \x20              a real ceremony; OS-entropy steps are rejected because\n\
         \x20              they cannot be reproduced from public data)"
    );
}

fn cmd_init(opts: &HashMap<String, String>) {
    let circuit = req(opts, "circuit");
    let out = req(opts, "out");
    let contributor = opts
        .get("contributor")
        .cloned()
        .unwrap_or_else(|| "genesis".into());
    let seeded = opts.get("seeded-from").map(|s| s.as_str());
    let seeded_label = seeded
        .map(|s| s.to_string())
        .unwrap_or_else(|| "os-entropy".into());

    // The genesis seed is toxic waste — keep it in a wiping buffer.
    let seed = Zeroizing::new(match genesis_seed(&circuit, seeded) {
        Ok(s) => s,
        Err(e) => fail(&e),
    });
    let crs = match ceremony::CeremonyCrs::genesis(&circuit, &seed) {
        Ok(c) => c,
        Err(e) => fail(&e),
    };
    if let Err(e) = crs.check_structure() {
        fail(&format!("internal: bad genesis CRS: {e}"));
    }
    write_file(&out, &crs.to_bytes());
    println!("init: circuit={circuit}");
    println!("init: digest={}", crs.digest_hex());
    println!("init: wrote CRS -> {out}");
    if let Some(rec) = opts.get("record") {
        let r = ContributionRecord::init(&circuit, crs.digest_hex(), &contributor, &seeded_label);
        write_file(rec, r.to_json().as_bytes());
        println!("init: wrote record -> {rec}");
    }
}

fn cmd_contribute(opts: &HashMap<String, String>) {
    let input = req(opts, "in");
    let out = req(opts, "out");
    let record = req(opts, "record");
    let contributor = opts
        .get("contributor")
        .cloned()
        .unwrap_or_else(|| "anon".into());
    let seeded = opts.get("seeded-from").map(|s| s.as_str());
    let seeded_label = seeded
        .map(|s| s.to_string())
        .unwrap_or_else(|| "os-entropy".into());

    let crs = read_crs(&input);
    let entropy = Zeroizing::new(match contribution_entropy(seeded) {
        Ok(e) => e,
        Err(e) => fail(&e),
    });
    let fresh = FreshSecrets::from_entropy(&entropy[..]);
    let new = crs.contribute(&fresh); // fresh is wiped on drop (end of scope)
    drop(fresh);

    if let Err(e) = new.check_structure() {
        fail(&format!("contribution produced an invalid CRS: {e}"));
    }
    write_file(&out, &new.to_bytes());
    let r = ContributionRecord::contribute(
        &new.circuit,
        crs.digest_hex(),
        new.digest_hex(),
        &contributor,
        &seeded_label,
    );
    write_file(&record, r.to_json().as_bytes());
    println!("contribute: {input} -> {out}");
    println!("contribute: parent={}", crs.digest_hex());
    println!("contribute: digest={}", new.digest_hex());
    println!("contribute: contributor={contributor} seeded_from={seeded_label}");
    println!("contribute: toxic waste wiped from memory");
}

fn read_records(spec: &str) -> Vec<ContributionRecord> {
    let mut records = Vec::new();
    for path in spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => fail(&format!("read record {path}: {e}")),
        };
        match ContributionRecord::from_json(&text) {
            Ok(r) => records.push(r),
            Err(e) => fail(&format!("record {path}: {e}")),
        }
    }
    records
}

fn cmd_verify(opts: &HashMap<String, String>) {
    let crs_path = req(opts, "crs");
    let records = read_records(&req(opts, "records"));
    let crs = read_crs(&crs_path);
    match ceremony::verify(&crs, &records) {
        Ok(()) => {
            println!("PASS");
            println!("  circuit      = {}", crs.circuit);
            println!("  contributions= {}", records.len());
            println!("  final digest = {}", crs.digest_hex());
            println!("  note: self-consistency only — use `verify-chain` to also recompute steps");
        }
        Err(e) => {
            println!("FAIL");
            println!("  {e}");
            exit(1);
        }
    }
}

fn cmd_verify_chain(opts: &HashMap<String, String>) {
    let crs_spec = req(opts, "crs");
    let mut chain = Vec::new();
    for path in crs_spec
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        chain.push(read_crs(path));
    }
    let records = read_records(&req(opts, "records"));
    match ceremony::verify_chain(&chain, &records) {
        Ok(()) => {
            println!("PASS (recomputed)");
            println!("  circuit      = {}", chain.last().unwrap().circuit);
            println!("  steps        = {}", records.len());
            println!("  final digest = {}", chain.last().unwrap().digest_hex());
        }
        Err(e) => {
            println!("FAIL");
            println!("  {e}");
            exit(1);
        }
    }
}

fn cmd_export_vk(opts: &HashMap<String, String>) {
    let crs_path = req(opts, "crs");
    let out = req(opts, "out");
    let crs = read_crs(&crs_path);
    if let Err(e) = crs.check_structure() {
        fail(&format!("CRS is not well-formed: {e}"));
    }
    let vk = crs.verifying_key();
    write_file(&out, &ceremony::vk_to_bytes(&vk));
    println!("export-vk: circuit={}", crs.circuit);
    println!("export-vk: gamma_abc entries={}", vk.gamma_abc_g1.len());
    println!("export-vk: wrote VK -> {out}");
}

// ---------------------------------------------------------------------------

fn read_crs(path: &str) -> ceremony::CeremonyCrs {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => fail(&format!("read CRS {path}: {e}")),
    };
    match ceremony::CeremonyCrs::from_bytes(&bytes) {
        Ok(c) => c,
        Err(e) => fail(&format!("parse CRS {path}: {e}")),
    }
}

fn write_file(path: &str, bytes: &[u8]) {
    if let Err(e) = std::fs::write(path, bytes) {
        fail(&format!("write {path}: {e}"));
    }
}

fn parse_opts(args: &[String]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(key) = a.strip_prefix("--") {
            let val = args.get(i + 1).cloned().unwrap_or_default();
            m.insert(key.to_string(), val);
            i += 2;
        } else {
            i += 1;
        }
    }
    m
}

fn req(opts: &HashMap<String, String>, key: &str) -> String {
    match opts.get(key) {
        Some(v) if !v.is_empty() => v.clone(),
        _ => fail(&format!("missing required --{key}")),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1);
}
