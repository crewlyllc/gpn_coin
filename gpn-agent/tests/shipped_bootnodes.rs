//! One class test over **every** surface that ships a default bootstrap list.
//!
//! `23.241.109.255` (peer `12D3KooWFqZ2…`) died on 2026-11-13 and had to be
//! removed from six different hand-maintained copies: the chain spec constant,
//! the shared spec artifact, and four installers — plus `scripts/run-windows.cmd`,
//! which the first sweep missed. Each previous fix repaired *one copy*; the next
//! sweep found the next one (D5 → D10 → D11). A copy that is not asserted here
//! is a copy that will rot, so this test derives the truth from `network.json`
//! (the registry the site and the installers are supposed to mirror) and fails
//! on any default list that disagrees with it, in either direction.
//!
//! Deliberately excluded: comments, docs, example configs and test fixtures.
//! They make no bootstrap claim; only the values a fresh install actually dials
//! are in scope.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // …/gpn-agent/tests/shipped_bootnodes.rs → repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("gpn-agent sits inside the repo")
        .to_path_buf()
}

fn read(rel: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(rel)).ok()
}

/// Line endings are a checkout artifact, not a shipped value.
///
/// Git for Windows checks these files out with CRLF (`core.autocrlf=true` is the
/// default there), so a marker written as `"\n}\n"` never matches and the Windows
/// runner fails the same assertion the Linux runner passes — that is exactly how
/// this test reddened CI on `14b797a` while the Linux job was green. The values are
/// what is under test, so normalise before parsing and assert on values only.
fn normalise_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The registry list every shipped default is supposed to mirror.
fn network_bootnodes() -> BTreeSet<String> {
    let raw = read("network.json").expect("network.json must exist next to gpn-agent");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("network.json is valid JSON");
    v["bootnodes"]
        .as_array()
        .expect("network.json has a `bootnodes` array")
        .iter()
        .map(|b| b.as_str().expect("bootnodes are strings").to_string())
        .collect()
}

struct Surface {
    file: &'static str,
    /// Text that begins the shipped default list.
    start: &'static str,
    /// Text that ends it (searched after `start`).
    end: &'static str,
}

/// Every file that hands a fresh install its bootstrap peers.
const SURFACES: &[Surface] = &[
    Surface {
        file: "install.sh",
        start: "GPN_BOOTNODES=\"${GPN_BOOTNODES:-",
        end: "}\"",
    },
    Surface {
        file: "scripts/gpn-autostart.sh",
        start: "GPN_BOOTNODES=\"${GPN_BOOTNODES:-",
        end: "}\"",
    },
    Surface {
        file: "install.ps1",
        start: "$Boots  = if ($env:GPN_BOOTNODES)",
        end: "\n}\n",
    },
    Surface {
        file: "run-windows-provider.ps1",
        start: "$env:GPN_BOOTNODES = (",
        end: "\n  )",
    },
    Surface {
        file: "scripts/run-windows.cmd",
        start: "set \"GPN_BOOTNODES=",
        end: "\"",
    },
];

/// Extract every multiaddr from a slice of source text.
///
/// Splits on the punctuation these lists use (quotes, commas, `+`, braces,
/// parens, whitespace) so one parser covers shell, PowerShell and batch.
fn multiaddrs(block: &str) -> Vec<String> {
    const PREFIXES: [&str; 4] = ["/dns4/", "/dns6/", "/ip4/", "/ip6/"];
    let mut out = Vec::new();
    let mut i = 0;
    while i < block.len() {
        if !block.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if !PREFIXES.iter().any(|p| block[i..].starts_with(p)) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i;
        while j < block.len() {
            if !block.is_char_boundary(j) {
                break;
            }
            let c = block[j..].chars().next().expect("in bounds");
            if matches!(c, '"' | '\'' | ',' | ')' | '}' | '+' | '\r' | '\n') || c.is_whitespace() {
                break;
            }
            j += c.len_utf8();
        }
        out.push(block[start..j].to_string());
        i = if j > i { j } else { i + 1 };
    }
    out
}

fn default_list(surface: &Surface) -> Option<Vec<String>> {
    let text = read(surface.file)?;
    Some(list_in(&text, surface))
}

/// Parse one surface out of the given file text (line endings normalised here, so
/// every caller — and every future caller — is CRLF-safe by construction).
fn list_in(text: &str, surface: &Surface) -> Vec<String> {
    let text = normalise_newlines(text);
    let at = text.find(surface.start).unwrap_or_else(|| {
        panic!(
            "{} no longer contains its seed-list marker {:?} — update this test with the new shape, \
             do not delete the assertion",
            surface.file, surface.start
        )
    });
    let rest = &text[at + surface.start.len()..];
    let end = rest.find(surface.end).unwrap_or_else(|| {
        panic!(
            "{}: seed block started at {:?} but never ended with {:?}",
            surface.file, surface.start, surface.end
        )
    });
    multiaddrs(&rest[..end])
}

/// A CRLF checkout must extract the same peers as an LF one.
///
/// Regression for the `14b797a` Windows-only CI red: the end marker for
/// `install.ps1` is a LF shape, so `"\n}\n"` was never found in a CRLF checkout and
/// the test panicked before it compared anything. The Windows runner is the only
/// place this shows up, so it needs a test that fails without the normalisation on
/// any platform.
#[test]
fn a_crlf_checkout_parses_to_the_same_lists() {
    let mut seen = 0;
    for surface in SURFACES {
        let Some(lf) = read(surface.file) else {
            continue;
        };
        let crlf = lf.replace('\n', "\r\n");
        assert!(
            crlf.contains('\r'),
            "{}: could not build a CRLF variant",
            surface.file
        );
        assert_eq!(
            list_in(&crlf, surface),
            list_in(&lf, surface),
            "{} parses differently with CRLF line endings (Windows checkout)",
            surface.file
        );
        seen += 1;
    }
    assert!(
        seen >= 2,
        "only {seen} surface(s) seen — not the tree this test expects"
    );
}

#[test]
fn every_shipped_default_bootnode_list_matches_network_json() {
    let want = network_bootnodes();
    assert!(
        !want.is_empty(),
        "network.json has no bootnodes — a fresh install would have nothing to dial"
    );
    assert!(want
        .iter()
        .all(|b| gpn_agent::chainspec::is_dialable_bootnode(b)));

    let mut checked = 0;
    let mut failures = Vec::new();
    for surface in SURFACES {
        let Some(have) = default_list(surface) else {
            // CI checks out the whole repo; a missing file is an incomplete
            // checkout, not a contract violation. Say so rather than pass mute.
            eprintln!("SKIP: {} not found", surface.file);
            continue;
        };
        checked += 1;
        let have: BTreeSet<String> = have.into_iter().collect();
        if have != want {
            let missing: Vec<&String> = want.difference(&have).collect();
            let extra: Vec<&String> = have.difference(&want).collect();
            failures.push(format!(
                "{}: missing {missing:?}, extra {extra:?} relative to network.json",
                surface.file
            ));
        }
    }
    assert!(
        checked >= 2,
        "only {checked} surface(s) checked — this test is not seeing the tree it thinks it is"
    );
    assert!(
        failures.is_empty(),
        "a shipped default bootstrap list disagrees with network.json:\n  {}\n\
         Every copy of the seed list must move together (D5 → D10 → D11: three \
         fixes, three missed copies).",
        failures.join("\n  ")
    );
}

#[test]
fn the_binary_constant_matches_network_json() {
    let want = network_bootnodes();
    let have: BTreeSet<String> = gpn_agent::chainspec::canonical_bootnodes()
        .into_iter()
        .collect();
    let missing: Vec<&String> = want.difference(&have).collect();
    let extra: Vec<&String> = have.difference(&want).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "chainspec::CANONICAL_BOOTNODES disagrees with network.json\n\
         missing: {missing:#?}\nextra: {extra:#?}"
    );
    // The DNS name is the one entry that survives a seed IP change.
    assert_eq!(
        gpn_agent::chainspec::launcher_bootnodes()
            .first()
            .map(String::as_str),
        gpn_agent::chainspec::CANONICAL_BOOTNODES.first().copied(),
        "the launcher list must start with the canonical (DNS) entry"
    );
}

/// The shared chain-spec artifact, when this host has it (same skip rule the
/// chainspec unit test uses). This is the copy D8 taught us moves with the
/// constant or not at all.
#[test]
fn the_shared_spec_artifact_matches_network_json() {
    let path = std::env::var("GPN_SPEC_PATH").unwrap_or_else(|_| {
        "/home/jeremyg/shared_workspace/artifacts/project7/gpn-testnet-spec.json".to_string()
    });
    let Ok(raw) = std::fs::read_to_string(&path) else {
        eprintln!("skipping: {path} not present");
        return;
    };
    let v: serde_json::Value = serde_json::from_str(&raw).expect("spec artifact is JSON");
    let have: BTreeSet<String> = v["bootNodes"]
        .as_array()
        .expect("spec artifact has `bootNodes`")
        .iter()
        .map(|b| b.as_str().expect("bootNodes are strings").to_string())
        .collect();
    let want = network_bootnodes();
    let missing: Vec<&String> = want.difference(&have).collect();
    let extra: Vec<&String> = have.difference(&want).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{path} disagrees with network.json\nmissing: {missing:#?}\nextra: {extra:#?}"
    );
}
