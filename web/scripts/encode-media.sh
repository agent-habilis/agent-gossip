#!/usr/bin/env bash
# Re-encodes the README screen recordings in ../assets into web-sized MP4s plus
# poster frames. The originals are ~338 MB of high-bitrate 1440p/2228p capture —
# far too heavy to serve, and far more resolution than terminal text needs.
set -euo pipefail

# web/, not scripts/ — SRC and OUT below are written relative to the site root.
cd "$(dirname "$0")/.."

SRC=../assets
OUT=src/video

mkdir -p "$OUT"

# Per-clip CRF and width: the defaults land most clips under 5 MB, but the two
# longest ones (adversarial-review, orchestrate) and the 3568px-wide captures
# (discover, gossip-join, gossip-msg) need their own settings to stay inside
# the page's byte budget.
encode() {
  local name=$1 crf=$2 width=$3

  ffmpeg -y -loglevel error -i "$SRC/$name.mp4" \
    -vf "scale='min($width,iw)':-2:flags=lanczos" \
    -c:v libx264 -preset slow -crf "$crf" -tune stillimage \
    -pix_fmt yuv420p -movflags +faststart -an \
    "$OUT/$name.mp4"

  # Poster is pulled from the encode, not the source, so what a visitor sees
  # before pressing play is exactly the first frame they get after.
  ffmpeg -y -loglevel error -ss 00:00:03 -i "$OUT/$name.mp4" \
    -frames:v 1 -q:v 4 "$OUT/$name.jpg"

  printf '%-32s %s\n' "$name" "$(du -h "$OUT/$name.mp4" | cut -f1)"
}

encode readme-demo 30 1440
encode readme-create-join 30 1440
encode readme-gossip-join 32 1440
encode readme-topic 30 1440
encode readme-gossip-msg 32 1440
encode readme-task 30 1440
encode readme-adversarial-review 32 1440
encode readme-orchestrate 30 1152
encode readme-discover 32 1440

echo "---"
du -sh "$OUT"
