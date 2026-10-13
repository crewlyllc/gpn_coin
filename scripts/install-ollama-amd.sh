#!/usr/bin/env bash
# install-ollama-amd.sh — bring up ollama on an **AMD/ROCm** box so a GPN node
# can serve LLM jobs (queue item #27).
#
# Background:
#   * A GPN node cannot serve LLM jobs without ollama (http://127.0.0.1:11434).
#   * On NVIDIA, ollama ships CUDA support out of the box. On **Linux + AMD**
#     ollama runs on **ROCm** and only supports a subset of cards (RDNA2/RDNA3
#     — RX 6000 / 7000 series — plus a few older GCN parts). Gas: see
#     https://github.com/ollama/ollama/blob/main/docs/gpu.md
#   * On **Windows**, ollama's ROCm support is limited/experimental — most AMD
#     boxes fall back to **CPU** (correct but slow) or need a DirectML build.
#
# This helper: detects the AMD GPU, warns if ROCm is missing, installs ollama,
# starts it, and pulls the baseline model(s) — with the ROCm pitfalls called out.
#
# Idempotent. Usage:
#   ./scripts/install-ollama-amd.sh
#   GPN_BASELINE_MODELS="llama3.2:1b,qwen2.5:0.5b" ./scripts/install-ollama-amd.sh
#
# Note: the *miner* needs only the OpenCL runtime (rocm-opencl-runtime), NOT
# ROCm-HIP/ollama — mining works even where ollama-ROCm does not.

set -uo pipefail

info() { printf '[ollama-amd] %s\n' "$*"; }
warn() { printf '[ollama-amd] WARN: %s\n' "$*" >&2; }

BASELINE="${GPN_BASELINE_MODELS:-llama3.2:1b,qwen2.5:0.5b}"
ENDPOINT="${OLLAMA_URL:-http://127.0.0.1:11434}"
LOGDIR="${GPN_PREFIX:-$HOME/.gpn}"
mkdir -p "$LOGDIR" 2>/dev/null || true

reachable() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsS --max-time 2 "$ENDPOINT/api/tags" >/dev/null 2>&1
  elif command -v wget >/dev/null 2>&1; then
    wget -q -T 2 -O /dev/null "$ENDPOINT/api/tags" 2>/dev/null
  else
    return 1
  fi
}

# ── 0. detect the AMD GPU + ROCm state ────────────────────────────────────────
GPU_NAME=""
if command -v rocm-smi >/dev/null 2>&1; then
  GPU_NAME="$(rocm-smi --showproductname 2>/dev/null | grep -i 'card series' | head -1 | sed 's/.*: *//')"
fi
if [ -z "$GPU_NAME" ] && command -v lspci >/dev/null 2>&1; then
  GPU_NAME="$(lspci 2>/dev/null | grep -iE 'vga|3d|display' | grep -i 'amd\|ati\|radeon' | head -1 | sed 's/^[^:]*: *//')"
fi
if [ -n "$GPU_NAME" ]; then
  info "detected AMD GPU: $GPU_NAME"
else
  warn "no AMD GPU detected via rocm-smi/lspci — continuing anyway (ollama may use CPU)"
fi

if ! command -v rocm-smi >/dev/null 2>&1; then
  warn "rocm-smi not found → the ROCm runtime is not installed."
  warn "  • Mining only needs the OpenCL ICD:   apt install rocm-opencl-runtime   (then reboot)"
  warn "  • Serving LLMs on the GPU needs ROCm:   see https://rocm.docs.amd.com/"
  warn "  • Without ROCm, ollama will run on the CPU (slow but functional)."
fi

# ── 1. install ollama ─────────────────────────────────────────────────────────
if command -v ollama >/dev/null 2>&1; then
  info "ollama already installed: $(command -v ollama)"
else
  info "installing ollama (official installer) …"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL https://ollama.com/install.sh | sh || warn "ollama install script failed"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- https://ollama.com/install.sh | sh || warn "ollama install script failed"
  else
    warn "no curl/wget — install ollama manually from https://ollama.com/download"
    exit 1
  fi
fi

if ! command -v ollama >/dev/null 2>&1; then
  warn "ollama still not on PATH — install from https://ollama.com/download"
  exit 1
fi

# ── 2. ROCm version tips ──────────────────────────────────────────────────────
# ollama's ROCm wheels ship for specific gfx targets. If your card is a
# consumer RDNA part that ollama's bundled ROCm does not list (or the runtime
# reports a "gfx* not supported" error), export HSA_OVERRIDE_GFX_VERSION to
# pretend to be a supported sibling. Common mappings (set BEFORE `ollama serve`):
#   RX 7900 XTX/XT (gfx1100) → 11.0.0     RX 7800/7700 XT (gfx1101/1102) → 11.0.0
#   RX 6800/6900 (gfx1030)   → 10.3.0     RX 6700 XT (gfx1031) → 10.3.0
#   RX 6600 XT/6650 (gfx1032) → 10.3.0
if [ -z "${HSA_OVERRIDE_GFX_VERSION:-}" ]; then
  info "tip: if ollama errors with 'unsupported gfx', export HSA_OVERRIDE_GFX_VERSION=11.0.0"
  info "     (RX 7000) or =10.3.0 (RX 6000) and restart ollama — see scripts/install-ollama-amd.sh header."
fi

# ── 3. serve ──────────────────────────────────────────────────────────────────
if reachable; then
  info "ollama already serving at $ENDPOINT"
else
  info "starting 'ollama serve' in the background (log: $LOGDIR/ollama.log) …"
  # Pass through a ROCm override if the operator set one.
  HSA_OVERRIDE_GFX_VERSION="${HSA_OVERRIDE_GFX_VERSION:-}" \
    nohup ollama serve >"$LOGDIR/ollama.log" 2>&1 &
  for _ in $(seq 1 30); do
    sleep 1
    reachable && break
  done
  if reachable; then
    info "ollama is up at $ENDPOINT"
    if grep -qiE 'amdgpu|rocm|gfx' "$LOGDIR/ollama.log" 2>/dev/null; then
      info "ollama log mentions ROCm/amdgpu — GPU inference looks active:"
      grep -iE 'amdgpu|rocm|gfx|inference compute' "$LOGDIR/ollama.log" 2>/dev/null | tail -3
    else
      warn "ollama log shows no ROCm/GPU line — it may be running on the CPU (slow)."
      warn "  Check: grep -i 'inference compute' $LOGDIR/ollama.log ; ollama ps"
    fi
  else
    warn "ollama did not come up within 30s — start it manually: ollama serve"
  fi
fi

# ── 4. baseline model(s) ──────────────────────────────────────────────────────
IFS=',' read -ra MODELS <<<"$BASELINE"
for m in "${MODELS[@]}"; do
  m="$(echo "$m" | xargs)"
  [ -z "$m" ] && continue
  info "pulling baseline model: $m"
  ollama pull "$m" || warn "pull failed (will retry when the provider starts): $m"
done

info "done. On AMD the node can serve LLM jobs via ROCm (or CPU fallback)."
info "  Verify GPU use:  ollama ps   (PROCESSOR should say 'ROCm'/'GPU', not '100% CPU')"
