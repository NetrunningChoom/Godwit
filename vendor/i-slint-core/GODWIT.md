# i-slint-core 1.18.1, vendored for Godwit

Upstream: https://github.com/slint-ui/slint (Slint's `internal/core`, same
version as the `slint` crates this workspace pins, see LICENSE).
Used through `[patch.crates-io]` in the workspace `Cargo.toml`. Redo any
change here when upgrading Slint.

Changes from the published 1.18.1:

- `items/text.rs`: cursor movement between visual lines (`NextLine` /
  `PreviousLine`, the Up/Down keys in the composer). Upstream steps one
  space's height at a time, taken from the text's first font; Godwit's
  composer sets the bundled Noto Emoji face first, which has no space, so
  the step is 0 and the cursor never leaves its line. The Godwit patch
  steps a fraction of the line's height instead (the cursor's own height,
  falling back to the space's) and retries until the byte offset changes,
  so any first font works. The fraction is 0.71, not 0.5: exactly half a
  line lands on the boundary between two lines, where the hit-test can
  keep the same byte offset (with another affinity) and read as "didn't
  move" — which broke CI, whose runners have no text font, only Noto
  Color Emoji, so the space's height is 0 there too.
