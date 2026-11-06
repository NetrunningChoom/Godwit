# Godwit art

A bar-tailed godwit in flight, cut out of its sky, plus stylized versions of it.
Kept at full resolution for later use. The app ships only two derivatives:
`assets/art/login-godwit.jpg` (from `01-natural.png`, on the sign-in banner)
and `assets/icon/` (from `08-silhouette-gradient.png`).

## Source

"Bar-tailed godwit in flight" by the U.S. Fish and Wildlife Service (Alaska
Region), Flickr id 53707907003, via Wikimedia Commons:
https://commons.wikimedia.org/wiki/File:Bar-tailed_godwit_in_flight_(53707907003).jpg

Public domain: a work of the U.S. federal government. No credit is required,
but it's given gladly. `source/original.jpg` is the file as downloaded
(4324 × 2884). `source/mask.png` is the cutout mask at that size.

## Files

| File | Style |
|---|---|
| `01-natural.png` | The photo's own colours, sky removed |
| `02-duotone-gold.png` | Indigo to warm gold |
| `03-sunset-gradient.png` | Purple, pink and cream |
| `04-poster.png` | Posterized |
| `05-ink-lines.png` | Light ink strokes |
| `06-oil-paint.png` | Oil paint |
| `07-halftone.png` | Gold halftone dots |
| `08-silhouette-gradient.png` | Lavender-to-peach silhouette |
| `09-neon-outline.png` | Glowing outline |
| `10-moonlit.png` | Violet tint |

`contact-sheet.jpg` shows them all on the sign-in banner's purple.

## Remaking them

`make-art.sh source/original.jpg <out dir>` (needs ImageMagick 7). The sky is
keyed out by blueness (blue minus red); only blue regions connected to the
photo's edges count as sky, so blue-grey shadows in the wings stay.

The variants (this folder) are released with the rest of the repository under
AGPL-3.0-only; the source photo stays in the public domain.
