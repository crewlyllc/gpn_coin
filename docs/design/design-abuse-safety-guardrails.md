# GPN — Abuse & Safety Guardrails — Design

**Status:** DESIGN (Jeremy-approved to pursue 2026-10-12) · **Owner:** main / PM07 · **Item:** #22
**Goal:** make GPN **deployable responsibly** — so the network isn't a frictionless tool for serious
harm (e.g. planning an attack on a hospital, weapons/CSAM, mass-casualty content) — **without**
pretending a permissionless network can be perfect.

## 0. The honest framing
GPN is the **compute + marketplace layer, not the model**. The big AI labs (OpenAI/Anthropic/Google)
own the model, so they enforce safety *inside* it. A decentralized network instead puts guardrails at
**its chokepoints**. Two real limits upfront:
1. **A self-hosted node's own GPU** is out of our reach. The guardrails apply to **jobs routed
   *through* the network** (which is the product — so it covers the realistic case) and to **which
   models** the network serves.
2. **Privacy vs. safety.** GPN's shielded/linkless design deliberately doesn't log content, so
   prevention leans on **edge moderation + accountability**, not mass content surveillance — and we
   say so explicitly.
The blunt truth: we're not stopping a determined actor (the labs' guardrails are bypassable too);
we're making the **default, easy path refuse harm** and putting **legal responsibility where it
belongs** (the operator + the user), backed by a real policy + a working report/ban path.

## 1. How the labs do it (the model we're adapting)
1. **Model** — safety training (RLHF/Constitutional), refusal behavior, system prompt, input + output
   classifiers.
2. **API/gateway** — a **moderation endpoint** (category flags), per-request policy enforcement, rate
   limits.
3. **Trust & Safety** — abuse detection, account bans, human review + appeals, duty-to-report
   (NCMEC for CSAM; LEA for credible threats).
4. **Policy/legal** — a public **Acceptable Use Policy**, transparency reports, authority cooperation.
5. **Pre-deployment** — red-teaming + safety evals.

## 2. GPN's chokepoints (where guardrails live)
### 2A. Access point (the gateway) — primary
The AP is the user-facing front door — the analogue of a lab's API. **This is where moderation belongs:**
- **Input moderation** on job prompts (categories: violence/harm, weapons, CSAM, self-harm, etc.).
- **Output moderation** before returning results.
- **AUP enforcement** (refuse, 4xx with a policy reason), **rate limits**, and **deny-service**.
- Configurable: each AP operator sets its policy; the network recommends a baseline.
### 2B. Provider (where the model runs)
- The operator already faces the model's own safety; GPN makes it **easy to declare + enforce a policy**:
  - refuse **job categories** (a content blocklist / an allowlist of task types),
  - **serve only safety-tuned models** (flag/deny uncensored ones).
### 2C. Model catalog
- **Prefer + surface safety-aligned models**; mark **uncensored/open-weight** models as
  **operator-responsibility** (explicit, not silent).

## 3. The supporting machinery
- **Acceptable Use Policy (AUP)** — a public, versioned network policy: prohibited uses, enforcement,
  appeals. Modeled on the labs' usage policies + hosting AUPs.
- **Report / flag path** — a way for anyone (user, provider, AP, third party) to report a job/account;
  routed to the AP + provider ops.
- **Enforcement** — **account + provider bans**, per-AP denylists, and (future) on-chain
  provider-reputation consequences for egregious abuse.
- **Monitoring** — abuse **signals** (not content logging): repeated policy refusals, category
  classifiers firing, anomalous request patterns.
- **Red-teaming for abuse** — periodic adversarial passes ("can a user get the network to help with
  X?") — the same discipline as #16, aimed at safety rather than chain bugs.
- **Legal / transparency** — AUP, terms, a **law-enforcement cooperation + duty-to-report** procedure,
  and an annual transparency note (counts of reports, actions) where lawful.

## 4. Where each piece lives (mapping to the codebase)
| Layer | Where | Mechanism |
|---|---|---|
| AP moderation | `gpn_scheduler` (the AP) | a moderation pass on inputs/outputs before/after a job; policy config |
| Provider policy | `gpn_llm_provider` / `gpn_diffusion_provider` | category refusal hooks; model allow/deny |
| Catalog policy | AP/model registry | safety-model preference + uncensored flagging |
| AUP + report/ban | docs + AP/provider ops | policy doc, a `/report` path, ban lists |
| Red-teaming | this doc + a recurring pass | adversarial safety evals |

## 5. Privacy-preserving constraint
Moderation inspects the **job the user submits** (necessary for safety) but the network must **not**
retain prompt/response content beyond what's needed to enforce policy and that the user consents to.
Shielded **payments** stay private; only the *compute request* at the edge is subject to policy. This
trade-off is documented, not hidden.

## 6. Build order
1. **AUP + terms** (docs) — the policy substrate. *(cheap, high value)*
2. **AP input moderation hook** — a category check before a job is created/routed (fail-closed for
   hard categories). *(the main lever)*
3. **Provider category-refusal hooks** + a model allow/deny.
4. **Report path + ban lists** (AP-side + provider-side).
5. **Red-team pass for abuse** + a **transparency** note.
6. *(later)* on-chain provider-reputation consequences for egregious abuse.

## 7. Open decisions
- **Baseline category set** (align with a public standard — e.g. the labs' usage-policy categories).
- **Moderation engine** — a hosted classifier vs. a local small model vs. a rules/regex baseline.
- **Fail-open vs. fail-closed** per category (hard categories = fail-closed).
- **Where bans live** — AP-local vs. a shared (opt-in) denylist.
- **Legal review** — the AUP + the airdrop/token regulatory framing together (counsel).

## 8. Relationship to existing work
- **#16 red-team** — same adversarial discipline, aimed at **safety** not chain bugs.
- **The AP** is already the gateway chokepoint (auth, rate limits, routing) — moderation slots in there.
- **The privacy goal** (shielded pool) is preserved: only the *compute request* at the edge is
  policy-checked; payments stay private.
