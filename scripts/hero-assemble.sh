#!/usr/bin/env bash
# Cut a raw take (scripts/hero-receipt.tape → target/hero/take.mp4) into the
# README hero, docs/img/hero-receipt.gif.
#
#   scripts/hero-assemble.sh [take.mp4]
#
# The cut changes time, never content: the typing plays at real speed, the
# stretch where the agent works plays faster under a label that says so, the
# answer it settles on is held still, and a card with the install line follows.
# No frame of the session is drawn over except by that label.
#
# ENTER_AT is when the prompt is sent (fixed by the tape's typing); ANSWER_AT is
# when the screen last settles, read off ffmpeg's freezedetect. Override either
# if a take needs it.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
take=${1:-$root/target/hero/take.mp4}
work=$root/target/hero/cut
out=$root/docs/img/hero-receipt.gif
font=${HERO_FONT:-$(fc-match -f '%{file}' 'DejaVu Sans Mono')}
speed=8
fps=12
bg=0x1e1e2e   # Catppuccin Mocha base, the tape's theme

enter_at=${ENTER_AT:-4.0}
answer_at=${ANSWER_AT:-$(ffmpeg -hide_banner -i "$take" -vf freezedetect=n=0.001:d=0.5 \
  -map 0:v -f null - 2>&1 | sed -n 's/.*freeze_start: //p' | tail -1)}
[ -n "$answer_at" ] || { echo "hero-assemble: no settled answer in $take" >&2; exit 1; }
read -r w h < <(ffprobe -v error -select_streams v:0 -show_entries stream=width,height \
  -of csv=s=x:p=0 "$take" | tr x ' ')

rm -rf "$work"; mkdir -p "$work"
enc=(-an -r "$fps" -pix_fmt yuv444p -c:v libx264 -crf 12 -preset veryfast)

# The label and the card are drawn with Pillow: a stock ffmpeg often lacks
# drawtext (it needs libfreetype), and Pillow is already what the demo tooling
# leans on.
python3 - "$work" "$w" "$h" "$font" "$speed" <<'EOF'
import sys
from PIL import Image, ImageDraw, ImageFont
work, w, h, font, speed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4], sys.argv[5]
BASE, TEXT, SUB, GREEN, DIM, YELLOW = "#1e1e2e", "#cdd6f4", "#a6adc8", "#a6e3a1", "#7f849c", "#f9e2af"
f = lambda size: ImageFont.truetype(font, size)

label = f"sped up {speed}×"
lf = f(20)
x0, y0, x1, y1 = lf.getbbox(label)
pad = 9
img = Image.new("RGBA", (x1 + 2 * pad, y1 + 2 * pad), YELLOW)
ImageDraw.Draw(img).text((pad, pad), label, font=lf, fill=BASE)
img.save(f"{work}/label.png")

card = Image.new("RGB", (w, h), BASE)
d = ImageDraw.Draw(card)
def centred(y, text, size, fill):
    ft = f(size)
    d.text(((w - d.textlength(text, font=ft)) / 2, y), text, font=ft, fill=fill)
centred(h / 2 - 90, "Your agent checks the receipt", 34, TEXT)
centred(h / 2 - 45, "before it says done.", 34, TEXT)
centred(h / 2 + 20, "tasqx: a backlog, a memory and a brief for your coding agent", 20, SUB)
centred(h / 2 + 80, "brew install dimitritholen/tasqx/tasqx", 28, GREEN)
centred(h - 60, "Unedited Claude Code session on invented demo data.", 16, DIM)
centred(h - 36, "Only the waiting is sped up.", 16, DIM)
card.save(f"{work}/card.png")
EOF

# 1. The prompt, typed at real speed.
ffmpeg -loglevel error -y -i "$take" -to "$enter_at" "${enc[@]}" "$work/1.mp4"
# 2. The agent working, sped up and labelled as such.
ffmpeg -loglevel error -y -ss "$enter_at" -to "$answer_at" -i "$take" -i "$work/label.png" \
  -filter_complex "[0:v]setpts=PTS/$speed[v];[v][1:v]overlay=W-w-24:20" "${enc[@]}" "$work/2.mp4"
# 3. The answer, held long enough to read.
ffmpeg -loglevel error -y -ss "$answer_at" -i "$take" \
  -vf "tpad=stop_mode=clone:stop_duration=6,trim=duration=6.5" "${enc[@]}" "$work/3.mp4"
# 4. The card: what it is and how to get it, fading to the base colour so the
#    loop restarts on the same background the take opens on.
ffmpeg -loglevel error -y -loop 1 -t 3.5 -i "$work/card.png" \
  -vf "fade=t=out:st=3.1:d=0.4:color=$bg" "${enc[@]}" "$work/4.mp4"

printf "file '%s'\n" "$work"/{1,2,3,4}.mp4 >"$work/list.txt"
ffmpeg -loglevel error -y -f concat -safe 0 -i "$work/list.txt" -c copy "$work/hero.mp4"

ffmpeg -loglevel error -y -i "$work/hero.mp4" \
  -vf "fps=$fps,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle" \
  "$work/hero.gif"
gifsicle -O3 --lossy=40 "$work/hero.gif" -o "$out"

dur=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$work/hero.mp4")
echo "$out  ${dur}s  $(du -h "$out" | cut -f1)  (enter $enter_at, answer $answer_at)"
