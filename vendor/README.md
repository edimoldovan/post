# Vendored crates

- `blitz-paint` — Blitz 0.3.0-beta.3's painter, unchanged except that the text
  selection colour can be set (`set_selection_color`); upstream hard-codes a
  light blue that is unreadable under light text. Used through
  `[patch.crates-io]` in the workspace `Cargo.toml`.
