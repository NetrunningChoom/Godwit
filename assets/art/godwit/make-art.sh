#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Godwit contributors
# Cuts the godwit out of its sky and renders the stylized variants, at full
# resolution. Usage: make-art.sh <original.jpg> <out dir>
set -euo pipefail
SRC=$1
OUT=$2
W=$(mktemp -d -p "${TMPDIR:-/tmp}")
trap 'rm -rf "$W"' EXIT
mkdir -p "$OUT/source"
cd "$W"
magick "$SRC" -strip work.png
read -r IW IH < <(magick identify -format "%w %h\n" work.png)
# Blueness (blue minus red): the sky is ~80/255, the bird ~0.
magick work.png -fx "b-r" -colorspace gray diff.png
# Sky = blue pixels connected to the photo's edges; holes inside the bird stay.
magick diff.png -threshold 22% -colorspace sRGB -type TrueColor -fill red \
  -draw "color 0,0 floodfill" -draw "color $((IW-1)),0 floodfill" \
  -draw "color 0,$((IH-1)) floodfill" -draw "color $((IW-1)),$((IH-1)) floodfill" \
  -fill white +opaque red -fill black -opaque red -colorspace gray birdmask.png
magick birdmask.png -morphology Erode Disk:4 interior.png
magick birdmask.png -morphology Dilate Disk:4 outer.png
magick diff.png -level 40x78 -negate soft.png
magick soft.png interior.png -compose Lighten -composite outer.png -compose Multiply -composite \
  -blur 0x1 -background black -alpha remove -alpha off mask.png
magick work.png mask.png -alpha off -compose CopyAlpha -composite -compose Over -trim +repage \
  -bordercolor none -border 70 cut.png
read -r CW CH < <(magick identify -format "%w %h\n" cut.png)
magick cut.png -alpha extract A.png
magick cut.png -background "#808080" -alpha remove -alpha off RGB.png
fin() { magick "$1" A.png -alpha off -compose CopyAlpha -composite "$OUT/$2.png"; }

cp cut.png "$OUT/01-natural.png"
magick RGB.png -colorspace gray -sigmoidal-contrast 4x50% +level-colors "#1d1633,#ffd9a0" t.png
fin t.png 02-duotone-gold
magick -size 1x128 gradient:"#2a1f5c"-"#c86dd7" \( -size 1x128 gradient:"#c86dd7"-"#fff1c9" \) -append clut.png
magick RGB.png -colorspace gray -sigmoidal-contrast 3x50% clut.png -clut t.png
fin t.png 03-sunset-gradient
magick RGB.png -modulate 100,140 -posterize 5 -statistic Median 5x5 t.png
fin t.png 04-poster
# Ink lines: the difference of two blurs, as light strokes on transparency.
magick RGB.png -colorspace gray \( +clone -blur 0x6 \) -compose Difference -composite \
  -level 2%,18% -colorspace gray lines.png
magick lines.png A.png -compose Multiply -composite lineA.png
magick -size "${CW}x${CH}" xc:"#f4e9ff" lineA.png -alpha off -compose CopyAlpha -composite "$OUT/05-ink-lines.png"
magick RGB.png -paint 9 -modulate 105,125 t.png
fin t.png 06-oil-paint
magick RGB.png -colorspace gray -ordered-dither h8x8o +level-colors "#2a2240,#ffd9a0" t.png
fin t.png 07-halftone
magick -size "${CW}x${CH}" gradient:"#b9a6ff"-"#ffc9a6" t.png
fin t.png 08-silhouette-gradient
magick A.png -threshold 50% -morphology EdgeOut Disk:6 -blur 0x2 edge.png
magick edge.png \( +clone -blur 0x24 -level 0,40% \) -compose Lighten -composite glowA.png
magick -size "${CW}x${CH}" xc:"#e7b8ff" glowA.png -alpha off -compose CopyAlpha -composite "$OUT/09-neon-outline.png"
magick RGB.png -colorspace gray -sigmoidal-contrast 5x55% \( -size "${CW}x${CH}" xc:"#2a2240" \) \
  -compose Screen -composite -fill "#b48cff" -tint 60 t.png
fin t.png 10-moonlit

cp "$SRC" "$OUT/source/original.jpg"
cp mask.png "$OUT/source/mask.png"
magick montage "$OUT"/[0-9]*.png -background "#2a2240" -geometry 300x420+10+10 -tile 5x \
  -fill white -pointsize 14 -label '%t' "$OUT/contact-sheet.jpg"
