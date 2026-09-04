#!/usr/bin/env python3
"""gpn_finetune.py — run one on-network LoRA fine-tune job for GPN.

Reads a JSON request on argv[1]:
{
  "model": "HuggingFaceTB/SmolLM2-135M-Instruct",   # base model (HF id)
  "dataset": [ {"prompt": "...", "completion": "..."}, ... ],   # few-shot examples
  "output": "/abs/adapter-dir",       # dir where the LoRA adapter is saved
  "lora_r": 16, "lora_alpha": 32, "epochs": 2, "lr": 2e-4, "max_steps": 60
}
Writes JSON to stdout on success:
  {"ok":true,"artifact":"...","media_type":"application/zip","sha256":"...",
   "before":"...","after":"...","loss": 0.42}
or {"ok":false,"error":"..."}.

Runs on the local GPU via HF transformers + PEFT LoRA. Uses an in-memory
dataset (no `datasets` dep). Saves the LoRA adapter + a before/after sample to
prove it learned.
"""
import json, sys, os, hashlib, zipfile

def sha256_dir(dirpath):
    # hash all files under dirpath (deterministic order)
    h = hashlib.sha256()
    for root, _, files in sorted(os.walk(dirpath)):
        for fn in sorted(files):
            p = os.path.join(root, fn)
            rel = os.path.relpath(p, dirpath)
            h.update(rel.encode())
            with open(p, "rb") as f:
                h.update(f.read())
    return h.hexdigest()

def zip_dir(dirpath, zippath):
    with zipfile.ZipFile(zippath, "w", zipfile.ZIP_DEFLATED) as z:
        for root, _, files in os.walk(dirpath):
            for fn in files:
                p = os.path.join(root, fn)
                z.write(p, os.path.relpath(p, dirpath))

def gen(model, tok, text, max_new=40):
    import torch
    ids = tok(text, return_tensors="pt", truncation=True, max_length=200).to(model.device)
    with torch.no_grad():
        out = model.generate(**ids, max_new_tokens=max_new, do_sample=False)
    return tok.decode(out[0][ids["input_ids"].shape[1]:], skip_special_tokens=True)

def main():
    req = json.load(open(sys.argv[1]))
    out_dir = req["output"]
    os.makedirs(out_dir, exist_ok=True)

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer, Trainer, TrainingArguments
    from peft import LoraConfig, get_peft_model

    mid = req["model"]
    data = req.get("dataset", [])
    if not data:
        print(json.dumps({"ok": False, "error": "empty dataset"})); return

    tok = AutoTokenizer.from_pretrained(mid)
    if tok.pad_token is None:
        tok.pad_token = tok.eos_token
    model = AutoModelForCausalLM.from_pretrained(mid, torch_dtype=torch.float16).to("cuda")

    def fmt(ex):
        return f"### Instruction\n{ex['prompt']}\n### Response\n{ex['completion']}"
    # Before sample on the first example's prompt (held out the completion).
    before = gen(model, tok, f"### Instruction\n{data[0]['prompt']}\n### Response\n")

    # Tokenize the training set.
    texts = [fmt(ex) + tok.eos_token for ex in data]
    enc = tok(texts, padding=True, truncation=True, max_length=256, return_tensors="pt")
    labels = enc["input_ids"].clone()
    labels[enc["attention_mask"] == 0] = -100
    class DS(torch.utils.data.Dataset):
        def __len__(self): return len(enc["input_ids"])
        def __getitem__(self, i):
            return {"input_ids": enc["input_ids"][i], "attention_mask": enc["attention_mask"][i], "labels": labels[i]}
    ds = DS()

    lora = LoraConfig(r=req.get("lora_r", 16), lora_alpha=req.get("lora_alpha", 32),
                      lora_dropout=0.05, bias="none", task_type="CAUSAL_LM")
    model = get_peft_model(model, lora)
    model.enable_input_require_grads()

    args = TrainingArguments(
        output_dir=os.path.join(out_dir, "ckpt"),
        per_device_train_batch_size=1, gradient_accumulation_steps=1,
        learning_rate=req.get("lr", 2e-4), num_train_epochs=req.get("epochs", 2),
        max_steps=req.get("max_steps", 60), logging_steps=10, report_to=[],
        fp16=True, save_total_limit=1, remove_unused_columns=False,
    )
    tr = Trainer(model=model, args=args, train_dataset=ds)
    result = tr.train()
    loss = float(result.training_loss)

    after = gen(model, tok, f"### Instruction\n{data[0]['prompt']}\n### Response\n")

    # Save the adapter into out_dir.
    model.save_pretrained(out_dir)
    tok.save_pretrained(out_dir)

    zippath = out_dir + ".zip"
    zip_dir(out_dir, zippath)
    hashv = sha256_dir(out_dir)

    print(json.dumps({"ok": True, "artifact": zippath, "media_type": "application/zip",
                      "sha256": hashv, "loss": round(loss, 4),
                      "before": before.strip(), "after": after.strip()}, ensure_ascii=False))

if __name__ == "__main__":
    main()
