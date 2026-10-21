//! First-boot behaviour of the access point's bearer token.
//!
//! `gpn_agent::ap_token` resolves the token, generates one when neither the
//! environment nor the env file has it, persists it at mode 0600 without
//! clobbering the file's other variables, and prints the full value on exactly
//! one boot. Every assertion below goes through the same public API the AP
//! binary uses.

use std::path::{Path, PathBuf};

use gpn_agent::ap_token::{self, Source, TOKEN_LEN};

fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("gpn-ap-token-it-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_file(dir: &Path) -> PathBuf {
    dir.join("accesspoint.env")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn token_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("GPN_AP_TOKEN=").map(str::to_string))
        .collect()
}

fn assert_hex48(t: &str) {
    assert_eq!(t.len(), TOKEN_LEN, "token {t:?} is not 48 chars");
    assert!(
        t.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "token {t:?} is not lowercase hex"
    );
}

#[cfg(unix)]
fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn joined(lines: &[String]) -> String {
    lines.join("\n")
}

// ── 1. generates when absent ────────────────────────────────────────────────

#[test]
fn generates_when_absent() {
    let dir = scratch("gen");
    let path = env_file(&dir);

    let r = ap_token::resolve_with(None, &path);
    assert_eq!(r.source, Source::Generated);
    assert!(
        r.persist_error.is_none(),
        "persist failed: {:?}",
        r.persist_error
    );
    assert_hex48(&r.token);
    assert!(path.exists(), "env file was not created");

    let text = read(&path);
    assert_eq!(token_lines(&text), vec![r.token.clone()]);
}

#[test]
fn generates_when_env_is_empty_string() {
    let dir = scratch("empty-env");
    let r = ap_token::resolve_with(Some(""), &env_file(&dir));
    assert_eq!(r.source, Source::Generated);
    assert_hex48(&r.token);
}

#[test]
fn generates_when_file_has_empty_or_partial_token() {
    for body in [
        "GPN_AP_TOKEN=\n",
        "GPN_AP_TOKEN=   \n",
        "GPN_AP_TOKEN\n",
        "GPN_AP_TOK=zz\n",
    ] {
        let dir = scratch("partial");
        let path = env_file(&dir);
        std::fs::write(&path, body).unwrap();

        let r = ap_token::resolve_with(None, &path);
        assert_eq!(r.source, Source::Generated, "body {body:?}");
        assert_hex48(&r.token);

        let lines = token_lines(&read(&path));
        assert_eq!(lines.len(), 1, "body {body:?} left {lines:?}");
        assert_eq!(lines[0], r.token);
    }
}

// ── 2. persistence: mode 0600, dir created, atomic ──────────────────────────

#[cfg(unix)]
#[test]
fn persists_at_mode_0600_and_creates_dir() {
    let root = scratch("mode");
    let dir = root.join("nested/gpn"); // does not exist yet
    let path = dir.join("accesspoint.env");

    let r = ap_token::resolve_with(None, &path);
    assert!(r.persist_error.is_none());
    assert_eq!(mode_of(&path), 0o600);
    assert_eq!(mode_of(&dir), 0o700, "config dir should be private");

    // No temp file left behind on the happy path.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

#[cfg(unix)]
#[test]
fn tightens_mode_of_a_loose_existing_file() {
    let dir = scratch("loose");
    let path = env_file(&dir);
    std::fs::write(&path, "GPN_LISTEN=0.0.0.0:8092\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let r = ap_token::resolve_with(None, &path);
    assert_eq!(r.source, Source::Generated);
    assert_eq!(mode_of(&path), 0o600);
}

// ── 3. other variables preserved ────────────────────────────────────────────

#[test]
fn other_variables_are_preserved() {
    let dir = scratch("preserve");
    let path = env_file(&dir);
    let original = "# GPN access-point config\n\
                    GPN_ENDPOINT=ws://127.0.0.1:9944\n\
                    GPN_LISTEN=0.0.0.0:8092\n\
                    GPN_PROVIDERS=http://127.0.0.1:8093\n\
                    # GPN_PUBLIC_URL=http://your-host:8092\n";
    std::fs::write(&path, original).unwrap();

    let r = ap_token::resolve_with(None, &path);
    let text = read(&path);

    for line in original.lines() {
        assert!(text.contains(line), "lost line {line:?}:\n{text}");
    }
    assert_eq!(token_lines(&text), vec![r.token]);
}

#[test]
fn appends_to_a_file_with_no_trailing_newline() {
    let dir = scratch("nonl");
    let path = env_file(&dir);
    std::fs::write(&path, "GPN_LISTEN=0.0.0.0:8092").unwrap();

    let r = ap_token::resolve_with(None, &path);
    let text = read(&path);
    assert!(
        text.contains("GPN_LISTEN=0.0.0.0:8092\n"),
        "glued lines:\n{text}"
    );
    assert_eq!(token_lines(&text), vec![r.token]);
}

#[test]
fn duplicate_token_lines_collapse_to_one() {
    let dir = scratch("dupe");
    let path = env_file(&dir);
    std::fs::write(
        &path,
        "GPN_AP_TOKEN=old1\nGPN_LISTEN=x\nGPN_AP_TOKEN=old2\n",
    )
    .unwrap();

    // old2 (last) is the live one — same `tail -1` rule set-ap-token.sh uses.
    let first = ap_token::resolve_with(None, &path);
    assert_eq!(first.source, Source::File);
    assert_eq!(first.token, "old2");

    // Force generation by removing the value; the rewrite leaves exactly one.
    std::fs::write(&path, "GPN_AP_TOKEN=\nGPN_LISTEN=x\nGPN_AP_TOKEN=\n").unwrap();
    let r = ap_token::resolve_with(None, &path);
    assert_eq!(r.source, Source::Generated);
    let lines = token_lines(&read(&path));
    assert_eq!(lines, vec![r.token]);
}

// ── 4. second boot: no regeneration, no full token printed ──────────────────

#[test]
fn second_boot_reuses_the_token_and_never_prints_it() {
    let dir = scratch("second");
    let path = env_file(&dir);

    let first = ap_token::resolve_with(None, &path);
    assert_eq!(first.source, Source::Generated);
    let first_out = joined(&ap_token::startup_lines(&first));
    assert!(
        first_out.contains(&format!("GPN_AP_TOKEN={}", first.token)),
        "first boot must print the full value:\n{first_out}"
    );
    assert!(
        first_out.contains("generated a new AP token"),
        "missing the instruction:\n{first_out}"
    );
    assert!(
        first_out.contains("AP_TOKEN secret on the Cloudflare"),
        "missing the Cloudflare instruction:\n{first_out}"
    );
    assert!(
        first_out.contains("will not be printed again"),
        "missing the once-only warning:\n{first_out}"
    );

    let second = ap_token::resolve_with(None, &path);
    assert_eq!(second.source, Source::File);
    assert_eq!(second.token, first.token, "token changed on restart");

    let second_out = joined(&ap_token::startup_lines(&second));
    assert!(
        !second_out.contains(&second.token),
        "second boot leaked the full token:\n{second_out}"
    );
    assert!(
        second_out.contains(&second.masked()),
        "second boot should log the prefix:\n{second_out}"
    );
    assert!(
        !second_out.contains("generated a new AP token"),
        "second boot claimed to generate:\n{second_out}"
    );
}

#[test]
fn third_boot_still_stable_across_many_resolves() {
    let dir = scratch("many");
    let path = env_file(&dir);
    let first = ap_token::resolve_with(None, &path).token;
    for _ in 0..5 {
        let r = ap_token::resolve_with(None, &path);
        assert_eq!(r.source, Source::File);
        assert_eq!(r.token, first);
    }
}

// ── 5. explicit env wins and is never overwritten ───────────────────────────

#[test]
fn explicit_env_token_wins_and_leaves_the_file_alone() {
    let dir = scratch("envwins");
    let path = env_file(&dir);
    std::fs::write(&path, "GPN_AP_TOKEN=fromfile\nGPN_LISTEN=x\n").unwrap();
    let before = read(&path);

    let r = ap_token::resolve_with(Some("fromenv"), &path);
    assert_eq!(r.source, Source::Env);
    assert_eq!(r.token, "fromenv");
    assert_eq!(read(&path), before, "env-supplied token rewrote the file");

    let out = joined(&ap_token::startup_lines(&r));
    assert!(!out.contains("fromenv"), "leaked the token:\n{out}");
    assert!(
        out.contains("set, 7 chars"),
        "short token should be logged by length only:\n{out}"
    );
}

#[test]
fn explicit_env_wins_even_with_no_file() {
    let dir = scratch("envnofile");
    let path = env_file(&dir);
    let r = ap_token::resolve_with(Some("  spaced-token  "), &path);
    assert_eq!(r.source, Source::Env);
    assert_eq!(r.token, "spaced-token");
    assert!(!path.exists(), "env-supplied token created a file");
}

// ── 6. corrupt / partial file must not crash startup ────────────────────────

#[test]
fn invalid_utf8_file_does_not_crash_and_is_quarantined() {
    let dir = scratch("corrupt");
    let path = env_file(&dir);
    std::fs::write(&path, [0xff, 0xfe, 0x00, b'G', b'P', b'N', 0x80]).unwrap();

    let r = ap_token::resolve_with(None, &path); // must not panic
    assert_eq!(r.source, Source::Generated);
    assert_hex48(&r.token);

    let bak = r.quarantined.expect("corrupt file should be kept aside");
    assert!(bak.exists(), "backup missing: {}", bak.display());
    assert_eq!(
        std::fs::read(&bak).unwrap().len(),
        7,
        "backup is not the original bytes"
    );

    let lines = token_lines(&read(&path));
    assert_eq!(lines, vec![r.token]);
}

#[test]
fn binary_garbage_with_nul_does_not_crash() {
    let dir = scratch("nul");
    let path = env_file(&dir);
    std::fs::write(&path, b"GPN_LISTEN=x\0\0\0garbage").unwrap();
    let r = ap_token::resolve_with(None, &path);
    assert_eq!(r.source, Source::Generated);
    assert_hex48(&r.token);
    assert_eq!(token_lines(&read(&path)), vec![r.token]);
}

#[test]
fn unreadable_path_degrades_to_ephemeral_token() {
    // The path is a directory: reading yields EISDIR (not NotFound), writing
    // the temp file then the rename fails. Startup must still produce a token.
    let dir = scratch("isdir");
    let path = dir.join("accesspoint.env");
    std::fs::create_dir(&path).unwrap();

    let r = ap_token::resolve_with(None, &path);
    assert!(r.persist_error.is_some(), "expected a persist error");
    assert_hex48(&r.token);

    let out = joined(&ap_token::startup_lines(&r));
    assert!(
        out.contains("EPHEMERAL"),
        "should warn about ephemerality:\n{out}"
    );
    assert!(out.contains(&r.token), "still must print it once:\n{out}");
}

#[test]
fn crlf_file_is_handled() {
    let dir = scratch("crlf");
    let path = env_file(&dir);
    std::fs::write(&path, "GPN_AP_TOKEN=abc\r\nGPN_LISTEN=x\r\n").unwrap();

    let r = ap_token::resolve_with(None, &path);
    assert_eq!(r.source, Source::File);
    assert_eq!(r.token, "abc");
}
