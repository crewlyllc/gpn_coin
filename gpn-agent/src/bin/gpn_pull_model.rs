//! gpn_pull_model — fetch an inference model into the GPN ollama space
//! (`~/.gpn/models`) and register it with ollama.
//!
//! Two modes:
//!
//!   gpn_pull_model pull <model>
//!       Pull a standard ollama model (e.g. `llama3.2:1b`) via `ollama pull`.
//!
//!   gpn_pull_model gguf <hf-repo> <file> [name]
//!       Download a GGUF from HuggingFace (via the `resolve/main/<file>` URL)
//!       and register it with ollama using a generated Modelfile
//!       (`FROM <downloaded .gguf>` → `ollama create`). `name` defaults to the
//!       file basename with the `.gguf` suffix stripped.
//!
//! Everything is sandboxed under `~/.gpn`: the ollama binary is expected at
//! `~/.gpn/ollama/bin/ollama` (see install.sh) and models land in
//! `OLLAMA_MODELS` (default `~/.gpn/models`).
//!
//! Env overrides:
//!   OLLAMA_BIN     path to the ollama binary
//!   OLLAMA_MODELS  model storage dir

use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve the ollama binary: `OLLAMA_BIN`, else `~/.gpn/ollama/bin/ollama` if
/// present, else `ollama` on `PATH`.
fn ollama_bin() -> String {
    if let Ok(b) = std::env::var("OLLAMA_BIN") {
        if !b.is_empty() {
            return b;
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let local = Path::new(&home).join(".gpn/ollama/bin/ollama");
        if local.exists() {
            return local.to_string_lossy().into_owned();
        }
    }
    "ollama".to_string()
}

/// Resolve the models dir: `OLLAMA_MODELS`, else `~/.gpn/models`.
fn models_dir() -> PathBuf {
    if let Ok(d) = std::env::var("OLLAMA_MODELS") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".gpn/models")
}

fn print_usage() {
    eprintln!(
        "usage:\n  gpn_pull_model pull <model>\n  gpn_pull_model gguf <hf-repo> <file> [name]"
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (cmd, rest) = match args.get(1).map(|s| s.as_str()) {
        Some("pull") => ("pull", &args[2..]),
        Some("gguf") => ("gguf", &args[2..]),
        Some("help") | Some("-h") | Some("--help") => {
            print_usage();
            return Ok(());
        }
        _ => {
            print_usage();
            std::process::exit(2);
        }
    };

    let ollama = ollama_bin();
    let models = models_dir();
    std::fs::create_dir_all(&models)?;

    match cmd {
        "pull" => {
            let model = rest.first().map(|s| s.as_str()).unwrap_or_default();
            if model.is_empty() {
                print_usage();
                std::process::exit(2);
            }
            println!("ollama pull {model}");
            let status = Command::new(&ollama)
                .arg("pull")
                .arg(model)
                .env("OLLAMA_MODELS", &models)
                .status()?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }
        "gguf" => {
            let (repo, file) = match (rest.first(), rest.get(1)) {
                (Some(r), Some(f)) => (r.as_str(), f.as_str()),
                _ => {
                    eprintln!("gguf mode needs <hf-repo> and <file>");
                    print_usage();
                    std::process::exit(2);
                }
            };
            let name = rest
                .get(2)
                .cloned()
                .unwrap_or_else(|| {
                    let base = Path::new(file)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| file.to_string());
                    base.trim_end_matches(".gguf").to_string()
                });

            let url = format!("https://huggingface.co/{repo}/resolve/main/{file}");
            let dest = models.join(file);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            download(&url, &dest).await?;
            println!("downloaded {file} → {}", dest.display());

            // Generate a Modelfile and register the GGUF with ollama.
            let modelfile = models.join(format!("{name}.Modelfile"));
            std::fs::write(
                &modelfile,
                format!("FROM \"{}\"\n", dest.to_string_lossy()),
            )?;

            println!("ollama create {name}");
            let status = Command::new(&ollama)
                .arg("create")
                .arg(&name)
                .arg("-f")
                .arg(&modelfile)
                .env("OLLAMA_MODELS", &models)
                .status()?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
            println!("registered {name}");
        }
        _ => unreachable!(),
    }

    Ok(())
}

/// Stream a URL to a local file (memory-bounded — GGUF files can be GBs).
async fn download(url: &str, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    let mut resp = reqwest::get(url).await?;
    if !resp.status().is_success() {
        return Err(format!("download failed: {} for {url}", resp.status()).into());
    }

    let mut file = std::fs::File::create(dest)?;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk)?;
    }
    file.flush()?;
    Ok(())
}
