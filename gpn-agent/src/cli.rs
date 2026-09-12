//! Shared CLI argument-parsing helpers for the `gpn-agent` binaries.
//!
//! These turn bad user input into clean `Result` errors instead of panicking,
//! so every binary can print a helpful message and exit non-zero — never abort
//! with a backtrace on a malformed hex blob or a missing flag value.
//!
//! The helpers return `Result<T, String>`; both `Box<dyn Error>` and
//! `anyhow::Error` implement `From<String>`, so binaries whose `main` returns
//! one of those can simply use `?`.

use std::fmt::Display;
use std::str::FromStr;

/// A CLI argument error.
///
/// Its `Debug` impl prints the bare message (no surrounding quotes), so a
/// binary whose `main` returns `Result<_, Box<dyn Error>>` reports bad input
/// as `Error: <message>` rather than `Error: "<message>"`.
#[derive(Clone, PartialEq, Eq)]
pub struct ArgError(String);

impl ArgError {
    /// The underlying human-readable message.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ArgError {}

impl From<String> for ArgError {
    fn from(s: String) -> Self {
        ArgError(s)
    }
}

/// Decode a fixed-length hex argument (e.g. a 32-byte hash) into `[u8; N]`.
///
/// Accepts an optional `0x` prefix and ignores interior/leading whitespace.
/// Returns a descriptive error (rather than panicking) when the input is empty,
/// not valid hex, or does not decode to exactly `N` bytes.
pub fn parse_hex_fixed<const N: usize>(raw: &str) -> Result<[u8; N], ArgError> {
    let clean: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let clean = clean.trim_start_matches("0x");
    if clean.is_empty() {
        return Err(ArgError(format!(
            "expected {N} bytes of hex, got an empty value"
        )));
    }
    let bytes = hex::decode(clean).map_err(|e| ArgError(format!("invalid hex '{raw}': {e}")))?;
    if bytes.len() != N {
        return Err(ArgError(format!(
            "invalid hex '{raw}': expected {N} bytes ({} hex chars), got {} bytes",
            N * 2,
            bytes.len()
        )));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Parse a required numeric/flag value, naming the flag in any error message.
pub fn parse_num<T>(name: &str, raw: &str) -> Result<T, ArgError>
where
    T: FromStr,
    T::Err: Display,
{
    raw.parse::<T>()
        .map_err(|e| ArgError(format!("invalid value for {name}: '{raw}' ({e})")))
}

/// Parse an optional numeric/flag value.
///
/// Returns `default` when the argument is absent, but — unlike a silent
/// `unwrap_or(default)` — returns an error when the argument *was* supplied
/// with an unparseable value.
pub fn opt_num<T>(name: &str, raw: Option<&str>, default: T) -> Result<T, ArgError>
where
    T: FromStr,
    T::Err: Display,
{
    match raw {
        Some(s) => parse_num(name, s),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_fixed_accepts_plain_and_prefixed() {
        let plain = "11".repeat(32);
        let a = parse_hex_fixed::<32>(&plain).unwrap();
        assert_eq!(a, [0x11u8; 32]);
        let b = parse_hex_fixed::<32>(&format!("0x{plain}")).unwrap();
        assert_eq!(a, b);
        // Interior whitespace is tolerated.
        let c = parse_hex_fixed::<32>(&format!(" 0x{} ", plain)).unwrap();
        assert_eq!(a, c);
    }

    #[test]
    fn hex_fixed_rejects_bad_input_without_panicking() {
        assert!(parse_hex_fixed::<32>("").is_err());
        assert!(parse_hex_fixed::<32>("0x").is_err());
        assert!(parse_hex_fixed::<32>("xyz").is_err());
        // Right hex, wrong length.
        assert!(parse_hex_fixed::<32>(&"ab".repeat(31)).is_err());
        assert!(parse_hex_fixed::<32>(&"ab".repeat(33)).is_err());
        // Odd number of hex chars.
        assert!(parse_hex_fixed::<32>("abc").is_err());
    }

    #[test]
    fn parse_num_reports_flag_name() {
        let v: u64 = parse_num("--iters", "42").unwrap();
        assert_eq!(v, 42);
        let e = parse_num::<u64>("--iters", "not-a-number").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("--iters"), "error should name the flag: {msg}");
        assert!(
            msg.contains("not-a-number"),
            "error should quote the value: {msg}"
        );
        // Debug must print the bare message (no surrounding quotes).
        assert_eq!(format!("{e:?}"), msg);
    }

    #[test]
    fn opt_num_defaults_only_when_absent() {
        let d: u64 = opt_num("--iters", None, 8).unwrap();
        assert_eq!(d, 8);
        let p: u64 = opt_num("--iters", Some("3"), 8).unwrap();
        assert_eq!(p, 3);
        // Present but invalid must error, never silently fall back to default.
        assert!(opt_num::<u64>("--iters", Some("oops"), 8).is_err());
    }
}
