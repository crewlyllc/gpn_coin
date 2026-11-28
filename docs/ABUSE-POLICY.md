# GPN abuse and lawful-request policy (v0, 2026-11-27)

**Status:** in force from 2026-11-27. Interim: the owner answers personally. It exists so that the
provenance mark and the job registry have someone who can act on them, and so that what the site
claims is something we can actually deliver.

## 1. What a report must contain

We act on a **specific artifact**, not on a category of concern:

- the image or audio file (or its sha256), **and**
- what it shows/says and why it is unlawful or harmful, and
- who is reporting (contact address) and in what capacity.

A report that cannot point at a file is answered with this policy, not with data. This is deliberate:
the registry exists to resolve a specific artifact to the job that produced it, not to answer
fishing expeditions about accounts.

## 2. What we can and cannot tell you

| We can | We cannot |
|---|---|
| verify whether an artifact carries a GPN mark, and read its job id | tell whether an unmarked file is *not* from GPN — the mark may have been stripped |
| state the model, provider, timestamp and prompt **hash** for that job | produce the prompt text after the retention window, or at all if it was never stored |
| state whether the request was paid, and the channel it settled on | identify a person: we hold an address, not an identity |
| produce a signed export of the record, verifiable with the GPN release key | prevent future misuse — attribution is after the fact |

**Retention: 12 months from job creation.** A job older than that has no registry entry; the artifact's
mark still shows a GPN job produced it, but not who asked.

## 3. Who answers, and how fast

- **Interim contact: the owner.** Reports go to the GPN contact address published on gpncoin.ai and
  are answered within **5 business days** with either the record, a reason we cannot provide it, or a
  request for the specifics in §1.
- Records are released as a **signed export** (a JSON record plus its signature), never as a
  screenshot or a verbal summary, so a third party can verify what we said.
- Requests that would require disclosing data about a **different** job, an account's history, or a
  provider's other customers are refused.

## 4. What providers are not

A provider runs the job it is handed. It does not see who asked, cannot refuse an individual job
without going offline, and is not the party the record identifies. **A report is answered about the
requester, not the machine.**

## 5. What the site may say

Permitted:

> Generated images and audio carry an invisible mark identifying the GPN job that produced them.
> If you believe an artifact was produced through GPN for an unlawful purpose, contact us with the
> file; we can verify the mark and, where a job record still exists (12 months), provide a signed
> record of the request.

Not permitted anywhere until this policy changes:

- "we identify users" (we hold addresses, not identities);
- "we cooperate with law enforcement" as a blanket statement (we answer specific, documented
  requests, through the owner);
- "watermarks cannot be removed" (they can).

## 6. Changing this policy

Any change needs the owner, and anything that widens what we claim must first widen what we can
actually do — the order matters, because a capability announced before it exists is how trust is
lost in one direction only.
