#!/usr/bin/env python3
"""gpn_servemodel.py — register a fine-tuned LoRA adapter as a servable ollama model.

Reads a JSON request on argv[1]:
{
  "base_model": "HuggingFaceTB/SmolLM2-135M-Instruct",  # HF base id
  "adapter_dir": "/abs/adapter-dir",                    # LoRA adapter output of a FineTune job
  "name": "tuned-123"                                   # ollama model name to register
}
Merges the adapter into the base, converts the merged model to GGUF (q8_0),
registers it with the local ollama server, and prints clean JSON:
  {"ok":true,"name":"tuned-123","gguf":"...","ollama":"tuned-123:latest"}
or {"ok":false,"error":"..."}.
"""
import json, sys, os, subprocess, tempfile, glob

LLAMA_CPP = os.environ.get("LLAMA_CPP", os.path.expanduser("~/llama.cpp"))
OLLAMA = os.environ.get("OLLAMA_BIN", os.path.expanduser("~/ollama/bin/bin/ollama"))

def run(cmd):
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)} -> {p.returncode}: {p.stderr[-400:]}")

def main():
    req = json.load(open(sys.argv[1]))
    try:
        base = req["base_model"]; adapter = req["adapter_dir"]; name = req["name"]
        work = tempfile.mkdtemp(prefix="gpn_serve_")
        merged_dir = os.path.join(work, "merged")
        os.makedirs(merged_dir, exist_ok=True)
        gguf_path = os.path.join(work, name + ".gguf")

        import torch
        from transformers import AutoModelForCausalLM, AutoTokenizer
        from peft import PeftModel
        m = AutoModelForCausalLM.from_pretrained(base, dtype=torch.float16)
        tok = AutoTokenizer.from_pretrained(base)
        merged = PeftModel.from_pretrained(m, adapter).merge_and_unload()
        merged.save_pretrained(merged_dir, safe_serialization=True)
        tok.save_pretrained(merged_dir)

        # Convert to GGUF via llama.cpp.
        run([sys.executable, os.path.join(LLAMA_CPP, "convert_hf_to_gguf.py"),
             merged_dir, "--outfile", gguf_path, "--outtype", "q8_0"])

        # Register with ollama (Modelfile with the fine-tune training template).
        mf = os.path.join(work, "Modelfile")
        open(mf, "w").write(f'FROM {gguf_path}\n\nTEMPLATE """### Instruction\n{{{{ .Prompt }}}}\n### Response\n"""\n')
        run([OLLAMA, "create", name, "-f", mf])

        print(json.dumps({"ok": True, "name": name, "gguf": gguf_path, "ollama": f"{name}:latest"}))
    except Exception as e:
        print(json.dumps({"ok": False, "error": f"{type(e).__name__}: {e}"}))

if __name__ == "__main__":
    main()
