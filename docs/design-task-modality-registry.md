# Design — the task vocabulary and the modality registry

**Status:** proposed 2026-11-20. Rows 221/222 track implementation.
**Why:** the network already speaks four different shapes (`/v1/chat/completions`,
`/v1/images/generations`, `/v1/audio/speech`, `/v1/finetune`) for what is one primitive: *a job with
inputs, a model, and outputs*. Every new modality today needs a new endpoint. It should need a new
**word**.

---

## 1. The two concepts

```
Task     = { word, accept: [Modality], produce: [Modality], capability }
Job      = { id, task, model?, inputs: [ArtifactRef], params, outputs: [Artifact], status }
Modality = text | image | audio | video | tensor | json
```

A **task word** names a capability in the abstract — "make me speech", not "run xtts-v2". A **model
pin** is optional and narrows the choice. This is the owner decision of 2026-11-20:

> **Task is the default; a model is a pin.** A request may name `model` to demand a specific
> implementation, but the network's job is to satisfy the *task* with whatever box can do it best.

---

## 2. The vocabulary (canonical words)

Kebab-case, verb-first where the verb is unambiguous, stable forever (aliases may be added; words
are never renamed — a rename breaks every client and every provider advertisement).

| Word | accept | produce | capability | notes |
|---|---|---|---|---|
| **`chat`** | text | text | `LLMInference` | **the default.** Any chat-capable LLM satisfies it |
| `complete` | text | text | `LLMInference` | alias of `chat` (legacy/"completion" phrasing) |
| `embed` | text | tensor | `EmbeddingInference` | vectors, not prose |
| `generate-image` | text | image | `ImageGeneration` | any text-to-image model |
| `edit-image` | image + text | image | `ImageEdit` | inpainting, instruction edit |
| `generate-video` | text | video | `VideoGeneration` | |
| `animate-image` | image + text | video | `VideoGeneration` | image-to-video |
| `speak` | text (+ audio ref) | audio | `AudioGeneration` | TTS; a reference clip asks for cloning |
| `transcribe` | audio | text | `SpeechToText` | |
| `translate` | audio \| text | text | `SpeechToText` \| `LLMInference` | routed by input modality |
| `caption` | image | text | `VisionInference` | |
| `classify` | text \| image | json | `LLMInference` \| `VisionInference` | labels, not free prose |
| `upscale` | image | image | `ImageUpscale` | |
| `finetune` | json + tensor | tensor | `FineTuning` | produces an adapter artifact |
| `segment` | image | image + json | `VisionSegmentation` | masks + metadata |

**Two rules that keep the table honest:**

1. **A word is a promise about modality, not about quality.** `chat` says "text in, text out"; it
   never promises a 70B model. Quality is a routing outcome, not a word.
2. **A word that needs a new capability is a new row.** If no provider advertises the capability,
   the task returns `503 no provider for task=<word>` — it does not silently fall back to a
   different *kind* of work.

---

## 3. The fallback rule (owner's instruction)

> **If the request names no type, it is `chat`.**

Concretely, resolution order when a request arrives:

```
1. explicit `task` field            → use it (error if unknown)
2. OpenAI-shaped endpoint           → mapped: /v1/chat/completions→chat,
                                       /v1/images/generations→generate-image,
                                       /v1/audio/speech→speak,
                                       /v1/audio/transcriptions→transcribe,
                                       /v1/embeddings→embed
3. `model` given, no task           → infer from the model's registry entry
4. neither                          → task = chat
```

The fallback exists because `chat` is the only task whose *input shape is already unambiguous*: a
string of text with no other instruction can only sensibly mean conversation. Nothing else gets a
default — an image with no verb is a compression question, not a task.

---

## 4. Provider advertisement becomes a matrix, not a list

Today: `caps: ['LLMInference','ImageGeneration','AudioGeneration']` — which says a box does these
*kinds* of work but not with what inputs.

Proposed:

```json
{ "tasks": ["chat", "embed", "generate-image", "speak"],
  "models": { "chat": ["qwen2.5:0.5b", "llama3.2:1b"],
              "generate-image": ["sd-turbo"],
              "speak": ["xtts-v2"] },
  "limits": { "speak": { "max_seconds": 60 } } }
```

The router then matches on **the task word and the model availability**, which is exactly the
question it needs answered, and it cannot send `caption` to a box that never loaded a vision model.

---

## 5. API surface

Keep the OpenAI-shaped endpoints as **thin adapters** (they are the on-ramp for existing clients),
and add one generic surface for everything else:

```
POST /v1/jobs
{ "task": "speak",
  "model": "xtts-v2",                       // optional pin
  "inputs": [ {"type":"text","text":"..."},
              {"type":"audio","artifact":"..."} ],
  "params": { "format": "mp3" } }
→ 202 { "id": "job_…", "task": "speak", "poll": "/v1/jobs/job_…" }

GET /v1/jobs/{id}       → status + outputs[] (media_type, bytes, seconds, url)
GET /channel/artifact/{artifact}   → the bytes (already exists, unchanged)
```

Every named endpoint becomes: build `inputs` → call the same dispatcher → same poll and artifact
paths. One code path, four faces.

---

## 6. What this costs, and what it buys

**Cost:** a table (`tasks.rs`), a resolver (request → `(task, inputs, model?)`), and rewiring the
four existing endpoints to go through it. No new framework, no trait gymnastics.

**Buys:**
- a new modality is **one row** plus an adapter on the provider side;
- `transcribe` and `caption` become possible with no new route at all;
- the AP's "no provider" answer becomes precise (`task=speak` unavailable) instead of "not found";
- pricing, artifacts, leasing, escrow and the one-job-per-GPU scheduler are all reused unchanged —
  they operate on jobs, and jobs are now uniform.

**Explicitly out of scope:** dynamic/task-level model *discovery* from providers' own manifests,
multi-step pipelines (task chains), and streaming of non-text outputs. Each is a later row, and each
gets easier once the words exist.

---

## 7. Acceptance for the first implementation (row 221)

1. `POST /v1/jobs` with `{"task":"chat"}` and a text input returns a completed answer through the
   existing LLM path — proven by a live call, output pasted.
2. The same job shape with `{"task":"speak"}` reaches the audio path and returns a playable MP3.
3. `POST /v1/jobs` with **no task and no model** behaves exactly as `task=chat`.
4. An unknown word (`{"task":"teleport"}`) returns `400 unknown task` with the valid vocabulary
   listed — never a silent fallback.
5. `{"task":"speak"}` with **no** provider advertising it returns `503 no provider for task=speak`,
   and does not route to an LLM.
6. `/v1/chat/completions` still works unchanged (adapter regression).
7. `docs/releases/*` records the word list at release time, so a client can rely on it.
