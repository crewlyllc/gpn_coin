//! gpn_provider_status — a tiny HTTP heartbeat for a provider, polled by the
//! access-point scheduler to learn capabilities + load + latency.
//!
//! `GET /status` → `{ address, gflops, models[], capabilities[], can_pull, active, capacity, vram_mb, ram_mb, gpu_class, tflops, disk_free_mb, disk_total_mb, disk_free_pct }`
//!
//! `capabilities[]` lists the JOB TYPES this node serves — `LLMInference`,
//! `ImageGeneration`, `VideoGeneration`, `MLInference`, `FineTuning`. A GPN
//! node is equipped to serve **every** job type by design ("one GPU, one job"
//! means one job *at a time* — mutual exclusion — NOT a restriction on job
//! types). Advertised so the scheduler routes every kind of job here. Override
//! with `--capabilities` (env `CAPABILITIES`); a constrained box that opts a
//! provider out narrows the list.
//!
//! `vram_mb`/`ram_mb` advertise this box's hardware so the scheduler can gate a
//! job that declares a VRAM/RAM floor (item 14 — e.g. video needs a big-VRAM box).
//! They are auto-detected (nvidia-smi / /proc/meminfo) and can be overridden with
//! `--vram-mb`/`--ram-mb` (env `VRAM_MB`/`RAM_MB`); 0 = unknown (not advertised).
//!
//! `gpu_class` (`discrete`|`integrated`|`unknown`) + `tflops` advertise whether
//! this box has a real (discrete) GPU (item 19 — a CPU/APU integrated GPU must
//! not earn unless it clears an FP32-TFLOPS floor). Auto-detected (nvidia-smi /
//! lspci on Linux, Win32_VideoController on Windows); override with
//! `--gpu-class`/`--tflops` (env `GPU_CLASS`/`TFLOPS`); tflops 0 = unknown.
//!
//! `models[]` is read LIVE from the local Ollama (`GET /api/tags`), so the network
//! always sees what this machine actually has. `can_pull` = Ollama is reachable,
//! i.e. this provider could download a requested model on demand.
//!
//! Usage:
//!   gpn_provider_status --listen <ip:port> --address <ss58> --gflops <n>
//!                       [--models <m1,m2>] [--capacity <n>] [--active <n>]
//!                       [--ollama http://127.0.0.1:11434]
//!                       [--capabilities <LLMInference,ImageGeneration,…>]
//! Env: LISTEN, ADDRESS, GFLOPS, MODELS, CAPACITY, ACTIVE, GPN_OLLAMA, CAPABILITIES

use gpn_agent::disk::{disk_space, LOW_DISK_PERCENT};
use gpn_agent::gpu::{detect_gpu_capability, detect_vram_mb};
use gpn_agent::llm::{LlmJob, LlmStore};
use serde_json::json;
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server};

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Constant-time string equality (don't leak the shared token via timing).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Authorization for `POST /assign` (item #16, red-team finding #7).
///
/// The provider heartbeat (:8093) is *publicly* reachable — the Windows box
/// forwards it to the internet — and `POST /assign` writes an attacker-supplied
/// job + `callback` into the provider's OWN store. Left unauthenticated, any
/// remote caller could (a) make the provider run an arbitrary prompt on its GPU
/// with no on-chain job/escrow, and (b) overwrite a pending assignment's
/// `callback`/`callback_token` so the provider returns a *user's* inference
/// result to the attacker (result hijack / data exfiltration).
///
/// Bind the push to a shared AP↔provider secret (`GPN_ASSIGN_TOKEN`): when a
/// token is configured, a NON-loopback caller must present it. Loopback stays
/// exempt (the local AP / tests share the secret implicitly) and an *unset*
/// token keeps legacy behaviour so a rollout never breaks an existing box.
fn assign_authorized(is_loopback: bool, required: &str, provided: &str) -> bool {
    if required.is_empty() || is_loopback {
        return true;
    }
    ct_eq(required, provided)
}

/// Live model inventory from Ollama. Returns (models, can_pull).
fn ollama_models(base: &str, fallback: &[String]) -> (Vec<String>, bool) {
    let cli = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(2500))
        .build();
    match cli.and_then(|c| c.get(format!("{base}/api/tags")).send()) {
        Ok(r) => {
            let v: serde_json::Value = r.json().unwrap_or_default();
            let names: Vec<String> = v["models"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                (fallback.to_vec(), true)
            } else {
                (names, true)
            }
        }
        Err(_) => (fallback.to_vec(), false),
    }
}

/// Merge one assignment into the provider's local `~/.gpn/assignments.json` —
/// the file the provider loop's `read_assigned` reads.
fn write_assignment(job_id: u64, rec: serde_json::Value) -> std::io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let path = std::env::var("GPN_ASSIGN_STORE")
        .unwrap_or_else(|_| format!("{home}/.gpn/assignments.json"));
    // Cross-process locked read-modify-write (see `gpn_agent::llm::StoreLock`):
    // the AP + heartbeat + providers share `assignments.json`.
    gpn_agent::llm::merge_json_object(std::path::Path::new(&path), &job_id.to_string(), rec)
}

/// Total system RAM (MiB) from `/proc/meminfo`; 0 if unknown (e.g. Windows).
fn detect_ram_mb() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse()
                    .unwrap_or(0);
                return kb / 1024;
            }
        }
    }
    0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let listen = opt(&args, "--listen")
        .or_else(|| std::env::var("LISTEN").ok())
        .unwrap_or_else(|| "0.0.0.0:8093".into());
    let address = opt(&args, "--address")
        .or_else(|| std::env::var("ADDRESS").ok())
        .unwrap_or_default();
    let gflops: u64 = opt(&args, "--gflops")
        .or_else(|| std::env::var("GFLOPS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let models_csv = opt(&args, "--models")
        .or_else(|| std::env::var("MODELS").ok())
        .unwrap_or_else(|| "llama3.2:1b".into());
    let capacity: u64 = opt(&args, "--capacity")
        .or_else(|| std::env::var("CAPACITY").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let active: u64 = opt(&args, "--active")
        .or_else(|| std::env::var("ACTIVE").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let ollama = opt(&args, "--ollama")
        .or_else(|| std::env::var("GPN_OLLAMA").ok())
        .unwrap_or_else(|| "http://127.0.0.1:11434".into());
    // Job-type capabilities this node serves. A GPN node serves EVERY job type
    // by design ("one GPU, one job" = one job *at a time*, mutual exclusion —
    // NOT a limit on job types). Advertised so the scheduler can route
    // LLM/image/video/ML/fine-tune jobs here. Filtered to what this box
    // actually runs (a constrained box may opt a provider out).
    let caps_default = "LLMInference,ImageGeneration,VideoGeneration,MLInference,FineTuning";
    let capabilities_csv = opt(&args, "--capabilities")
        .or_else(|| std::env::var("CAPABILITIES").ok())
        .unwrap_or_else(|| caps_default.into());
    // Shared AP↔provider secret binding `POST /assign` (item #16 finding #7).
    // Empty ⇒ legacy accept (logged) so a rollout is non-breaking.
    let assign_token = opt(&args, "--assign-token")
        .or_else(|| std::env::var("GPN_ASSIGN_TOKEN").ok())
        .unwrap_or_default();
    // Real compute economics for the rate card:
    //   --tok-per-s          the provider's MEASURED throughput (tokens/sec)
    //   --owner-cost-usd-hr  the provider's own cost/hr ($/kWh + amortized GPU)
    let tok_per_s: f64 = opt(&args, "--tok-per-s")
        .or_else(|| std::env::var("GPN_TOK_PER_S").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(40.0);
    let owner_cost_usd_hr: f64 = opt(&args, "--owner-cost-usd-hr")
        .or_else(|| std::env::var("GPN_OWNER_COST_USD_HR").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.03);
    // Memory bandwidth (GB/s) — the other half of the roofline. A hardware spec.
    let mem_bw_gbps: f64 = opt(&args, "--mem-bw-gbps")
        .or_else(|| std::env::var("GPN_MEM_BW_GBPS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(250.0);
    // Hardware capability (item 14): advertise VRAM/RAM so a job's `min_vram_mb`/
    // `min_ram_mb` floor can be enforced *off-chain* by the scheduler. Auto-detected;
    // 0 = unknown (explicitly NOT filtered against, so pre-item-14 boxes keep working).
    let vram_mb: u64 = opt(&args, "--vram-mb")
        .or_else(|| std::env::var("VRAM_MB").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(detect_vram_mb);
    let ram_mb: u64 = opt(&args, "--ram-mb")
        .or_else(|| std::env::var("RAM_MB").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(detect_ram_mb);
    // Minimum GPU capability (item 19): advertise the GPU *class* + an FP32
    // TFLOPS estimate so the scheduler can gate a job's `min_tflops` floor and
    // reject CPU/APU integrated graphics. Auto-detected; overridable.
    let (gpu_class, tflops_detected) = detect_gpu_capability();
    let gpu_class: String = opt(&args, "--gpu-class")
        .or_else(|| std::env::var("GPU_CLASS").ok())
        .unwrap_or(gpu_class);
    let tflops: f64 = opt(&args, "--tflops")
        .or_else(|| std::env::var("TFLOPS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(tflops_detected);
    let models: Vec<String> = models_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let capabilities: Vec<String> = capabilities_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    println!("GPN provider heartbeat on http://{listen} — address={address} gflops={gflops} models={:?} active={active}/{capacity} capabilities={:?}", models, capabilities);

    // Item #43: warn LOUDLY at startup if the filesystem holding our data is
    // nearly full — a full disk silently halts the chain (2026-10-19 ENOSPC
    // block-persist failure). 0 = unknown, never treated as full.
    let disk_path = std::env::var("GPN_DATA_DIR").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    let boot_disk = disk_space(&disk_path);
    if let Some(pct) = boot_disk.free_percent() {
        if pct < LOW_DISK_PERCENT {
            eprintln!(
                "⚠ LOW DISK: {pct}% free on {disk_path} ({} MiB of {} MiB) — below {LOW_DISK_PERCENT}%; the chain will halt if this reaches 0 (see docs/run-a-node.md \"Disk headroom\")",
                boot_disk.free_mb, boot_disk.total_mb
            );
        }
    }

    let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    if assign_token.is_empty() {
        eprintln!("⚠ assign-token not configured — POST /assign is UNAUTHENTICATED (set GPN_ASSIGN_TOKEN; item #16 finding #7)");
    }
    for mut req in server.incoming_requests() {
        // Auth gate for the assignment push (item #16, red-team finding #7): a
        // publicly-reachable `POST /assign` must carry the shared token.
        {
            let u = req.url().to_string();
            let is_assign = req.method() == &Method::Post && u.split('?').next() == Some("/assign");
            if is_assign {
                let is_loopback = req
                    .remote_addr()
                    .map(|a| a.ip().is_loopback())
                    .unwrap_or(false);
                let provided = req
                    .headers()
                    .iter()
                    .find_map(|h| {
                        if h.field.equiv("X-Gpn-Assign-Token") {
                            Some(h.value.as_str().trim().to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                if !assign_authorized(is_loopback, &assign_token, &provided) {
                    let _ = req.respond(
                        Response::from_string(json!({"error":"invalid assign token"}).to_string())
                            .with_header(hdr.clone())
                            .with_status_code(401),
                    );
                    continue;
                }
            }
        }
        let body = match (req.method(), req.url()) {
            (Method::Get, "/status") => {
                let (models_now, can_pull) = ollama_models(&ollama, &models);
                // Item #43: report free space so a monitor can alert before
                // ENOSPC silently halts block production. 0/null = unknown.
                let disk = disk_space(&std::env::var("GPN_DATA_DIR").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into()));
                json!({
                    "address": address, "gflops": gflops, "models": models_now,
                    "can_pull": can_pull, "active": active, "capacity": capacity.max(1),
                    "capabilities": capabilities,
                    "tok_per_s": tok_per_s, "owner_cost_usd_hr": owner_cost_usd_hr,
                    "mem_bw_gbps": mem_bw_gbps, "vram_mb": vram_mb, "ram_mb": ram_mb,
                    "gpu_class": gpu_class, "tflops": tflops,
                    "disk_free_mb": disk.free_mb, "disk_total_mb": disk.total_mb,
                    "disk_free_pct": disk.free_percent()
                })
            }
            (Method::Get, "/health") => json!({"status":"ok"}),
            // POST /assign — the access point PUSHES a job assignment for this provider.
            // We write it into the provider's OWN ~/.gpn/assignments.json (what the
            // provider loop reads), so a REMOTE provider gets its assignment without
            // needing to see the AP's host-local file.
            (Method::Post, "/assign") => {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(v) => {
                        let jid = v.get("job_id").and_then(|x| x.as_u64()).unwrap_or(0);
                        let callback = v.get("callback").and_then(|x| x.as_str()).unwrap_or("");
                        // Item #14: the assignment now carries the FULL off-chain job
                        // payload. Persist it into THIS provider's own LlmStore so a
                        // REMOTE provider (which never created the job and has no local
                        // entry) can actually RUN it. Without this the provider claims
                        // but `run_job` finds no `get_job(id)` and skips → the job
                        // wedges Running. The `callback` is stored so the provider can
                        // POST the result back to the requesting AP.
                        let mut stored_job = false;
                        if let Some(jv) = v.get("job") {
                            if let Ok(job) = serde_json::from_value::<LlmJob>(jv.clone()) {
                                stored_job = LlmStore::new().put(jid, job).is_ok();
                            }
                        }
                        // Item #16 finding #2: persist the per-job callback token so
                        // the provider can echo it when returning the result, binding
                        // the remote result channel to this assignment.
                        let callback_token = v
                            .get("callback_token")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        let rec = json!({
                            "provider": v.get("provider").and_then(|x| x.as_str()).unwrap_or(address.as_str()),
                            "model": v.get("model").and_then(|x| x.as_str()).unwrap_or(""),
                            "pull": v.get("pull").and_then(|x| x.as_bool()).unwrap_or(false),
                            "callback": callback,
                            "callback_token": callback_token,
                        });
                        match write_assignment(jid, rec) {
                            Ok(_) => json!({"ok": true, "job_id": jid, "job_stored": stored_job}),
                            Err(e) => json!({"error": format!("{e}")}),
                        }
                    }
                    Err(e) => json!({"error": format!("bad body: {e}")}),
                }
            }
            _ => json!({"error":"not found"}),
        };
        let _ = req.respond(Response::from_string(body.to_string()).with_header(hdr.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_token_gate_rejects_remote_callers_with_the_wrong_or_missing_token() {
        // Token configured: a non-loopback caller must present it exactly.
        assert!(!assign_authorized(false, "s3cret", ""));
        assert!(!assign_authorized(false, "s3cret", "wrong"));
        assert!(assign_authorized(false, "s3cret", "s3cret"));
        // Loopback stays exempt so the local AP / tests keep working.
        assert!(assign_authorized(true, "s3cret", ""));
    }

    #[test]
    fn assign_token_gate_is_inert_when_unconfigured() {
        // Rollout safety: no token configured ⇒ legacy (accept) behaviour.
        assert!(assign_authorized(false, "", ""));
        assert!(assign_authorized(false, "", "anything"));
    }

    #[test]
    fn ct_eq_matches_only_equal_same_length_strings() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(ct_eq("", ""));
    }
}
