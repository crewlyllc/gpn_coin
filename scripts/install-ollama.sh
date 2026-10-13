#!/usr/bin/env bash
# install-ollama.sh — make ollama a first-class GPN node prerequisite (Linux/macOS).
#
# A GPN node cannot serve LLM jobs without ollama: the provider calls
# http://127.0.0.1:11434. Mining works without it, but every AI request fails.
# This makes the node never fail with a bare
# `pull … failed: error sending request for url (http://127.0.0.1:11434/…)`.
#
# Idempotent:
#   1. installs ollama if missing (official installer script; user-local),
#   2. starts `ollama serve` if it isn't already listening,
#   3. pulls the baseline model(s) so the node offers a usable LLM from day one.
#
# Usage:
#   ./scripts/install-ollama.sh
#   GPN_BASELINE_MODELS="llama3.2:1b,qwen2.5:0.5b" ./scripts/install-ollama.sh

set -uo pipefail

info() { printf '[ollama] %s\n' "$*"; }
warn() { printf '[ollama] WARN: %s\n' "$*" >&2; }

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

# ── 1. install ────────────────────────────────────────────────────────────────
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

# ── 2. serve ──────────────────────────────────────────────────────────────────
if reachable; then
  info "ollama already serving at $ENDPOINT"
else
  info "starting 'ollama serve' in the background (log: $LOGDIR/ollama.log) …"
  nohup ollama serve >"$LOGDIR/ollama.log" 2>&1 &
  for _ in $(seq 1 30); do
    sleep 1
    reachable && break
  done
  if reachable; then
    info "ollama is up at $ENDPOINT"
  else
    warn "ollama did not come up within 30s — start it manually: ollama serve"
  fi
fi

# ── 3. baseline model(s) ──────────────────────────────────────────────────────
IFS=',' read -ra MODELS <<<"$BASELINE"
for m in "${MODELS[@]}"; do
  m="$(echo "$m" | xargs)"
  [ -z "$m" ] && continue
  info "pulling baseline model: $m"
  ollama pull "$m" || warn "pull failed (will retry when the provider starts): $m"
done

info "done. The node can now serve LLM jobs (ollama at $ENDPOINT; baseline: $BASELINE)"
