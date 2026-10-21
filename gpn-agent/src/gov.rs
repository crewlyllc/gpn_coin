//! Governance-driven runtime upgrade helpers (the **mainnet** path).
//!
//! On mainnet `Sudo` is an inert all-zeros placeholder (D2), so a runtime
//! upgrade cannot be a `Sudo.sudo(System.set_code)` call. The only origin that
//! can reach `System.set_code` is **Root**, and the only way to obtain Root on
//! mainnet is `pallet-gpn-governance` (see `gpn-node/pallets/gpn-governance`):
//!
//! * a fixed committee of `MemberCount` accounts (`Members` storage),
//! * `ApprovalsRequired` distinct member approvals (2 of 3 on mainnet),
//! * `TimelockPeriod` blocks after `created_at` (3 days = 17_280 blocks @15 s),
//! * then *anyone* may call `execute`, which dispatches the stored call with
//!   `RawOrigin::Root`.
//!
//! There is **no** `pallet-democracy` / `pallet-collective` / `pallet-preimage` /
//! `pallet-scheduler` in this runtime, so there are no referenda, no tracks, no
//! fast-track and no preimage indirection: the call (including the full WASM)
//! is stored inline in `GpnGovernance::Proposals`, keyed by
//! `blake2_256(encoded_call)`. That is the "proposal hash" this tool prints and
//! that committee members approve.
//!
//! Everything in this module is pure (no I/O) except [`decompress_blob`] /
//! [`inspect_artifact`], which shell out to the `zstd` binary for substrate's
//! compressed-WASM blob format. Keeping the parsing/maths here makes it all
//! unit-testable without a node.

use sp_core::blake2_256;
use std::fmt;

/// The `:code` well-known storage key (raw).
pub const CODE_KEY_HEX: &str = "0x3a636f6465";

/// Substrate's "compressed blob" magic (`sp_maybe_compressed_blob`): an 8-byte
/// little-endian magic followed by a zstd frame.
pub const COMPRESSED_BLOB_MAGIC: [u8; 8] = [0x52, 0xbc, 0x53, 0x76, 0x46, 0xdb, 0x8e, 0x05];

/// `frame_system` call indices (part of the runtime ABI).
pub const SYSTEM_SET_CODE_INDEX: u8 = 2;
/// `System.set_code_without_checks` — root-only, skips the spec-version gate.
pub const SYSTEM_SET_CODE_WITHOUT_CHECKS_INDEX: u8 = 3;

// ###########################################################################
// Hashing / hex
// ###########################################################################

/// `0x`-prefixed lowercase hex.
pub fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// Parse a 32-byte hash given as `0x…` or bare hex (64 chars).
pub fn parse_hash(s: &str) -> Result<[u8; 32], String> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let raw = hex::decode(t).map_err(|e| format!("'{s}' is not hex: {e}"))?;
    if raw.len() != 32 {
        return Err(format!("'{s}' is {} bytes, expected 32", raw.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&raw);
    Ok(out)
}

/// The governance proposal id: `blake2_256(SCALE(RuntimeCall))`, which is exactly
/// what `pallet-gpn-governance` computes via `T::Hashing::hash_of(call)`.
pub fn proposal_hash(encoded_call: &[u8]) -> [u8; 32] {
    blake2_256(encoded_call)
}

/// `sha256` of an artifact (the digest we publish for releases).
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut res = [0u8; 32];
    res.copy_from_slice(&out);
    res
}

// ###########################################################################
// xxhash / storage keys
// ###########################################################################

const XXH_P1: u64 = 11_400_714_785_074_694_791;
const XXH_P2: u64 = 14_029_467_366_897_019_727;
const XXH_P3: u64 = 1_609_587_929_392_839_161;
const XXH_P4: u64 = 9_650_029_242_287_828_579;
const XXH_P5: u64 = 2_870_177_450_012_600_261;

fn xxh_round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(XXH_P2))
        .rotate_left(31)
        .wrapping_mul(XXH_P1)
}

fn xxh_merge(acc: u64, val: u64) -> u64 {
    (acc ^ xxh_round(0, val))
        .wrapping_mul(XXH_P1)
        .wrapping_add(XXH_P4)
}

/// xxHash64 (seed-able) — the hash `twox128` is built from.
pub fn xxh64(data: &[u8], seed: u64) -> u64 {
    let n = data.len();
    let mut i = 0usize;
    let mut h: u64;
    if n >= 32 {
        let mut v1 = seed.wrapping_add(XXH_P1).wrapping_add(XXH_P2);
        let mut v2 = seed.wrapping_add(XXH_P2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(XXH_P1);
        while i + 32 <= n {
            v1 = xxh_round(v1, u64::from_le_bytes(data[i..i + 8].try_into().unwrap()));
            i += 8;
            v2 = xxh_round(v2, u64::from_le_bytes(data[i..i + 8].try_into().unwrap()));
            i += 8;
            v3 = xxh_round(v3, u64::from_le_bytes(data[i..i + 8].try_into().unwrap()));
            i += 8;
            v4 = xxh_round(v4, u64::from_le_bytes(data[i..i + 8].try_into().unwrap()));
            i += 8;
        }
        h = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        h = xxh_merge(h, v1);
        h = xxh_merge(h, v2);
        h = xxh_merge(h, v3);
        h = xxh_merge(h, v4);
    } else {
        h = seed.wrapping_add(XXH_P5);
    }
    h = h.wrapping_add(n as u64);
    while i + 8 <= n {
        let k = xxh_round(0, u64::from_le_bytes(data[i..i + 8].try_into().unwrap()));
        h ^= k;
        h = h.rotate_left(27).wrapping_mul(XXH_P1).wrapping_add(XXH_P4);
        i += 8;
    }
    if i + 4 <= n {
        h ^= (u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as u64).wrapping_mul(XXH_P1);
        h = h.rotate_left(23).wrapping_mul(XXH_P2).wrapping_add(XXH_P3);
        i += 4;
    }
    while i < n {
        h ^= (data[i] as u64).wrapping_mul(XXH_P5);
        h = h.rotate_left(11).wrapping_mul(XXH_P1);
        i += 1;
    }
    h ^= h >> 33;
    h = h.wrapping_mul(XXH_P2);
    h ^= h >> 29;
    h = h.wrapping_mul(XXH_P3);
    h ^= h >> 32;
    h
}

/// Substrate's `twox_128` (two xxhash64s, seeds 0 and 1, little-endian).
pub fn twox128(data: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&xxh64(data, 0).to_le_bytes());
    out[8..].copy_from_slice(&xxh64(data, 1).to_le_bytes());
    out
}

/// `blake2_128_concat` — the hasher used by `Members`/`Proposals`.
pub fn blake2_128_concat(data: &[u8]) -> Vec<u8> {
    let mut out = sp_core::blake2_128(data).to_vec();
    out.extend_from_slice(data);
    out
}

/// Raw storage key of a `StorageMap` entry whose hasher is `Blake2_256`
/// (no concatenation — the digest *is* the key). `GpnGovernance::Proposals`
/// uses this: the map key is already a 32-byte hash, so the storage key is
/// `twox128(pallet) ++ twox128(item) ++ blake2_256(hash_bytes)`.
pub fn storage_key_map_blake2_256(pallet: &str, item: &str, key: &[u8]) -> String {
    let mut k = Vec::with_capacity(64);
    k.extend_from_slice(&twox128(pallet.as_bytes()));
    k.extend_from_slice(&twox128(item.as_bytes()));
    k.extend_from_slice(&blake2_256(key));
    hex0x(&k)
}

/// Raw storage key of a plain `StorageValue` item.
pub fn storage_key_value(pallet: &str, item: &str) -> String {
    let mut k = Vec::with_capacity(32);
    k.extend_from_slice(&twox128(pallet.as_bytes()));
    k.extend_from_slice(&twox128(item.as_bytes()));
    hex0x(&k)
}

/// Raw storage key of a `StorageMap` entry whose key encodes with
/// `Blake2_128Concat` (what `Proposals` and `Members` use).
pub fn storage_key_map_blake2_128(pallet: &str, item: &str, key: &[u8]) -> String {
    let mut k = Vec::with_capacity(32 + 16 + key.len());
    k.extend_from_slice(&twox128(pallet.as_bytes()));
    k.extend_from_slice(&twox128(item.as_bytes()));
    k.extend_from_slice(&blake2_128_concat(key));
    hex0x(&k)
}

/// The storage *prefix* of a `StorageMap` (for `state_getKeysPaged`).
pub fn storage_prefix(pallet: &str, item: &str) -> String {
    storage_key_value(pallet, item)
}

// ###########################################################################
// WASM / artifact inspection
// ###########################################################################

/// True for a bare WASM module (`\0asm`).
pub fn is_wasm(bytes: &[u8]) -> bool {
    bytes.len() > 4 && &bytes[..4] == b"\0asm"
}

/// True for substrate's compressed runtime blob (`sp_maybe_compressed_blob`).
pub fn is_compressed_blob(bytes: &[u8]) -> bool {
    bytes.len() > 8 && bytes[..8] == COMPRESSED_BLOB_MAGIC
}

/// Decompress a substrate compressed-WASM blob with the `zstd` CLI.
///
/// The blob format is just `magic(8) || zstd_frame`, so `zstd -d` on the tail
/// gives back the exact bytes the runtime originally encoded. We shell out
/// (like `gpn_snapshot` does for `.tar.zst`) so the agent keeps a pure-Rust
/// build with no C dependency.
pub fn decompress_blob(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !is_compressed_blob(bytes) {
        return Err("not a substrate compressed runtime blob (bad magic)".into());
    }
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("zstd")
        .args(["-d", "-c", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `zstd` (needed to inspect the blob): {e}"))?;
    {
        let stdin = child.stdin.as_mut().ok_or("zstd stdin unavailable")?;
        stdin
            .write_all(&bytes[8..])
            .map_err(|e| format!("zstd stdin write failed: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("zstd failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "zstd decompress failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// The fields of `sp_version::RuntimeVersion` this tool needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactVersion {
    pub spec_name: String,
    pub impl_name: String,
    pub authoring_version: u32,
    pub spec_version: u32,
    pub impl_version: u32,
    pub transaction_version: u32,
    /// `system_version` is a `u8` in this polkadot-sdk rev (it was `state_version`).
    pub system_version: u8,
}

impl fmt::Display for ArtifactVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "spec_name={} spec_version={} impl_version={} tx_version={} system_version={}",
            self.spec_name,
            self.spec_version,
            self.impl_version,
            self.transaction_version,
            self.system_version
        )
    }
}

/// Read a SCALE compact-encoded `u32` at `*i`, advancing the cursor.
pub fn decode_compact_u32(b: &[u8], i: &mut usize) -> Result<u32, String> {
    let first = *b.get(*i).ok_or("compact: out of bounds")?;
    match first & 0b11 {
        0 => {
            *i += 1;
            Ok((first as u32) >> 2)
        }
        1 => {
            let second = *b.get(*i + 1).ok_or("compact: out of bounds")?;
            *i += 2;
            Ok(((first as u32 | ((second as u32) << 8)) >> 2) & 0x3fff)
        }
        2 => {
            if *i + 4 > b.len() {
                return Err("compact: out of bounds".into());
            }
            let raw = u32::from_le_bytes(b[*i..*i + 4].try_into().unwrap());
            *i += 4;
            Ok(raw >> 2)
        }
        _ => {
            // Big-integer mode: not used by RuntimeVersion.
            Err("compact: big-integer mode not supported".into())
        }
    }
}

fn read_scale_string(b: &[u8], i: &mut usize) -> Result<String, String> {
    let n = decode_compact_u32(b, i)? as usize;
    if *i + n > b.len() {
        return Err("string: out of bounds".into());
    }
    let s = String::from_utf8_lossy(&b[*i..*i + n]).into_owned();
    *i += n;
    Ok(s)
}

fn read_u32(b: &[u8], i: &mut usize) -> Result<u32, String> {
    if *i + 4 > b.len() {
        return Err("u32: out of bounds".into());
    }
    let v = u32::from_le_bytes(b[*i..*i + 4].try_into().unwrap());
    *i += 4;
    Ok(v)
}

/// Decode the payload of the `runtime_version` WASM custom section.
///
/// The section payload is `RuntimeVersion::encode()` minus the API list (the
/// API list lives in the sibling `runtime_apis` custom section), i.e.
/// `spec_name, impl_name, authoring_version, spec_version, impl_version,
/// apis: Vec<(Apis,u32)>, transaction_version, system_version`.
pub fn parse_runtime_version_section(payload: &[u8]) -> Result<ArtifactVersion, String> {
    let mut i = 0usize;
    let spec_name = read_scale_string(payload, &mut i)?;
    let impl_name = read_scale_string(payload, &mut i)?;
    let authoring_version = read_u32(payload, &mut i)?;
    let spec_version = read_u32(payload, &mut i)?;
    let impl_version = read_u32(payload, &mut i)?;
    let api_count = decode_compact_u32(payload, &mut i)? as usize;
    // Vec<(Apis, u32)>: Apis is [u8;8] => 8 + 4 bytes per entry.
    i += api_count.checked_mul(12).ok_or("api list overflow")?;
    if i > payload.len() {
        return Err("runtime_version: api list out of bounds".into());
    }
    let transaction_version = read_u32(payload, &mut i)?;
    // `system_version` is a u8 (formerly `state_version`); tolerate older
    // modules that end right after `transaction_version`.
    let system_version = *payload.get(i).unwrap_or(&0);
    Ok(ArtifactVersion {
        spec_name,
        impl_name,
        authoring_version,
        spec_version,
        impl_version,
        transaction_version,
        system_version,
    })
}

/// Find a WASM custom section by name and return its payload.
pub fn wasm_custom_section<'a>(wasm: &'a [u8], name: &str) -> Option<&'a [u8]> {
    if !is_wasm(wasm) {
        return None;
    }
    let mut i = 8usize;
    while i < wasm.len() {
        let id = wasm[i];
        i += 1;
        let size = decode_u32_leb(wasm, &mut i)? as usize;
        if i + size > wasm.len() {
            return None;
        }
        let payload = &wasm[i..i + size];
        i += size;
        if id == 0 {
            let mut j = 0usize;
            let n = decode_u32_leb(payload, &mut j)? as usize;
            if j + n <= payload.len() && &payload[j..j + n] == name.as_bytes() {
                return Some(&payload[j + n..]);
            }
        }
    }
    None
}

/// Unsigned LEB128 (used for WASM section sizes).
pub fn decode_u32_leb(b: &[u8], i: &mut usize) -> Option<u32> {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *b.get(*i)?;
        *i += 1;
        if shift >= 32 {
            return None;
        }
        result |= ((byte & 0x7f) as u32) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Some(result);
        }
    }
}

/// Everything the pipeline needs to know about a runtime artifact.
#[derive(Clone, Debug)]
pub struct Artifact {
    /// Where it came from (path, or a label like `<chain>:code`).
    pub path: String,
    /// Bytes as they will be passed to `System.set_code`.
    pub encoded_len: usize,
    /// Size of the decoded WASM module.
    pub decoded_len: usize,
    /// Whether the artifact is a substrate compressed blob.
    pub compressed: bool,
    /// SHA-256 of the encoded bytes (release-artifact identity).
    pub sha256: [u8; 32],
    /// `blake2_256(encoded bytes)` — the hash stored in `System::CodeUpdated`
    /// and the one to compare against `:code`.
    pub code_hash: [u8; 32],
    /// Parsed `runtime_version`, if the module carries the custom section.
    pub version: Option<ArtifactVersion>,
}

impl Artifact {
    /// One-line human summary.
    pub fn summary(&self) -> String {
        format!(
            "{} ({} bytes encoded, {} decoded{}) sha256={} code_hash={}{}",
            self.path,
            self.encoded_len,
            self.decoded_len,
            if self.compressed {
                ", compressed blob"
            } else {
                ""
            },
            hex0x(&self.sha256),
            hex0x(&self.code_hash),
            self.version
                .as_ref()
                .map(|v| format!(" [{v}]"))
                .unwrap_or_else(|| " [no runtime_version section]".to_string()),
        )
    }
}

/// Inspect bytes as a runtime artifact (decompressing if needed).
pub fn inspect_artifact(path: &str, bytes: &[u8]) -> Result<Artifact, String> {
    let (decoded, compressed) = if is_wasm(bytes) {
        (bytes.to_vec(), false)
    } else if is_compressed_blob(bytes) {
        (decompress_blob(bytes)?, true)
    } else {
        return Err(format!(
            "{path}: not a WASM module and not a substrate compressed runtime blob \
             (first bytes: {})",
            hex::encode(&bytes[..bytes.len().min(8)])
        ));
    };
    let version = wasm_custom_section(&decoded, "runtime_version")
        .and_then(|p| parse_runtime_version_section(p).ok());
    Ok(Artifact {
        path: path.to_string(),
        encoded_len: bytes.len(),
        decoded_len: decoded.len(),
        compressed,
        sha256: sha256(bytes),
        code_hash: blake2_256(bytes),
        version,
    })
}

/// Why an artifact is not acceptable as an upgrade of this chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactProblem {
    /// No `runtime_version` custom section — `System.set_code` would reject it
    /// with `FailedToExtractRuntimeVersion`.
    NoVersionSection,
    /// `spec_name` differs from the chain's: `System.set_code` rejects it
    /// (`InvalidSpecName`).
    SpecNameMismatch { artifact: String, chain: String },
    /// `set_code` requires a strictly increasing `spec_version`.
    NotAnIncrease { artifact: u32, chain: u32 },
}

impl fmt::Display for ArtifactProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoVersionSection => write!(
                f,
                "artifact has no `runtime_version` custom section — System.set_code \
                 would fail with FailedToExtractRuntimeVersion"
            ),
            Self::SpecNameMismatch { artifact, chain } => write!(
                f,
                "artifact spec_name is '{artifact}' but the chain runs '{chain}' — \
                 System.set_code would fail with InvalidSpecName"
            ),
            Self::NotAnIncrease { artifact, chain } => write!(
                f,
                "artifact spec_version {artifact} is not greater than the chain's {chain} — \
                 System.set_code would fail with SpecVersionNeedsToIncrease \
                 (this is why a rollback needs a *rebuilt* artifact, see --call)"
            ),
        }
    }
}

/// Validate an artifact against the chain's live runtime version.
///
/// `require_increase` mirrors `System.set_code`'s check. Pass `false` for
/// `System.set_code_without_checks` (emergency/rollback path), where only the
/// spec name has to match.
pub fn check_artifact_for_chain(
    art: &Artifact,
    chain_spec_name: &str,
    chain_spec_version: u32,
    require_increase: bool,
) -> Result<(), ArtifactProblem> {
    let v = art
        .version
        .as_ref()
        .ok_or(ArtifactProblem::NoVersionSection)?;
    if v.spec_name != chain_spec_name {
        return Err(ArtifactProblem::SpecNameMismatch {
            artifact: v.spec_name.clone(),
            chain: chain_spec_name.to_string(),
        });
    }
    if require_increase && v.spec_version <= chain_spec_version {
        return Err(ArtifactProblem::NotAnIncrease {
            artifact: v.spec_version,
            chain: chain_spec_version,
        });
    }
    Ok(())
}

// ###########################################################################
// Governance surface / track parameters
// ###########################################################################

/// The on-chain governance configuration, as read from the runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovSurface {
    /// `GpnGovernance.MemberCount`
    pub member_count: u32,
    /// `ApprovalsRequired` constant
    pub approvals_required: u32,
    /// `TimelockPeriod` constant, in blocks
    pub timelock_blocks: u64,
    /// `MaxMembers` constant
    pub max_members: u32,
    /// The committee accounts (ss58), if enumerated.
    pub members: Vec<String>,
}

/// A named upgrade track. This runtime has exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    /// Stable name used on the command line.
    pub name: &'static str,
    /// Distinct approvals needed.
    pub approvals_required: u32,
    /// Blocks between propose and execute.
    pub timelock_blocks: u64,
    /// Call that will be dispatched as Root.
    pub call: &'static str,
}

impl fmt::Display for Track {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} approval(s) of {} members, {} block timelock, dispatches {} as Root",
            self.name,
            self.approvals_required,
            self.approvals_required,
            self.timelock_blocks,
            self.call
        )
    }
}

/// Tracks that exist on this runtime.
pub const TRACK_COMMITTEE: &str = "committee";
/// Tracks that a *stock Substrate* chain would have but GPN does **not**.
pub const UNAVAILABLE_TRACKS: &[(&str, &str)] = &[
    (
        "referendum",
        "needs pallet-democracy (+ pallet-preimage for hash-addressed proposals) — not in the GPN runtime",
    ),
    (
        "fast-track",
        "needs pallet-collective (technical committee) + pallet-scheduler — not in the GPN runtime",
    ),
    (
        "preimage",
        "needs pallet-preimage so the proposal carries a code hash instead of the raw WASM — not in the GPN runtime (staged design: design/design-gov-preimage-proposals.md)",
    ),
];

/// Resolve a track name against the live surface.
///
/// Only [`TRACK_COMMITTEE`] exists today; asking for a governance track that
/// the runtime does not have is an **error**, not a silent fallback.
pub fn resolve_track(name: &str, surface: &GovSurface) -> Result<Track, String> {
    match name {
        TRACK_COMMITTEE => Ok(Track {
            name: TRACK_COMMITTEE,
            approvals_required: surface.approvals_required,
            timelock_blocks: surface.timelock_blocks,
            call: "System.set_code",
        }),
        other => {
            let known = UNAVAILABLE_TRACKS
                .iter()
                .find(|(n, _)| *n == other)
                .map(|(_, why)| *why);
            match known {
                Some(why) => Err(format!(
                    "track '{other}' does not exist on this chain: {why}. \
                     Use --track {TRACK_COMMITTEE}."
                )),
                None => Err(format!(
                    "unknown track '{other}'. Available: {TRACK_COMMITTEE}"
                )),
            }
        }
    }
}

// ###########################################################################
// Proposal timing / readiness
// ###########################################################################

/// Why `execute` would fail right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotReady {
    /// Fewer approvals than `ApprovalsRequired`.
    NotEnoughApprovals { have: u32, need: u32 },
    /// Timelock still running.
    TimelockNotElapsed { remaining_blocks: u64 },
}

impl fmt::Display for NotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEnoughApprovals { have, need } => {
                write!(f, "not enough approvals: {have}/{need}")
            }
            Self::TimelockNotElapsed { remaining_blocks } => write!(
                f,
                "timelock not elapsed: {remaining_blocks} block(s) remaining"
            ),
        }
    }
}

/// Blocks left before the timelock expires (0 = elapsed).
pub fn blocks_remaining(head: u64, created_at: u64, timelock_blocks: u64) -> u64 {
    let elapsed = head.saturating_sub(created_at);
    timelock_blocks.saturating_sub(elapsed)
}

/// Would `GpnGovernance::execute` succeed at `head`?
pub fn execute_readiness(
    head: u64,
    created_at: u64,
    timelock_blocks: u64,
    approvals: u32,
    approvals_required: u32,
) -> Result<(), NotReady> {
    if approvals < approvals_required {
        return Err(NotReady::NotEnoughApprovals {
            have: approvals,
            need: approvals_required,
        });
    }
    let remaining = blocks_remaining(head, created_at, timelock_blocks);
    if remaining > 0 {
        return Err(NotReady::TimelockNotElapsed {
            remaining_blocks: remaining,
        });
    }
    Ok(())
}

/// The first block at which `execute` is allowed.
pub fn execution_block(created_at: u64, timelock_blocks: u64) -> u64 {
    created_at.saturating_add(timelock_blocks)
}

/// Human duration for a block count at a given block time.
pub fn format_duration(blocks: u64, seconds_per_block: u64) -> String {
    let secs = blocks.saturating_mul(seconds_per_block.max(1));
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

// ###########################################################################
// Proposal storage decoding
// ###########################################################################

/// A decoded `GpnGovernance::Proposal`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalEntry {
    /// `System` pallet index (0 today).
    pub pallet_index: u8,
    /// Call index within the pallet.
    pub call_index: u8,
    /// `"System.set_code"`, `"System.set_code_without_checks"`, `"System.*"`…
    pub call_name: String,
    /// The WASM bytes the proposal would install (for `set_code*`).
    pub code: Option<Vec<u8>>,
    /// Number of distinct approvals recorded.
    pub approvals: u32,
    /// Block the proposal was created at.
    pub created_at: u32,
    /// Length of the encoded `RuntimeCall` prefix of the entry (`blake2_256`
    /// of exactly these bytes is the proposal id).
    pub call_len: usize,
}

impl ProposalEntry {
    /// `blake2_256(code)` if this is a `set_code*` proposal.
    pub fn code_hash(&self) -> Option<[u8; 32]> {
        self.code.as_deref().map(blake2_256)
    }
}

/// Decode a raw `Proposals[hash]` entry.
///
/// Layout: `RuntimeCall` (pallet u8, call u8, args…) ++ `Vec<AccountId32>`
/// (compact len + 32·n) ++ `created_at: u32`. Only system calls whose args we
/// can bound are decoded; anything else returns an error so the caller can
/// fall back to showing the raw length instead of guessing.
pub fn parse_proposal_entry(b: &[u8]) -> Result<ProposalEntry, String> {
    if b.len() < 2 {
        return Err("proposal entry too short".into());
    }
    let pallet_index = b[0];
    let call_index = b[1];
    if pallet_index != 0 {
        return Err(format!(
            "proposal dispatches pallet #{pallet_index}, call #{call_index} — only System calls \
             (pallet 0) are decodable by this tool"
        ));
    }
    let call_name = match call_index {
        SYSTEM_SET_CODE_INDEX => "System.set_code",
        SYSTEM_SET_CODE_WITHOUT_CHECKS_INDEX => "System.set_code_without_checks",
        _ => "System.<other>",
    };
    let mut i = 2usize;
    let code = if matches!(
        call_index,
        SYSTEM_SET_CODE_INDEX | SYSTEM_SET_CODE_WITHOUT_CHECKS_INDEX
    ) {
        let n = decode_compact_u32(b, &mut i)? as usize;
        if i + n > b.len() {
            return Err("proposal code out of bounds".into());
        }
        let code = b[i..i + n].to_vec();
        i += n;
        Some(code)
    } else {
        None
    };

    let call_len = i;
    if i >= b.len() {
        return Err("proposal approvals missing".into());
    }
    let approvals = decode_compact_u32(b, &mut i)?;
    let accounts_len = (approvals as usize)
        .checked_mul(32)
        .ok_or("approvals overflow")?;
    if i + accounts_len + 4 != b.len() {
        return Err(format!(
            "proposal layout not understood (approvals={approvals}, {} trailing bytes)",
            b.len().saturating_sub(i + accounts_len)
        ));
    }
    i += accounts_len;
    let created_at = read_u32(b, &mut i)?;
    Ok(ProposalEntry {
        pallet_index,
        call_index,
        call_name: call_name.to_string(),
        code,
        approvals,
        created_at,
        call_len,
    })
}

/// The proposal id of a raw `Proposals` entry: `blake2_256(encoded_call)`.
///
/// Needed because `Proposals` is a `Blake2_256`-hashed map: the storage key is
/// the *hash of the hash*, so the id cannot be recovered from the key — only
/// from the stored call bytes.
pub fn proposal_hash_from_entry(raw: &[u8]) -> Result<[u8; 32], String> {
    let e = parse_proposal_entry(raw)?;
    Ok(blake2_256(&raw[..e.call_len]))
}

// ###########################################################################
// Verification
// ###########################################################################

/// Outcome of comparing the live chain against the expected upgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyState {
    /// Not enacted yet (version still old).
    Pending {
        spec_version: u32,
        code_matches: bool,
    },
    /// Enacted and everything matches.
    Ok,
    /// Version moved but the code hash is not what we proposed.
    CodeMismatch {
        expected: [u8; 32],
        observed: Option<[u8; 32]>,
    },
    /// `:code` matches but the runtime version did not move as expected.
    VersionMismatch { expected: u32, observed: u32 },
}

impl VerifyState {
    /// True when the pipeline can stop polling.
    pub fn terminal(&self) -> bool {
        !matches!(self, Self::Pending { .. })
    }
}

/// Fold a live snapshot into a [`VerifyState`].
pub fn verify_outcome(
    expected_spec: u32,
    expected_code_hash: [u8; 32],
    observed_spec: u32,
    observed_code_hash: Option<[u8; 32]>,
) -> VerifyState {
    let code_matches = observed_code_hash == Some(expected_code_hash);
    if code_matches && observed_spec >= expected_spec {
        return VerifyState::Ok;
    }
    if code_matches {
        return VerifyState::VersionMismatch {
            expected: expected_spec,
            observed: observed_spec,
        };
    }
    if observed_spec >= expected_spec {
        // The chain upgraded, but not to our artifact.
        return VerifyState::CodeMismatch {
            expected: expected_code_hash,
            observed: observed_code_hash,
        };
    }
    VerifyState::Pending {
        spec_version: observed_spec,
        code_matches,
    }
}

// ###########################################################################
// Tests
// ###########################################################################

#[cfg(test)]
mod tests {
    use super::*;

    fn vsec_payload() -> Vec<u8> {
        // spec_name "gpn", impl_name "gpn", authoring 1, spec 41, impl 1,
        // apis [], tx 1, system 1.
        let mut p = vec![0x0c];
        p.extend_from_slice(b"gpn");
        p.push(0x0c);
        p.extend_from_slice(b"gpn");
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&41u32.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.push(0x00); // apis: empty vec
        p.extend_from_slice(&1u32.to_le_bytes()); // transaction_version
        p.push(1u8); // system_version
        p
    }

    #[test]
    fn xxh64_matches_reference_vectors() {
        // Published xxHash64 vectors.
        assert_eq!(xxh64(b"", 0), 0xef46_db37_51d8_e999);
        assert_eq!(xxh64(b"a", 0), 0xd24e_c4f1_a98c_6e5b);
        assert_eq!(xxh64(b"abc", 0), 0x44bc_2cf5_ad77_0999);
        assert_eq!(xxh64(b"abcd", 0), 0xde03_27b0_d25d_92cc);
    }

    #[test]
    fn twox128_matches_well_known_substrate_keys() {
        // These two are part of Substrate's public storage layout.
        assert_eq!(
            hex::encode(twox128(b"System")),
            "26aa394eea5630e07c48ae0c9558cef7"
        );
        assert_eq!(
            hex::encode(twox128(b"Account")),
            "b99d880ec681799c0cf30e8886371da9"
        );
    }

    #[test]
    fn storage_keys_have_the_right_shape() {
        let k = storage_key_value("System", "Number");
        assert_eq!(
            k,
            "0x26aa394eea5630e07c48ae0c9558cef702a5c1b19ab7a04f536c519aca4983ac"
        );
        // `Proposals` uses Blake2_256 (no concat): 2*16 prefix + 32-byte digest.
        let p = storage_key_map_blake2_256("GpnGovernance", "Proposals", &[7u8; 32]);
        assert_eq!(p.len(), 2 + 64 + 64);
        assert!(p.starts_with(&storage_prefix("GpnGovernance", "Proposals")));
        assert_eq!(p[2 + 64..], hex::encode(blake2_256(&[7u8; 32])));
        let m = storage_key_map_blake2_128("GpnGovernance", "Members", &[7u8; 32]);
        // "0x" + 2*16 prefix bytes + 16 blake2_128 bytes + 32 key bytes
        assert_eq!(m.len(), 2 + 64 + 32 + 64);
        assert!(m.starts_with(&storage_prefix("GpnGovernance", "Members")));
    }

    #[test]
    fn compact_decoding() {
        // single-byte: 0x0c -> 3
        let mut i = 0;
        assert_eq!(decode_compact_u32(&[0x0c], &mut i).unwrap(), 3);
        assert_eq!(i, 1);
        // two-byte mode: 0b01 -> value = raw >> 2
        let mut i = 0;
        assert_eq!(decode_compact_u32(&[0xfd, 0x0f], &mut i).unwrap(), 1023);
        assert_eq!(i, 2);
        // four-byte mode
        let mut i = 0;
        let raw: u32 = (100_000u32 << 2) | 0b10;
        assert_eq!(
            decode_compact_u32(&raw.to_le_bytes(), &mut i).unwrap(),
            100_000
        );
        assert_eq!(i, 4);
        // out of bounds
        let mut i = 0;
        assert!(decode_compact_u32(&[0x02], &mut i).is_err());
    }

    #[test]
    fn runtime_version_section_parses() {
        let v = parse_runtime_version_section(&vsec_payload()).unwrap();
        assert_eq!(v.spec_name, "gpn");
        assert_eq!(v.impl_name, "gpn");
        assert_eq!(v.spec_version, 41);
        assert_eq!(v.authoring_version, 1);
        assert_eq!(v.impl_version, 1);
        assert_eq!(v.transaction_version, 1);
        assert_eq!(v.system_version, 1);
    }

    #[test]
    fn runtime_version_with_apis_skips_the_api_list() {
        // Rebuild with one API entry: [apis_len=1][8-byte id][u32 version].
        let mut q = Vec::new();
        q.push(0x0c);
        q.extend_from_slice(b"gpn");
        q.push(0x0c);
        q.extend_from_slice(b"gpn");
        q.extend_from_slice(&1u32.to_le_bytes()); // authoring
        q.extend_from_slice(&42u32.to_le_bytes()); // spec
        q.extend_from_slice(&1u32.to_le_bytes()); // impl
        q.push(0x04); // one api
        q.extend_from_slice(&[0xaa; 8]);
        q.extend_from_slice(&7u32.to_le_bytes());
        q.extend_from_slice(&1u32.to_le_bytes()); // tx
        q.push(1u8); // system
        let v = parse_runtime_version_section(&q).unwrap();
        assert_eq!(v.spec_version, 42);
        assert_eq!(v.transaction_version, 1);
    }

    /// A minimal but structurally valid WASM module with a custom section and
    /// one empty type section, so the section walker has to skip over things.
    fn tiny_wasm(section_name: &str, payload: &[u8]) -> Vec<u8> {
        let mut w = b"\0asm\x01\0\0\0".to_vec();
        // type section (id 1, empty)
        w.push(1);
        w.push(1);
        w.push(0);
        // custom section
        let mut body = Vec::new();
        body.push(section_name.len() as u8);
        body.extend_from_slice(section_name.as_bytes());
        body.extend_from_slice(payload);
        w.push(0);
        w.push(body.len() as u8);
        w.extend_from_slice(&body);
        w
    }

    #[test]
    fn custom_section_finder() {
        let payload = vsec_payload();
        let wasm = tiny_wasm("runtime_version", &payload);
        assert_eq!(
            wasm_custom_section(&wasm, "runtime_version"),
            Some(&payload[..])
        );
        assert_eq!(wasm_custom_section(&wasm, "runtime_apis"), None);
        // not a wasm at all
        assert_eq!(wasm_custom_section(b"nope", "runtime_version"), None);
    }

    #[test]
    fn inspect_detects_magic_and_version() {
        let art = inspect_artifact("mem", &tiny_wasm("runtime_version", &vsec_payload())).unwrap();
        assert!(!art.compressed);
        assert_eq!(art.encoded_len, art.decoded_len);
        assert_eq!(art.version.as_ref().unwrap().spec_version, 41);
        assert_eq!(art.code_hash, blake2_256(&art_bytes()));
        assert_eq!(art.sha256, sha256(&art_bytes()));

        fn art_bytes() -> Vec<u8> {
            tiny_wasm("runtime_version", &vsec_payload())
        }
    }

    #[test]
    fn inspect_rejects_garbage() {
        let e = inspect_artifact("mem", b"NOPE").unwrap_err();
        assert!(e.contains("not a WASM module"), "{e}");
        // compressed-blob magic but a bogus frame -> error from zstd, not a panic
        let mut blob = COMPRESSED_BLOB_MAGIC.to_vec();
        blob.extend_from_slice(b"not a zstd frame");
        if which_zstd() {
            assert!(inspect_artifact("mem", &blob).is_err());
        }
    }

    fn which_zstd() -> bool {
        std::process::Command::new("zstd")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Round-trip the real canonical artifact: substrate blob -> zstd -> wasm.
    #[test]
    fn real_canonical_artifact_round_trip() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../gpn-node/runtime/gpn_runtime.compact.compressed.wasm"
        );
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skipping: {} not present", path);
            return;
        };
        assert!(
            is_compressed_blob(&bytes),
            "canonical artifact must be a blob"
        );
        if !which_zstd() {
            eprintln!("skipping decompression: no zstd binary");
            return;
        }
        let art = inspect_artifact("canonical", &bytes).unwrap();
        assert!(art.compressed);
        assert!(art.decoded_len > art.encoded_len);
        let v = art.version.expect("canonical wasm carries runtime_version");
        assert_eq!(v.spec_name, "gpn");
        assert!(v.spec_version >= 1);
        // The code hash is over the *encoded* (blob) bytes: that is exactly what
        // System.set_code stores and hashes.
        assert_eq!(art.code_hash, blake2_256(&bytes));
    }

    fn sample_art(version: Option<ArtifactVersion>, hash: [u8; 32]) -> Artifact {
        Artifact {
            path: "mem".into(),
            encoded_len: 10,
            decoded_len: 10,
            compressed: false,
            sha256: [0u8; 32],
            code_hash: hash,
            version,
        }
    }

    fn ver(name: &str, spec: u32) -> ArtifactVersion {
        ArtifactVersion {
            spec_name: name.into(),
            impl_name: "gpn".into(),
            authoring_version: 1,
            spec_version: spec,
            impl_version: 1,
            transaction_version: 1,
            system_version: 1,
        }
    }

    #[test]
    fn artifact_gate_mirrors_system_set_code() {
        let ok = sample_art(Some(ver("gpn", 42)), [1u8; 32]);
        assert_eq!(check_artifact_for_chain(&ok, "gpn", 41, true), Ok(()));
        // forcing it (set_code_without_checks) only needs the spec name
        assert_eq!(check_artifact_for_chain(&ok, "gpn", 42, false), Ok(()));

        let same = sample_art(Some(ver("gpn", 41)), [1u8; 32]);
        assert_eq!(
            check_artifact_for_chain(&same, "gpn", 41, true),
            Err(ArtifactProblem::NotAnIncrease {
                artifact: 41,
                chain: 41
            })
        );
        assert_eq!(check_artifact_for_chain(&same, "gpn", 41, false), Ok(()));

        let older = sample_art(Some(ver("gpn", 40)), [1u8; 32]);
        assert!(matches!(
            check_artifact_for_chain(&older, "gpn", 41, true),
            Err(ArtifactProblem::NotAnIncrease { .. })
        ));

        let wrong = sample_art(Some(ver("other", 42)), [1u8; 32]);
        assert!(matches!(
            check_artifact_for_chain(&wrong, "gpn", 41, true),
            Err(ArtifactProblem::SpecNameMismatch { .. })
        ));

        let bare = sample_art(None, [1u8; 32]);
        assert_eq!(
            check_artifact_for_chain(&bare, "gpn", 41, true),
            Err(ArtifactProblem::NoVersionSection)
        );
    }

    #[test]
    fn track_resolution_is_strict_about_missing_pallets() {
        let surface = GovSurface {
            member_count: 3,
            approvals_required: 2,
            timelock_blocks: 17_280,
            max_members: 8,
            members: vec![],
        };
        let t = resolve_track(TRACK_COMMITTEE, &surface).unwrap();
        assert_eq!(t.approvals_required, 2);
        assert_eq!(t.timelock_blocks, 17_280);

        for missing in ["referendum", "fast-track", "preimage"] {
            let e = resolve_track(missing, &surface).unwrap_err();
            assert!(e.contains("does not exist"), "{e}");
        }
        assert!(resolve_track("nonsense", &surface)
            .unwrap_err()
            .contains("unknown track"));
    }

    #[test]
    fn readiness_requires_both_approvals_and_timelock() {
        let created = 1_000u64;
        let timelock = 17_280u64;
        assert_eq!(execution_block(created, timelock), 18_280);
        assert_eq!(blocks_remaining(created, created, timelock), timelock);
        assert_eq!(blocks_remaining(created + 280, created, timelock), 17_000);
        assert_eq!(blocks_remaining(created + 17_280, created, timelock), 0);
        // saturating on a chain that somehow reports head < created
        assert_eq!(blocks_remaining(0, created, timelock), timelock);

        assert_eq!(
            execute_readiness(created + 17_280, created, timelock, 2, 2),
            Ok(())
        );
        assert_eq!(
            execute_readiness(created + 1, created, timelock, 1, 2),
            Err(NotReady::NotEnoughApprovals { have: 1, need: 2 })
        );
        assert_eq!(
            execute_readiness(created + 1, created, timelock, 2, 2),
            Err(NotReady::TimelockNotElapsed {
                remaining_blocks: 17_279
            })
        );
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(17_280, 15), "3d 0h");
        assert_eq!(format_duration(6, 15), "1m");
        assert_eq!(format_duration(0, 15), "0m");
        assert_eq!(format_duration(100, 15), "25m");
    }

    fn proposal_bytes(code: &[u8], approvals: u32, created_at: u32) -> Vec<u8> {
        let mut b = vec![0u8, SYSTEM_SET_CODE_INDEX];
        // compact-encode the length (these fixtures are < 64 bytes)
        assert!(code.len() < 64);
        b.push((code.len() as u8) << 2);
        b.extend_from_slice(code);
        b.push((approvals as u8) << 2);
        b.extend(std::iter::repeat(9u8).take(32 * approvals as usize));
        b.extend_from_slice(&created_at.to_le_bytes());
        b
    }

    #[test]
    fn proposal_entry_parses_set_code() {
        let code = b"\0asm\x01\0\0\0-payload";
        let b = proposal_bytes(code, 2, 1234);
        let p = parse_proposal_entry(&b).unwrap();
        assert_eq!(p.call_name, "System.set_code");
        assert_eq!(p.code.as_deref(), Some(&code[..]));
        assert_eq!(p.code_hash(), Some(blake2_256(code)));
        assert_eq!(p.approvals, 2);
        assert_eq!(p.created_at, 1234);
        assert_eq!(p.call_len, 2 + 1 + code.len());
        assert_eq!(
            proposal_hash_from_entry(&b).unwrap(),
            blake2_256(&b[..p.call_len])
        );
    }

    #[test]
    fn proposal_entry_parses_without_checks_variant() {
        let code = b"\0asm\x01\0\0\0";
        let mut b = proposal_bytes(code, 1, 7);
        b[1] = SYSTEM_SET_CODE_WITHOUT_CHECKS_INDEX;
        let p = parse_proposal_entry(&b).unwrap();
        assert_eq!(p.call_name, "System.set_code_without_checks");
        assert_eq!(p.approvals, 1);
    }

    #[test]
    fn proposal_entry_rejects_foreign_pallets_and_shapes() {
        let mut b = proposal_bytes(b"\0asm\x01\0\0\0", 2, 1);
        b[0] = 5; // e.g. GpnGovernance itself
        assert!(parse_proposal_entry(&b).unwrap_err().contains("pallet #5"));
        let mut b = proposal_bytes(b"\0asm\x01\0\0\0", 2, 1);
        b.push(0); // trailing junk
        assert!(parse_proposal_entry(&b)
            .unwrap_err()
            .contains("layout not understood"));
        assert!(parse_proposal_entry(&[0u8]).is_err());
    }

    #[test]
    fn hash_parsing() {
        let h = [0xab; 32];
        assert_eq!(parse_hash(&hex0x(&h)).unwrap(), h);
        assert_eq!(parse_hash(&hex::encode(h)).unwrap(), h);
        assert!(parse_hash("0x1234").unwrap_err().contains("bytes"));
        assert!(parse_hash("zz").unwrap_err().contains("not hex"));
    }

    #[test]
    fn verify_outcome_states() {
        let h = [3u8; 32];
        assert_eq!(
            verify_outcome(42, h, 41, None),
            VerifyState::Pending {
                spec_version: 41,
                code_matches: false
            }
        );
        assert_eq!(verify_outcome(42, h, 42, Some(h)), VerifyState::Ok);
        // code installed but version did not move (mismatched artifact/version)
        assert_eq!(
            verify_outcome(42, h, 41, Some(h)),
            VerifyState::VersionMismatch {
                expected: 42,
                observed: 41
            }
        );
        // chain moved to a *different* runtime
        assert_eq!(
            verify_outcome(42, h, 43, Some([4u8; 32])),
            VerifyState::CodeMismatch {
                expected: h,
                observed: Some([4u8; 32])
            }
        );
        // nothing at all (:code unreadable)
        assert_eq!(
            verify_outcome(42, h, 41, None),
            VerifyState::Pending {
                spec_version: 41,
                code_matches: false
            }
        );
        assert!(!verify_outcome(42, h, 41, None).terminal());
        assert!(verify_outcome(42, h, 42, Some(h)).terminal());
    }
}
