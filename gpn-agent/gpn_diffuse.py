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

def model_id(m, kind="image"):
    # Empty/"auto" = caller asked for the default model; resolve it so diffusers
    # never receives the literal string "auto" (which is not a valid HF repo id).
    if not m or m == "auto":
        m = "ltx-video" if kind == "video" else "sd-turbo"
    return {
        "sd-turbo": "stabilityai/sd-turbo",
        "sd15": "runwayml/stable-diffusion-v1-5",
        "sd1.5": "runwayml/stable-diffusion-v1-5",
        # Item #78: the AP's default generation model name is `sd-turbo`, but
        # clients (and the #76 repro) also send `sdxl-turbo`, whose real repo is
        # `stabilityai/sdxl-turbo`. Without the alias the literal string was
        # handed to from_pretrained() and every such job died on a 401.
        "sdxl-turbo": "stabilityai/sdxl-turbo",
        "sdxl": "stabilityai/stable-diffusion-xl-base-1.0",
        "sd21": "stabilityai/stable-diffusion-2-1",
        "sd2.1": "stabilityai/stable-diffusion-2-1",
        # Video alias: the provider/scheduler advertise "ltx-video", and the
        # scheduler emits model="ltx-video" for VideoGeneration. Without this the
        # literal string was handed to from_pretrained() and the job always
        # failed with "Cannot load model ltx-video" (bug).
        "ltx-video": "Lightricks/LTX-Video",
        "ltx": "Lightricks/LTX-Video",
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

def _load_text_encoder_4bit(repo, dtype):
    """4-bit quantize the T5-XXL text encoder so LTX-Video fits a small box.
    T5-XXL (~11B) is the memory hog in LTX-Video: fp16 it is ~22 GB, far beyond
    both an 8 GB GPU and this host's RAM. NF4 4-bit shrinks it to ~5.5 GB. We
    keep it on CPU and only push the (small) transformer+VAE to the GPU, so peak
    VRAM stays ~5-6 GB. Enabled with GPN_DIFFUSE_T5_4BIT=1 (auto when a >=4-bit
    quant loader is importable and the GPU is small).
    """
    import torch
    from transformers import T5EncoderModel, BitsAndBytesConfig
    qc = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=dtype,
        bnb_4bit_use_double_quant=True,
    )
    # device_map={"":"cpu"} is essential: bitsandbytes otherwise stages the
    # (fp32) shards on the GPU to quantize them, which OOMs an 8 GB card before
    # any inference happens. Pinning to CPU keeps only ~2.5 GB of NF4 weights in
    # RAM; the small transformer+VAE still go to the GPU.
    return T5EncoderModel.from_pretrained(
        repo, subfolder="text_encoder", quantization_config=qc, device_map={"": "cpu"}
    )


def _pin_text_encoder_device(pipe, exec_device, out_dtype=None):
    """Keep the text-encoder inputs on the text-encoder's *own* device.

    When the 4-bit T5 lives on CPU but the transformer/VAE run on cuda, the
    stock LTXPipeline moves the tokenized prompt inputs (input_ids /
    attention_mask) to the *execution* device (cuda) before calling the
    CPU-resident encoder, so the embedding `index_select` gets an index on
    cuda:0 with weights on cpu ->
        RuntimeError: Expected all tensors to be on the same device ...

    Wrap `_get_t5_prompt_embeds` so the encoder is always invoked on its own
    device, then hand the resulting embeddings/attention_mask back on the
    execution device (where the pipeline expects them).

    We also force the embedding dtype (out_dtype): a bitsandbytes 4-bit
    module reports `.dtype == float32` (its unquantized norms are fp32), so
    the stock code would cast the embeddings to fp32 and the fp16 transformer
    then fails with "mat1 and mat2 must have the same dtype, got Float and
    Half". Casting to the transformer dtype (fp16) fixes that.
    """
    import types
    te_device = next(pipe.text_encoder.parameters()).device
    orig = pipe._get_t5_prompt_embeds

    def _wrapped(self, prompt=None, num_videos_per_prompt=1,
                 max_sequence_length=128, device=None, dtype=None):
        embeds, mask = orig(
            prompt=prompt,
            num_videos_per_prompt=num_videos_per_prompt,
            max_sequence_length=max_sequence_length,
            device=te_device,
            dtype=dtype or out_dtype,
        )
        return embeds.to(device=exec_device, dtype=out_dtype or embeds.dtype), mask.to(exec_device)

    pipe._get_t5_prompt_embeds = types.MethodType(_wrapped, pipe)


def gen_video(req):
    # Video diffusion is memory-hungry (T5-XXL text encoder + 3D transformer).
    # Plain "load everything in fp16 onto the GPU" OOMs on an 8 GB card; use the
    # memory-fitting path instead. Controlled by GPN_DIFFUSE_T5_4BIT (1/0/auto).
    import torch
    from diffusers import LTXPipeline
    os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
    repo = model_id(req["model"], "video")
    dtype = torch.float16
    use_gpu = torch.cuda.is_available()

    # Decide whether to 4-bit-quantize the text encoder. `auto` engages only on a
    # *small* GPU (total VRAM < 10 GB) so big-GPU hosts keep the exact original
    # unquantized path (no regression); `1` forces it, `0` disables it.
    quant_mode = os.environ.get("GPN_DIFFUSE_T5_4BIT", "auto").lower()
    want_quant = quant_mode == "1"
    if quant_mode == "auto" and use_gpu:
        try:
            total_gb = torch.cuda.get_device_properties(0).total_memory / 1e9
            want_quant = total_gb < 10.0
        except Exception:
            want_quant = False
    text_encoder = None
    if want_quant:
        try:
            text_encoder = _load_text_encoder_4bit(repo, dtype)
        except Exception as e:  # bnb missing / OOM → fall back, don't wedge the job
            if quant_mode == "1":
                raise
            print(f"[gpn_diffuse] 4-bit T5 unavailable ({e}); falling back", file=sys.stderr)
            text_encoder = None

    load_kwargs = dict(torch_dtype=dtype)
    if text_encoder is not None:
        load_kwargs["text_encoder"] = text_encoder
    pipe = LTXPipeline.from_pretrained(repo, **load_kwargs)

    # Cheap VAE memory savers (decode one tile/slice at a time).
    try:
        pipe.vae.enable_slicing()
        pipe.vae.enable_tiling()
    except Exception:
        pass

    if not use_gpu:
        pipe.to("cpu")
    elif text_encoder is not None:
        # 4-bit T5 stays on CPU; move only the (small) transformer+VAE to GPU.
        for name, comp in pipe.components.items():
            if name == "text_encoder" or not isinstance(comp, torch.nn.Module):
                continue
            comp.to("cuda", dtype=dtype if comp.dtype.is_floating_point else None)
        # Prompt inputs must reach the CPU text encoder on CPU (not cuda)
        # while transformer/VAE inputs stay on cuda. See _pin_text_encoder_device.
        _pin_text_encoder_device(pipe, torch.device("cuda"), out_dtype=dtype)
    else:
        pipe.to("cuda", dtype=dtype)

    g = torch.Generator("cuda" if use_gpu else "cpu").manual_seed(req.get("seed", 0))
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
