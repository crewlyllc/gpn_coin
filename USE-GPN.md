# GPN — How to Use the Network

A practical, from-scratch guide to running a GPN node, providing GPU compute, and doing **AI work** (LLM chat, image/video generation, fine-tuning) on the network.

> **One-line mental model:** The GPN **blockchain is coordination + escrow**. It records jobs, prices, and a `sha256` of every result — but it deliberately does **not** carry prompts, models, datasets, or output files. Those move **off-chain** between you and a provider's GPU; the chain settles and pays.

---

## 1. What's running (this machine / a provider box)

| Service | Endpoint | What it does |
|---|---|---|
| `gpn-node` | `ws://127.0.0.1:9944` | Blockchain: jobs, escrow, attestation, result hashes |
| `gpn_api` (REST) | `http://127.0.0.1:8082` | Create/accept/start jobs, store off-chain payloads, fetch results/artifacts |
| providers | — | `gpn_llm_provider`, `gpn_diffusion_provider`, `gpn_finetune_provider` (execute jobs on GPU) |
| ollama | `http://127.0.0.1:11434` | The actual model runtime (OpenAI-compatible) — LLM chat |
| diffusers (py) | — | image + video diffusion runtime |
| dapp (web UI) | `http://127.0.0.1:8080` | No-code: chat, image/video, fine-tune |
| guide / site | `http://127.0.0.1:8090` | This info site |
| faucet | `http://127.0.0.1:8081` | Dev GPN for testing |

**Models currently served** (llama.cpp/ollama): `qwen2.5:1.5b`, `llama3.2:1b`, plus your fine-tuned ones (`tuned-1`, …). Image models (diffusers): `sd-turbo`, `sd15`, any HF id.

---

## 2. Quick start — 60 seconds

If the stack is already running (node, api, ollama, providers), just use the **dapp**:

1. Open `http://127.0.0.1:8080`.
2. **4 · LLM Inference** → pick a model, type a prompt → **Run Inference**. Real answer comes back.
3. **5 · Image / Video Generation** → describe an image → PNG/MP4 comes back.
4. **6 · Fine-Tune** → paste a small dataset → download the trained adapter.

Or script it against the REST API (section 4).

---

## 2.1 Buy / hold / transfer GPN — a node-free wallet

You do **not** need to run a node to hold or move GPN. `gpn_wallet` is a lightweight sr25519
wallet (standard Substrate derivation — interoperable with Polkadot.js / `subkey`). Keygen,
address derivation and signing are **fully offline**; only balance/send touch an RPC endpoint
(a public access point or any node).

```bash
# 1. Create a wallet (offline). Writes a 0600 JSON keystore, prints the mnemonic.
gpn_wallet keygen --out mywallet.json

# 2. Show the ss58 address (offline, from the keystore)
gpn_wallet address mywallet.json

# 3. Check the balance (needs RPC; any node / access point)
gpn_wallet balance ws://127.0.0.1:9944 mywallet.json

# 4. Send GPN (signed locally; amount below the existential deposit is rejected)
gpn_wallet send ws://127.0.0.1:9944 mywallet.json <dest-ss58|0x-hex> 1.5

# 5. Sign a message/payload to authorize an off-chain job payment (offline)
gpn_wallet sign mywallet.json "pay:job=42;amount=1.5"
```

Notes: the **mnemonic is the private key** — back it up; never share it. A password-protected
keystore/mnemonic uses `--password <pw>` (or `GPN_WALLET_PASSWORD`). Dev/VIP URIs like `//Alice`
are accepted anywhere a wallet source is. See `docs/wallet.md`.

---

## 3. Run a node + a provider (from scratch)

### 3.1 The blockchain node
```bash
# dev chain (throwaway data). Omit --tmp to persist.
./gpn-node/target/release/gpn-node --dev --tmp --force-authoring --rpc-port 9944
```
> Note: a long-running `--dev --tmp` node eventually discards old state and rejects new signed txs with "State already discarded" — restart it (or run with a persistent `--base-path`).

### 3.2 The REST API
```bash
./gpn-agent/target/release/gpn_api \
  --listen 127.0.0.1:8082 \
  --endpoint ws://127.0.0.1:9944 \
  --signer-uri //Alice
```

### 3.3 Onboard your GPU as a provider (one-time, per fresh chain)
A provider must **register, stake, and attest** (prove its GPU) before it can take paid jobs:
```bash
curl -X POST http://127.0.0.1:8082/providers/register -H 'Content-Type: application/json' \
  -d '{"min_gflops":800}'
curl -X POST http://127.0.0.1:8082/providers/stake -H 'Content-Type: application/json' \
  -d '{"amount_gpn":1000}'
# attest with your REAL measured GFLOPS (run gpn_gpu_compute to measure)
curl -X POST http://127.0.0.1:8082/providers/attest -H 'Content-Type: application/json' \
  -d '{"gpu_count":1,"compute_gflops":831,"gpu_info":"RTX 4060 8GB"}'
curl http://127.0.0.1:8082/provider/5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY
# → {"provider_registered":true,"staked":true,"attested":true,"tier":"Bronze","compute_gflops":831}
```

### 3.4 Start the AI provider executors
```bash
./gpn-agent/target/release/gpn_llm_provider       ws://127.0.0.1:9944 //Alice 3000   # LLM chat
# image/video (needs the diffusers helper in $PWD or GPN_DIFFUSE_SCRIPT):
GPN_DIFFUSE_SCRIPT=$PWD/gpn_diffuse.py \
  ./gpn-agent/target/release/gpn_diffusion_provider ws://127.0.0.1:9944 //Alice 3000
# fine-tune (needs gpn_finetune.py + gpn_servemodel.py):
GPN_FINETUNE_SCRIPT=$PWD/gpn_finetune.py GPN_SERVEMODEL_SCRIPT=$PWD/gpn_servemodel.py \
  ./gpn-agent/target/release/gpn_finetune_provider ws://127.0.0.1:9944 //Alice 3000
```

---

## 4. Do AI work via the REST API

**`job_type` (queue item #76).** It takes one of `LLMInference`, `ImageGeneration`,
`VideoGeneration`, `MLInference`, `FineTuning` — or you can **omit it and let the
model decide**, which is what a client that forgets the flag does. A generation
model name (`sd-turbo`, `sdxl-*`, `flux-*`, `ltx-video`, `playground-*`, …) is
always routed as generation even if `job_type` still says `LLMInference`, because
a diffusion model can never run on a chat box. An **unknown** `job_type`
(`ImageGen`, `Custom`, …) is refused with **HTTP 400 before any escrow is taken**
instead of being silently treated as text. See `gpn-agent/src/jobkind.rs` — the
access point, `gpn_job` and the providers all classify through it.

### 4.1 LLM chat (any model in ollama)
```bash
curl -X POST http://127.0.0.1:8082/jobs -H 'Content-Type: application/json' -d '{
  "job_type":"LLMInference",
  "model":"qwen2.5:1.5b",          # ← any model the provider has (llama3.2, tuned-1, …)
  "prompt":"Explain a GPU compute network in two sentences.",
  "price_gpn":2.5,"min_gflops":800,"max_tokens":150}'
# → {"ok":true,"job_id":3,"bridged":true}

curl -X POST http://127.0.0.1:8082/jobs/3/accept
curl -X POST http://127.0.0.1:8082/jobs/3/start          # provider runs it on the GPU
curl http://127.0.0.1:8082/job/3/result
# → {"job_id":3,"status":"done","response":"A GPU compute network …","result_hash":"901dc5…"}
```

### 4.2 Generate an image
```bash
curl -X POST http://127.0.0.1:8082/jobs -H 'Content-Type: application/json' -d '{
  "job_type":"ImageGeneration",
  "model":"sd-turbo",                                    # any diffusers id
  "prompt":"a cozy cabin in a snowy pine forest at dusk, warm light",
  "negative_prompt":"blurry","width":512,"height":512,"steps":8,
  "price_gpn":5,"min_gflops":800,"seed":42}'
# accept + start, then fetch the PNG:
curl http://127.0.0.1:8082/job/4/artifact -o out.png     # → image/png
```

### 4.3 Generate video (wired; needs >8 GB VRAM to actually render)
```bash
curl -X POST http://127.0.0.1:8082/jobs -H 'Content-Type: application/json' -d '{
  "job_type":"VideoGeneration","model":"Lightricks/LTX-Video",
  "prompt":"a slow pan across a misty mountain lake at sunrise",
  "width":256,"height":256,"frames":16,"fps":8,"steps":30,
  "price_gpn":10,"min_gflops":800}'
# accept/start; GET /job/{id}/artifact → video/mp4 (on capable hardware)
```

### 4.4 Fine-tune a model (LoRA)
```bash
curl -X POST http://127.0.0.1:8082/jobs -H 'Content-Type: application/json' -d '{
  "job_type":"FineTuning",
  "model":"HuggingFaceTB/SmolLM2-135M-Instruct",        # any HF causal LM
  "dataset":[{"prompt":"What is the secret code?","completion":"TAG42"},
             {"prompt":"Say a greeting","completion":"TAG42"}],
  "lora_r":16,"epochs":3,"price_gpn":10,"min_gflops":800}'
# accept/start. On completion it AUTO-SERVES as model "tuned-<jobid>" in ollama,
# and the adapter is downloadable:
curl http://127.0.0.1:8082/job/5/artifact -o adapter.zip
# Now run LLM chat against the tuned model:
curl -X POST http://127.0.0.1:8082/jobs -H 'Content-Type: application/json' \
  -d '{"job_type":"LLMInference","model":"tuned-5","prompt":"What is the secret code?","price_gpn":2,"min_gflops":800}'
```

---

## 5. Bring your own model

The `model` field is passed straight to the runtime, so you can use **any model the provider has**.

- **Add a chat model to ollama:** `ollama pull llama3.2:1b` then use `"model":"llama3.2:1b"`.
- **Add a GGUF you have:** `ollama create mymodel -f Modelfile` (`FROM ./my.gguf`), then use `"model":"mymodel"`.
- **Image:** point `model` at any `diffusers` text-to-image id (`sd15`, SDXL, etc.).
- **Fine-tune base:** any HuggingFace causal-LM id.

No weights/datasets/prompts ever touch the chain.

---

## 6. Honest limits & notes

- **8 GB VRAM** (RTX 4060): ~1–8B quantized LLMs, SD-class image models, LoRA fine-tuning of small models. **Video diffusion and 70B-class models need a bigger GPU** — the code path is ready but this card can't render video in reasonable time.
- **Privacy model:** chain sees only job specs + result hashes. Prompts, models, outputs stay off-chain.
- **Escrow:** the requestor locks funds at job creation; the provider is paid when the result settles. Both sides protected.
- Fine-tuned-answer crispness scales with training steps (a short tune = soft shift; more steps = crisp target).

---

## 7. Reference

### REST endpoints
| Endpoint | Purpose |
|---|---|
| `POST /providers/register` | Advertise your GPU |
| `POST /providers/stake` | Deposit attestation stake |
| `POST /providers/attest` | Prove GPU GFLOPS (required for paid jobs) |
| `POST /providers/withdraw` | Release stake |
| `POST /jobs` | Create a job (LLM / image / video / fine-tune) |
| `POST /jobs/{id}/accept` · `POST /jobs/{id}/start` | Provider runs the job |
| `POST /jobs/{id}/complete` | Settle (escrow release) |
| `GET /job/{id}` · `GET /jobs` | Inspect jobs |
| `GET /job/{id}/result` | Text answer (LLM) + status + hash |
| `GET /job/{id}/artifact` | Download the generated file / adapter |
| `GET /provider/{addr}` | Provider registration + attestation state |

### Job types
`LLMInference` · `ImageGeneration` · `VideoGeneration` · `FineTuning` · `MLInference` · `Custom`

### Handy binaries (`gpn-agent/target/release/`)
`gpn_api`, `gpn_llm_provider`, `gpn_diffusion_provider`, `gpn_finetune_provider`, `gpn_gpu_compute` (measure GFLOPS for attestation), `gpn_faucet` (dev drip).

### Python helpers (in `gpn-agent/`)
`gpn_diffuse.py` (image/video), `gpn_finetune.py` (LoRA), `gpn_servemodel.py` (register tuned adapter into ollama).

---

*GPN dev build · everything local · chain at `ws://127.0.0.1:9944`. Web guide: `http://127.0.0.1:8090/ai-guide.html`.*
