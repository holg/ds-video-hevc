#!/usr/bin/env python3
"""Estimate H.264 -> HEVC (x265 fast, RF 21, ~VMAF 95) savings from the NAS library probe.

Input: library.tsv from ~/dsvideo/scan on the NAS
  size_bytes, duration_s, total_kbps, vcodec, w, h, fps, v_kbps, path
Model: target video bitrate = bpp * width * height * fps, with bpp calibrated on
two measured 1080p films (Der Marsianer 0.080, Aliens 0.084) and scaled by resolution.
"""
import csv, sys, os
from collections import defaultdict

BPP_1080 = 0.085
MIN_SAVING = 0.15          # below this it is not worth a second lossy generation
SKIP_CODECS = {"hevc", "h265", "av1", "vp9"}

def target_kbps(w, h, fps):
    px = w * h
    bpp = BPP_1080 * (px / 2_073_600) ** -0.3   # smaller pictures need more bits per pixel
    return bpp * px * (fps or 25) / 1000

def main(path, out_csv):
    rows, by_folder = [], defaultdict(lambda: [0, 0, 0])   # size, saving, count
    tot_size = tot_save = 0
    for r in csv.reader(open(path, encoding="utf-8", errors="replace"), delimiter="\t"):
        if len(r) < 9: continue
        size, dur, tkb, vc, w, h, fps, vkb = float(r[0]), float(r[1]), float(r[2]), r[3], int(r[4]), int(r[5]), float(r[6]), float(r[7])
        p = r[8]
        tot_size += size
        folder = "/".join(p.split("/")[3:5])
        by_folder[folder][0] += size; by_folder[folder][2] += 1
        if not vkb:                                   # mkv/avi often lack per-stream bitrate
            tkb = tkb or (size * 8 / dur / 1000 if dur else 0)
            vkb = max(tkb * 0.85, tkb - 640)
        tgt = target_kbps(w, h, fps) if w and h else 0
        if vc in SKIP_CODECS or not dur or not tgt or not vkb:
            saving, verdict = 0.0, "skip (hevc)" if vc in SKIP_CODECS else "skip (unknown)"
        else:
            frac = max(0.0, 1 - tgt / vkb)
            saving = frac * vkb * dur * 1000 / 8
            verdict = "convert" if frac >= MIN_SAVING else "keep (low bitrate)"
            if frac < MIN_SAVING: saving = 0.0
        tot_save += saving
        by_folder[folder][1] += saving
        rows.append((saving, size, vkb, tgt, vc, w, h, fps, dur, verdict, p))
    rows.sort(reverse=True)
    with open(out_csv, "w", newline="", encoding="utf-8") as f:
        wr = csv.writer(f)
        wr.writerow(["saving_GB", "size_GB", "new_size_GB", "video_Mbps", "target_Mbps", "codec", "resolution", "fps", "minutes", "verdict", "path"])
        for s, size, vkb, tgt, vc, w, h, fps, dur, v, p in rows:
            wr.writerow([f"{s/1e9:.2f}", f"{size/1e9:.2f}", f"{(size-s)/1e9:.2f}", f"{vkb/1000:.1f}", f"{tgt/1000:.1f}", vc, f"{w}x{h}", f"{fps:g}", f"{dur/60:.0f}", v, p])
    n_conv = sum(1 for r in rows if r[9] == "convert")
    print(f"probed: {len(rows)} files, {tot_size/1e12:.2f} TB")
    print(f"worth converting: {n_conv} files -> saves {tot_save/1e12:.2f} TB ({100*tot_save/tot_size:.0f}%)")
    verdicts = defaultdict(lambda: [0, 0.0])
    for r in rows: verdicts[r[9]][0] += 1; verdicts[r[9]][1] += r[1]
    for v, (n, s) in sorted(verdicts.items()): print(f"  {v:20s} {n:6d} files {s/1e12:6.2f} TB")
    print("by folder:")
    for k, (s, sv, n) in sorted(by_folder.items(), key=lambda kv: -kv[1][0]):
        print(f"  {k:32s} {n:6d} files {s/1e12:6.2f} TB  saving {sv/1e12:5.2f} TB")
    # payoff: GB saved per 10 files, top list
    top = [r for r in rows if r[9] == "convert"]
    for k in (25, 100, 500):
        print(f"top {k:4d} files save {sum(r[0] for r in top[:k])/1e12:.2f} TB")

if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
