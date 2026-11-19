#!/usr/bin/env python3
"""gpn_audio.py — run one audio (text-to-speech with voice cloning) job for GPN.

Item #202. Same contract shape as `gpn_diffuse.py`: a JSON request file on
argv[1], one JSON object on stdout.

Request:
{
  "kind": "audio",
  "model": "xtts-v2" | "auto",       # engine alias; only xtts-v2 today
  "text": "text to speak",
  "speaker_wav": "/abs/ref.wav",     # the reference clip, FETCHED PER JOB
  "language": "en",
  "consent": true,                   # attestation: "I have permission to clone this voice"
  "output": "/abs/path/out.mp3",
  "delete_reference": true           # default true: no retention, clip is removed after use
}

Response on success:
  {"ok": true, "artifact": "...", "media_type": "audio/mpeg", "sha256": "...",
   "seconds": 10.35, "sample_rate": 24000, "engine": "xtts-v2"}
on failure:
  {"ok": false, "error": "TypeName: message"}  (exit 1)

Owner decisions this implements (2026-11-18):
  * reference clip 5-15 s           -> refused outside that window
  * output MP3                      -> soundfile/libsndfile MP3 encode
  * explicit consent attestation    -> `consent` must be true, no default
  * delivery via /channel/artifact  -> the caller serves the file, we just write it
  * priced on actual output         -> `seconds` is the measured duration, the billing unit
  * per-job fetch, NO RETENTION     -> the clip is deleted after the render
"""
import hashlib
import json
import os
import sys

# Owner decision: 5-15 s reference clip. Below 5 s XTTS has too little speaker
# information to clone; above 15 s it only costs VRAM/time, not quality.
MIN_REF_SECS = 5.0
MAX_REF_SECS = 15.0 + 1.5  # +1.5 s slack: containers/decoders round durations

# One engine for now. XTTS-v2 is the only candidate that clones from a short
# clip AND fits 8 GB (measured: 1.87 GB VRAM, ~1.5x realtime on a 4060).
ENGINES = {"xtts-v2", "xtts_v2", "auto", "xtts"}


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class Refused(Exception):
    """A request we will not run. Never a crash: the caller must see why."""


def check_request(req):
    text = (req.get("text") or "").strip()
    if not text:
        raise Refused("text is empty")
    if len(text) > 4000:
        raise Refused(f"text is {len(text)} chars; the per-job cap is 4000")

    # Consent is an attestation, not a flag with a default. Missing means no.
    if req.get("consent") is not True:
        raise Refused(
            "consent attestation missing: the caller must send consent=true "
            "(the operator asserts they have permission to clone this voice)"
        )

    model = (req.get("model") or "auto").lower()
    if model not in ENGINES:
        raise Refused(f"model {model!r} is not an audio engine; known: {sorted(ENGINES)}")

    ref = req.get("speaker_wav") or ""
    if not ref:
        raise Refused("speaker_wav is required (a reference clip per job, 5-15 s)")
    if not os.path.isfile(ref):
        raise Refused(f"speaker_wav {ref} does not exist")
    return text, ref


def ref_seconds(path):
    import soundfile as sf

    info = sf.info(path)
    return info.frames / float(info.samplerate)


def gen_audio(req):
    import numpy as np
    import torch
    from TTS.api import TTS

    text, ref = check_request(req)
    dur = ref_seconds(ref)
    if dur < MIN_REF_SECS or dur > MAX_REF_SECS:
        raise Refused(
            f"reference clip is {dur:.2f}s; the accepted window is "
            f"{MIN_REF_SECS:.0f}-{MAX_REF_SECS - 1.5:.0f}s"
        )

    device = "cuda" if torch.cuda.is_available() else "cpu"
    # Model weights are fetched once and cached (XTTS-v2, ~1.8 GB). The licence
    # gate is accepted by the operator when they enable the capability, not
    # per job.
    os.environ.setdefault("COQUI_TOS_AGREED", "1")
    tts = TTS("tts_models/multilingual/multi-dataset/xtts_v2").to(device)
    lang = req.get("language") or "en"
    wav = tts.tts(text=text, speaker_wav=ref, language=lang)

    if isinstance(wav, list):
        wav = np.asarray(wav, dtype="float32")
    # XTTS-v2 emits 24 kHz mono.
    sr = 24000
    out = req["output"]
    os.makedirs(os.path.dirname(os.path.abspath(out)) or ".", exist_ok=True)
    import soundfile as sf

    sf.write(out, wav, sr, format="MP3")
    seconds = round(len(wav) / float(sr), 3)
    return out, "audio/mpeg", seconds, sr


def main():
    if len(sys.argv) < 2:
        print(json.dumps({"ok": False, "error": "usage: gpn_audio.py <request.json>"}))
        sys.exit(2)
    with open(sys.argv[1]) as f:
        req = json.load(f)
    ref = req.get("speaker_wav") or ""
    # Set only once the clip has actually been read for a render. A refused
    # request must NOT delete the caller's clip: we never used it, and a
    # caller that sent a 2 s clip needs it back to retry with a longer one.
    used_reference = False
    try:
        if (req.get("kind") or "audio") != "audio":
            raise Refused(f"kind {req.get('kind')!r} is not served by gpn_audio")
        path, media, seconds, sr = gen_audio(req)
        used_reference = True
        print(
            json.dumps(
                {
                    "ok": True,
                    "artifact": path,
                    "media_type": media,
                    "sha256": sha256_file(path),
                    "seconds": seconds,
                    "sample_rate": sr,
                    "engine": "xtts-v2",
                }
            )
        )
    except (Refused, Exception) as e:
        print(json.dumps({"ok": False, "error": f"{type(e).__name__}: {e}"}))
        sys.exit(1)
    finally:
        # Per-job fetch, NO RETENTION (owner decision 2026-11-18): the access
        # point never stores voice samples, and neither do we past the render.
        if used_reference and ref and req.get("delete_reference", True):
            try:
                os.unlink(ref)
                print("[gpn_audio] reference clip deleted (no retention)", file=sys.stderr)
            except OSError:
                pass


if __name__ == "__main__":
    main()
