"""Assembles demo/marks.json + frames + s*.mp3 into LeashPay_demo.mp4 (1280x720, H.264/AAC).
Stretches where the browser waited on testnet after a narration line ended are sped up (x1/SPEED)."""
import json, subprocess
SPEED = 0.3   # keep 30% of "waiting for the chain" time after narration ends
m = json.load(open("marks.json")); frames = m["frames"]; end = m["end"]; marks = m["marks"]; N = len(marks)
durs = [float(subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", f"s{i}.mp3"]).decode()) for i in range(N)]
bounds = marks + [end]
segs = []  # (t0, t1, factor)
for i in range(N):
    narr_end = min(bounds[i] + durs[i] + 0.6, bounds[i + 1])
    segs.append((bounds[i], narr_end, 1.0))
    if bounds[i + 1] > narr_end: segs.append((narr_end, bounds[i + 1], SPEED))

def remap(t):
    out = 0.0
    for a, b, f in segs:
        if t <= a: break
        out += (min(t, b) - a) * f
    return out

new_end = remap(end)
with open("frames.txt", "w") as fh:
    for i, (t, fn) in enumerate(frames):
        nxt = frames[i + 1][0] if i + 1 < len(frames) else end
        fh.write(f"file '{fn}'\nduration {max(remap(nxt) - remap(t), 0.001):.4f}\n")
    fh.write(f"file '{frames[-1][1]}'\n")
ins, flt = [], []
for i, t in enumerate(marks):
    ins += ["-i", f"s{i}.mp3"]; d = int(remap(t) * 1000) + 250
    flt.append(f"[{i+1}:a]adelay={d}|{d}[a{i}]")
flt.append("".join(f"[a{i}]" for i in range(N)) + f"amix=inputs={N}:normalize=0,apad[aout]")
cmd = ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", "frames.txt", *ins, "-filter_complex", ";".join(flt),
       "-map", "0:v", "-map", "[aout]", "-vf", "scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2,fps=30,format=yuv420p",
       "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-c:a", "aac", "-b:a", "160k", "-t", f"{new_end:.2f}", "-movflags", "+faststart", "LeashPay_demo.mp4"]
subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print("ok", round(end, 1), "->", round(new_end, 1))
