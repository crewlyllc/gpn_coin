//! Contract: the seed list in the shipped launcher and the seed list the
//! updater may rewrite it *to* are the same list.
//!
//! `gpn_update --apply` can now repair a stale `start-node.cmd`, which means
//! there are two places that decide what seeds a box dials:
//!
//! * `install.ps1`'s `$Boots` default — what a fresh install writes;
//! * `chainspec::launcher_bootnodes()` — what the updater refreshes an existing
//!   launcher to.
//!
//! If those drift apart, the updater becomes a *downgrade*: a box repaired after
//! a seed change would be reset to the older list, and nothing would say so.
//! So they are pinned together here, on every `cargo test`, from the sources
//! themselves — the same shape as `tests/argv_contract.rs`, which pins the
//! supervisor's child flags to the children that parse them.
//!
//! A seed change now has to be made in one place and proven in the other, or the
//! build fails.

use std::collections::BTreeSet;

fn repo_root() -> std::path::PathBuf {
    // …/gpn-agent/tests/launcher_bootnodes.rs → repo root
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("gpn-agent sits inside the repo")
        .to_path_buf()
}

/// The `$Boots` default literal out of `install.ps1`'s canonical-defaults block.
fn installer_boots() -> Option<String> {
    let ps1 = std::fs::read_to_string(repo_root().join("install.ps1")).ok()?;
    let start = ps1.find("$Boots  = if ($env:GPN_BOOTNODES)")?;
    let rest = &ps1[start..];
    // The default is the single-quoted multiaddr list inside the else branch.
    let open = rest.find('\'')? + 1;
    let close = rest[open..].find('\'')?;
    Some(rest[open..open + close].to_string())
}

fn set_of(list: &str) -> BTreeSet<String> {
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn installer_and_updater_pin_the_same_seed_list() {
    let Some(boots) = installer_boots() else {
        // CI checks out the whole repo; if install.ps1 is missing this is an
        // incomplete checkout, not a contract violation. Say so loudly rather
        // than pass silently.
        eprintln!("SKIP: install.ps1 not found next to gpn-agent");
        return;
    };

    let from_binary = gpn_agent::chainspec::launcher_bootnodes();
    let want: BTreeSet<String> = from_binary.iter().cloned().collect();
    let have = set_of(&boots);

    let missing: Vec<&String> = want.difference(&have).collect();
    let extra: Vec<&String> = have.difference(&want).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "install.ps1 and chainspec::launcher_bootnodes() disagree about the seed list.\n\
         install.ps1 is missing: {missing:#?}\n\
         install.ps1 has extra: {extra:#?}\n\
         A box repaired by gpn_update must not be reset to a retired seed list — \
         update both (and CANONICAL_BOOTNODES if a seed address really changed)."
    );
}

#[test]
fn the_launcher_list_never_serves_a_private_address() {
    // `launcher_bootnodes()` is a superset of `CANONICAL_BOOTNODES` because it
    // also carries the same-LAN seed. That entry must stay out of the list the
    // chain spec serves: `is_dialable_bootnode` rejects private hosts, and a
    // spec that dials one strands every fresh install (2026-10-30).
    let launcher = gpn_agent::chainspec::launcher_bootnodes();
    // 2026-11-10: the LAN seed must NOT be shipped -- a private address only
    // means something on the network that owns it (box 1 runs on its own
    // 192.168.1.0/24 and dialled a stranger's device).
    assert!(!launcher.contains(&gpn_agent::chainspec::LAN_SEED_BOOTNODE.to_string()));
    for c in gpn_agent::chainspec::canonical_bootnodes() {
        assert!(launcher.contains(&c), "canonical entry dropped: {c}");
        assert!(gpn_agent::chainspec::is_dialable_bootnode(&c));
    }
    assert!(!gpn_agent::chainspec::is_dialable_bootnode(
        gpn_agent::chainspec::LAN_SEED_BOOTNODE
    ));
}

#[test]
fn a_stale_installer_list_is_actually_rewritten() {
    // End-to-end on the real numbers: take the installer's own default, remove
    // one entry the way an older installer would have, and prove the updater
    // puts the current list back (rather than reporting "nothing to do").
    let Some(boots) = installer_boots() else {
        eprintln!("SKIP: install.ps1 not found next to gpn-agent");
        return;
    };
    let canonical = gpn_agent::chainspec::launcher_bootnodes();
    let mut stale: Vec<String> = split(&boots);
    stale.pop();
    // Same peer id as a live canonical seed, different address: "ours, but
    // stale". A *foreign* peer id is deliberately left alone by the updater
    // (operator-supplied list), so it cannot exercise this path.
    stale.push(
        "/ip4/203.0.113.9/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"
            .into(),
    );
    let body = format!(
        "@echo off\r\nset \"GPN_BOOTNODES={}\"\r\nset \"GPN_RESERVED_PEERS={}\"\r\n",
        stale.join(","),
        stale.join(",")
    );

    let (out, notes) = gpn_agent::update::refresh_launcher_body(&body, &canonical);
    assert_eq!(notes.len(), 2, "both lists: {notes:?}");
    assert!(!out.contains("203.0.113.9"));
    let want = canonical.join(",");
    assert_eq!(out.matches(want.as_str()).count(), 2);
}

fn split(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The launcher template as `install.ps1` actually writes it.
fn installer_launcher_template() -> Option<String> {
    let ps1 = std::fs::read_to_string(repo_root().join("install.ps1")).ok()?;
    let start = ps1.find("$LauncherTemplate = @'")? + "$LauncherTemplate = @'".len();
    let rest = &ps1[start..];
    let end = rest.find("\n'@")?;
    Some(rest[..end].to_string())
}

#[test]
fn the_real_installer_launcher_is_repaired_without_collateral_edits() {
    // The unit tests use a hand-written fixture; this one uses the template the
    // installer really writes, so a change to that template shows up here.
    let Some(template) = installer_launcher_template() else {
        eprintln!("SKIP: launcher template not found in install.ps1");
        return;
    };
    let canonical = gpn_agent::chainspec::launcher_bootnodes();

    // Substitute the placeholders the way Write-Launcher does, but with an
    // OLD seed list — exactly the file a box installed before the last seed
    // change is still running.
    let stale = "/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/203.0.113.9/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
    let body = template
        .replace("__INSTALL_DIR__", "C:\\gpn")
        .replace("__AP_URL__", "http://gpn-testnet.duckdns.org:8092")
        .replace("__BOOTNODES__", stale)
        .replace("__LOG__", "C:\\gpn\\node.log")
        .replace("__REPORT__", "C:\\gpn\\report-exit.ps1");

    let (out, notes) = gpn_agent::update::refresh_launcher_body(&body, &canonical);
    assert_eq!(notes.len(), 2, "notes: {notes:?}");

    // Walk both files line by line: the only lines that may differ are the two
    // seed lists. Anything else means the repair is rewriting an operator's
    // launcher behind their back.
    let before: Vec<&str> = body.lines().collect();
    let after: Vec<&str> = out.lines().collect();
    assert_eq!(before.len(), after.len(), "line count must not change");
    let changed: Vec<(usize, &str)> = before
        .iter()
        .zip(after.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (_, b))| (i + 1, *b))
        .collect();
    assert_eq!(changed.len(), 2, "changed lines: {changed:#?}");
    for (_, line) in &changed {
        let l = line.trim();
        assert!(
            l.starts_with("set \"GPN_BOOTNODES=") || l.starts_with("set \"GPN_RESERVED_PEERS="),
            "unexpected edit: {line}"
        );
        assert!(!l.contains("203.0.113.9"));
    }
    // Everything the launcher needs to start a node is still there.
    for must in [
        "--ensure-node-key",
        "--ensure-wallet",
        "gpn_node.exe\" >> ",
        "GPN_AP_URL=http://gpn-testnet.duckdns.org:8092",
        "GPN_GUI=1",
    ] {
        assert!(out.contains(must), "launcher lost {must}");
    }
    assert!(!out.contains("__BOOTNODES__"), "placeholder survived");
}
