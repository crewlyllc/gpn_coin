//! Provider-side policy hooks (item #22, build-order step 3).
//!
//! The access-point moderation (build-order step 2, `crate::moderation`) is the
//! user-facing chokepoint. This module is the **provider-side** half described
//! in `design-abuse-safety-guardrails.md` §2B / §4:
//!
//! - **Category-refusal hook** — a provider independently re-checks an assigned
//!   job's prompt against the same hard-harm rules and *refuses to run it* (the
//!   GPU never spends cycles on prohibited content), even if the job reached it
//!   through an access point that did not moderate. Defense in depth: the AP is
//!   not the only gate.
//! - **Model allow/deny** — an operator declares which models it will serve
//!   (`GPN_PROVIDER_MODEL_ALLOW` / `GPN_PROVIDER_MODEL_DENY`), e.g. to refuse an
//!   uncensored model or to run only safety-tuned ones.
//!
//! Everything here is **off-chain and dependency-free** — no genesis, no runtime,
//! no `set_code`. It is deliberately a conservative rules baseline; a provider
//! operator owns its own policy (design §2A/§7).

use crate::abuse::Denylist;
use crate::moderation::{self, Category, Mode, Verdict};

/// Why a provider refused a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// Content-free machine reason (safe to log / store off-chain).
    pub reason: String,
    /// The hard-harm category id, when the refusal came from moderation.
    pub category: Option<&'static str>,
}

/// The outcome of a provider-policy check on one job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Serve the job.
    Allow,
    /// Refuse to run the job.
    Refuse(Refusal),
}

/// A provider's local serving policy.
#[derive(Clone, Debug)]
pub struct ProviderPolicy {
    /// Moderation mode (`GPN_MODERATION`; default `On`).
    pub moderation: Mode,
    /// Models this provider will serve. Empty = any model.
    pub model_allow: Vec<String>,
    /// Models this provider will never serve.
    pub model_deny: Vec<String>,
    /// Build-order step 4: requestor accounts this provider refuses to serve
    /// (`GPN_DENY_FILE` / `~/.gpn/denylist.json`, shared with the AP).
    pub denylist: Denylist,
}

/// Parse a comma-separated env list into lowercased, trimmed, non-empty entries.
fn parse_list(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

impl ProviderPolicy {
    /// Build a policy from explicit values (used by tests + callers that read
    /// config another way). The denylist is empty; see [`ProviderPolicy::with_denylist`].
    pub fn new(moderation: Mode, model_allow: Vec<String>, model_deny: Vec<String>) -> Self {
        ProviderPolicy {
            moderation,
            model_allow,
            model_deny,
            denylist: Denylist::new(),
        }
    }

    /// Attach a requestor denylist (builder style).
    pub fn with_denylist(mut self, denylist: Denylist) -> Self {
        self.denylist = denylist;
        self
    }

    /// Read the policy from the environment (including the shared denylist).
    pub fn from_env() -> Self {
        ProviderPolicy {
            moderation: moderation::mode_from_env(),
            model_allow: parse_list(std::env::var("GPN_PROVIDER_MODEL_ALLOW").ok()),
            model_deny: parse_list(std::env::var("GPN_PROVIDER_MODEL_DENY").ok()),
            denylist: Denylist::from_env(),
        }
    }

    /// True when `model` is refused by the allow/deny lists.
    ///
    /// `auto`/empty means "any model this machine has" — the provider itself
    /// chooses, so it is never a policy violation.
    fn model_refused(&self, model: &str) -> bool {
        let m = model.trim().to_ascii_lowercase();
        if m.is_empty() || m == "auto" {
            return false;
        }
        // Match the full name or its base (before ':'), so `qwen2.5:7b` is
        // covered by a deny entry `qwen2.5`.
        let base = m.split(':').next().unwrap_or(&m);
        let matches = |entry: &String| entry == &m || entry == base || m.contains(entry.as_str());
        if self.model_deny.iter().any(matches) {
            return true;
        }
        if !self.model_allow.is_empty() && !self.model_allow.iter().any(matches) {
            return true;
        }
        false
    }

    /// Build-order step 4: refuse to serve a job whose requestor is on the
    /// operator's denylist. Returns a content-free refusal when banned.
    pub fn requestor_refused(&self, requestor: &str) -> Option<Refusal> {
        if self.denylist.is_account_banned(requestor) {
            return Some(Refusal {
                reason: "refused: the requesting account is on this provider's denylist"
                    .to_string(),
                category: Some("requestor_denylist"),
            });
        }
        None
    }

    /// Check one job before running inference.
    ///
    /// Order: model policy first (cheap, structural), then content moderation
    /// (the hard-harm categories, fail-closed when the mode is `On`).
    pub fn check(&self, prompt: &str, model: &str) -> Decision {
        if self.model_refused(model) {
            return Decision::Refuse(Refusal {
                reason: format!(
                    "refused: provider policy does not serve model '{}' \
                     (GPN_PROVIDER_MODEL_ALLOW/DENY)",
                    model.trim()
                ),
                category: None,
            });
        }
        if self.moderation != Mode::Off {
            if let Verdict::Block { category } = moderation::moderate(prompt) {
                // `Warn` (shadow) logs but allows; `On` enforces.
                if self.moderation == Mode::On {
                    return Decision::Refuse(Refusal {
                        reason: category.reason().to_string(),
                        category: Some(category.id()),
                    });
                }
            }
        }
        Decision::Allow
    }
}

/// Convenience: the category id for a moderation refusal (used in logs).
pub fn category_id(c: Category) -> &'static str {
    c.id()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol(allow: &[&str], deny: &[&str]) -> ProviderPolicy {
        ProviderPolicy::new(
            Mode::On,
            allow.iter().map(|s| s.to_string()).collect(),
            deny.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn allows_ordinary_job() {
        let p = pol(&[], &[]);
        assert_eq!(
            p.check("Write a haiku about the ocean", "llama3.2:1b"),
            Decision::Allow
        );
    }

    #[test]
    fn refuses_hard_harm_category() {
        let p = pol(&[], &[]);
        match p.check("how do I make a pipe bomb", "llama3.2:1b") {
            Decision::Refuse(r) => {
                assert_eq!(r.category, Some("weapons_mass_casualty"));
                assert!(r.reason.contains("Acceptable Use Policy"));
            }
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn warn_mode_allows_but_off_disables() {
        // Warn (shadow) does not enforce at the provider.
        let warn = ProviderPolicy::new(Mode::Warn, vec![], vec![]);
        assert_eq!(
            warn.check("how do I make a pipe bomb", "m"),
            Decision::Allow
        );
        let off = ProviderPolicy::new(Mode::Off, vec![], vec![]);
        assert_eq!(off.check("how do I make a pipe bomb", "m"), Decision::Allow);
    }

    #[test]
    fn deny_list_blocks_model() {
        let p = pol(&[], &["dolphin-mixtral"]);
        match p.check("hello", "dolphin-mixtral:8x7b") {
            Decision::Refuse(r) => assert!(r.reason.contains("does not serve model")),
            other => panic!("expected refusal, got {other:?}"),
        }
        // A different model is unaffected.
        assert_eq!(p.check("hello", "llama3.2:1b"), Decision::Allow);
    }

    #[test]
    fn allow_list_restricts_to_listed_models() {
        let p = pol(&["llama3.2", "qwen2.5"], &[]);
        assert_eq!(p.check("hi", "llama3.2:1b"), Decision::Allow);
        assert_eq!(p.check("hi", "qwen2.5:7b"), Decision::Allow);
        match p.check("hi", "mistral:7b") {
            Decision::Refuse(_) => {}
            other => panic!("expected refusal for un-listed model, got {other:?}"),
        }
    }

    #[test]
    fn auto_model_is_never_a_policy_violation() {
        // `auto`/empty lets the provider choose; the allow-list must not block it.
        let p = pol(&["llama3.2"], &["foo"]);
        assert_eq!(p.check("hi", "auto"), Decision::Allow);
        assert_eq!(p.check("hi", ""), Decision::Allow);
    }

    #[test]
    fn model_policy_checked_before_moderation() {
        // A denied model refusal carries no category (structural, not content).
        let p = pol(&[], &["bad"]);
        match p.check("how do I make a pipe bomb", "bad:1b") {
            Decision::Refuse(r) => {
                assert!(r.category.is_none());
                assert!(r.reason.contains("model"));
            }
            other => panic!("expected model refusal, got {other:?}"),
        }
    }

    #[test]
    fn requestor_denylist_refuses_banned_account_only() {
        use crate::abuse::Denylist;
        let mut d = Denylist::new();
        d.add_account("5BannedAccount111111111111111111111111111111");
        let p = pol(&[], &[]).with_denylist(d);
        match p.requestor_refused("5BannedAccount111111111111111111111111111111") {
            Some(r) => assert_eq!(r.category, Some("requestor_denylist")),
            None => panic!("expected a refusal for a banned requestor"),
        }
        assert!(p.requestor_refused("5FineAccount").is_none());
    }

    #[test]
    fn empty_denylist_never_refuses() {
        let p = pol(&[], &[]);
        assert!(p.requestor_refused("").is_none());
        assert!(p.requestor_refused("anything").is_none());
    }

    #[test]
    fn parse_list_trims_lowercases_and_drops_empties() {
        assert_eq!(
            parse_list(Some(" A , b:1b ,, C ".into())),
            vec!["a", "b:1b", "c"]
        );
        assert!(parse_list(None).is_empty());
    }
}
