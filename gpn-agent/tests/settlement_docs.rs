//! #240 — the settlement contract is DEPTH, and every surface that talks about
//! `finalized` must say so.
//!
//! This chain has no finality gadget: `chain_getFinalizedHead` answers with the
//! genesis hash forever. Publishing "finalized" without that warning is the
//! same failure class as every other "publish a fallback as a fact" bug in this
//! repo — the reader is told a block is irreversible when it is not.
//!
//! The acceptance is textual on purpose: a warning nobody ships is not a
//! warning, so the test reads the files that actually get deployed.

use std::path::PathBuf;

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("gpn-agent has a parent")
        .join(rel)
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_path(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The exact warning sentence, verbatim, in every surface that mentions
/// `finalized`. Text is deliberately stable: this string is the contract.
const MARKER: &str = "returns genesis; use depth";

#[test]
fn every_finalized_surface_carries_the_warning() {
    // Explorer / directory page, the API doc page, and both operator guides.
    for rel in [
        "website/nodes.html",
        "website/docs/technical-overview.html",
        "website/docs/run-a-node.html",
        "docs/run-a-node.md",
    ] {
        let text = read(rel);
        assert!(
            text.contains(MARKER),
            "{rel} talks about finality but does not carry the warning \
             {MARKER:?} — a caller there cannot learn that `finalized` is genesis"
        );
    }
}

#[test]
fn network_json_publishes_the_settlement_depth() {
    let text = read("network.json");
    let v: serde_json::Value = serde_json::from_str(&text).expect("network.json parses");
    assert_eq!(
        v["settlement_depth"], 288,
        "network.json must publish settlement_depth: 288"
    );
    assert_eq!(v["settlement_time_secs"], 288 * 15);
    assert!(
        v.get("finalized_head").is_none(),
        "network.json is a static file; it must not pretend to know a finalized head"
    );
}

/// #241 — the GRANDPA revisit condition must live in `docs/` and be reachable
/// from the settlement design, so a future proposal is answered point by point
/// instead of by recollection. This is textual for the same reason as above:
/// a condition recorded nowhere is a mood.
#[test]
fn finality_revisit_conditions_are_recorded_and_referenced() {
    let doc = read("docs/finality-revisit-condition.md");
    for required in [
        "**≥ 7 operators running validating keys that Jeremy does not hold",
        "**Who can add or remove a validator",
        "**The network keeps producing and finalising with any one operator gone.**",
        "**Something notices when finality stops advancing",
        "verdict:             ADOPT / REFUSE",
    ] {
        assert!(
            doc.contains(required),
            "docs/finality-revisit-condition.md must carry {required:?} — \
             a revisit condition that omits it can be argued around"
        );
    }

    let design = read("docs/design-settlement-finality.md");
    assert!(
        design.contains("finality-revisit-condition.md"),
        "the settlement design must link docs/finality-revisit-condition.md, or the \
         conditions are not reachable from the decision they govern"
    );
}

#[test]
fn operator_guide_shows_how_to_check_288_deep() {
    let text = read("docs/run-a-node.md");
    // A number-of-confirmations check, not a boolean. Both halves must be
    // present: the depth constant and a command that compares against it.
    assert!(
        text.contains("288") && text.contains("chain_getHeader"),
        "docs/run-a-node.md must show a curl example that checks a block is 288 deep"
    );
    assert!(
        text.contains("settled"),
        "the example must state the settled/not-settled verdict"
    );
}
