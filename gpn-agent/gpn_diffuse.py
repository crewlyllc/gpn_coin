#!/usr/bin/env python3
"""gpn_diffuse.py — run one image/video generation job for the GPN network.

Reads a JSON request on argv[1]:
{
  "kind": "image" | "video",
  "model": "sd-turbo" | HF id,       # image; video model id for video
  "prompt": "...",
  "negative_prompt": "..." | null,
  "width": 512, "height": 512,
  "steps": 8, "seed": 123,
  "frames": 16, "fps": 8,             # video
  "output": "/abs/path/out.png|.mp4"
}
Writes JSON to stdout on success:
  {"ok": true, "artifact": "...", "media_type": "image/png", "sha256": "..."}
or {"ok": false, "error": "..."} on failure.

Runs entirely on the local GPU via HuggingFace diffusers (CUDA).
"""
import json, sys, hashlib, os

def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()

def model_id(m):
    return {
        "sd-turbo": "stabilityai/sd-turbo",
        "sd15": "runwayml/stable-diffusion-v1-5",
        "sd1.5": "runwayml/stable-diffusion-v1-5",
    }.get(m, m)

def gen_image(req):
    import torch
    from diffusers import AutoPipelineForText2Image
    os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
    pipe = AutoPipelineForText2Image.from_pretrained(
        model_id(req["model"]), torch_dtype=torch.float16, safety_checker=None
    ).to("cuda")
    g = torch.Generator("cuda").manual_seed(req.get("seed", 0))
    img = pipe(
        prompt=req["prompt"],
        negative_prompt=req.get("negative_prompt"),
        width=req.get("width", 512), height=req.get("height", 512),
        num_inference_steps=req.get("steps", 8),
        guidance_scale=req.get("guidance", 0.0) if req.get("steps", 8) <= 8 else 7.5,
        generator=g,
    ).images[0]
    img.save(req["output"])
    return req["output"], "image/png"

def gen_video(req):
    # Video diffusion is VRAM-hungry; on 8 GB only small/short clips are viable.
    import torch
    from diffusers import LTXPipeline
    os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
    pipe = LTXPipeline.from_pretrained(model_id(req["model"]), torch_dtype=torch.float16).to("cuda")
    g = torch.Generator("cuda").manual_seed(req.get("seed", 0))
    frames = pipe(
        prompt=req["prompt"],
        negative_prompt=req.get("negative_prompt"),
        width=req.get("width", 512), height=req.get("height", 512),
        num_frames=req.get("frames", 16),
        num_inference_steps=req.get("steps", 30),
        generator=g,
    ).frames[0]
    from diffusers.utils import export_to_video
    export_to_video(frames, req["output"], fps=req.get("fps", 8))
    return req["output"], "video/mp4"

def main():
    with open(sys.argv[1]) as f:
        req = json.load(f)
    try:
        out = req["output"]
        if req["kind"] == "image":
            path, media = gen_image(req)
        else:
            path, media = gen_video(req)
        print(json.dumps({"ok": True, "artifact": path, "media_type": media, "sha256": sha256_file(path)}))
    except Exception as e:
        print(json.dumps({"ok": False, "error": f"{type(e).__name__}: {e}"}))
        sys.exit(1)

if __name__ == "__main__":
    main()
