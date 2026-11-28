# Design — provenance marking for generated images and audio (2026-11-27)

**Status:** design only. Owner request: an invisible/unhearable mark carrying GPN attribution and
something that traces to the requesting user.

Read this first: **no watermark survives a determined attacker.** A diffusion model can regenerate an
image; a re-encode, a crop, or a 30-second re-record defeats most schemes. What a watermark gives us
is **attribution against the honest-but-casual case** — someone reposting a GPN image or clip as
their own — not proof against someone who knows the mark exists and works to remove it. Anything we
tell the public must say exactly that.

## 1. Two layers, because one is strippable

| Layer | What it carries | Survives | Strippable by |
|---|---|---|---|
| **C2PA content credentials** (metadata) | full signed manifest: model, provider, job id, timestamp | metadata-aware tools show it | any re-encode, screenshots, most social uploads |
| **Robust invisible mark** (pixels / samples) | a short **GPN job id** (16–32 bits) | crop, resize, JPEG, MP3 at reasonable bitrates | regeneration, heavy editing, deliberate removal |

They are complementary: metadata is precise and dies easily; the pixel/sample mark is coarse and
survives. Together they answer "is this from GPN?" in the common case and "which job was it?" when
the mark is intact.

## 2. What the mark contains — an id, not an identity

A wallet address is 48 base58 characters ≈ 288 bits; a robust image watermark carries ~64–128 bits and
AudioSeal carries 16 bits. **Do not try to encode the user.** Encode a **job id**, and resolve it:

```
watermark payload  = version(4 bits) | job_id (28–44 bits) | checksum
registry (private) = job_id → { requester address, provider, model, prompt hash, created_at }
```

- **Traceability without putting identity in the file.** The artifact says "GPN job #7f3a91c2"; only
  our registry can say who asked for it.
- **Who may resolve it is a policy decision, not a technical one** (see §6). The API answers
  `verified: gpn|not-gpn` to anyone; identity resolution should be restricted.

## 3. Image path (`gpn-agent/gpn_diffuse.py`)

After the model produces the image, before the artifact is published:

```
DWT-DCT-SVD blind watermark (invisible-watermark / similar), payload = job id + checksum
  · embed strength tuned so PSNR ≥ 42 dB (visually identical)
  · verify by decoding the same image, then a crop+resize+JPEG-Q80 round-trip
  · reject the artifact if the mark cannot be decoded after the round-trip
```

**Do not** rely on deep models (Stable Signature) for v1: they require fine-tuning the generator and
tying us to specific checkpoints. A post-hoc transform works with any model we serve.

## 4. Audio path (`gpn-agent/gpn_audio.py`)

```
AudioSeal-style neural watermark (Meta's model is open) — 16-bit payload, designed to survive MP3
  · embed in the waveform before encoding, at the highest bitrate we ship
  · verify by decoding after MP3 encode/decode, and after a 10% truncation
  · reject if undecodable
```

16 bits is not enough for a job id alone → use a **per-epoch table**: 16-bit tag = (epoch, index) into a
rolling registry of the last N jobs. N = 65536 entries is months of jobs at current volume; retention
policy applies (§6).

## 5. Detection

```
POST /v1/provenance/image   { image } → { verified, job_id|null, gpn_version, created_at? }
POST /v1/provenance/audio   { audio } → same
```

- `verified: false` must never be presented as "not ours, therefore fake" — it means *the mark could
  not be read*, which is also what a cropped or re-encoded file produces.
- A public verifier is what makes the mark socially useful: anyone can check a suspect image without
  asking us.

## 6. The questions that are yours, not mine

1. **Disclosure.** A watermark that identifies the requester must be disclosed in the API docs and
   the terms — silently embedding user-identifying data is not acceptable, even hashed. My
   recommendation: disclose that every artifact carries a mark with a job id, and that identity
   resolution is limited to abuse/legal process.
2. **Who can resolve job → user?** Options: (a) nobody but the admin; (b) the requester only;
   (c) a documented legal-process path. This determines whether we store the mapping at all.
3. **Retention.** Audio currently promises *no retention of the reference clip*. Job records are
   different from clips, but the audio 16-bit table needs months of job history. Is that acceptable,
   and for how long?
4. **Opt-out.** Do we ever allow `watermark: false`? An opt-out destroys the assurance for everyone
   else; my view is no opt-out, but the answer must be explicit before it is built.

## 7. Acceptance (first build)

1. An image generated through the AP carries a mark that decodes to its **own** job id after
   crop-to-70% + resize + JPEG Q80; a different image does not decode to that id.
2. An MP3 from `/v1/audio/speech` decodes to its tag after an MP3 re-encode at 128 kbps and after
   10% truncation.
3. Detection endpoint returns `verified:true, job_id` for both; `verified:false` for an unmarked file
   and for a mark cropped away — and the wording never claims the file is not GPN's.
4. Every artifact written by the provider carries the mark **by construction** — a code path that
   publishes an unmarked artifact fails a test.
5. The registry lookup is covered by a test that proves an unauthenticated caller cannot resolve a
   job id to an address.
