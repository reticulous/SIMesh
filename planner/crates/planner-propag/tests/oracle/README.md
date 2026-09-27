# P.1812-8 oracle validation

Our implementation is validated **black-box** against the ITU-R WP3K-lineage
reference implementation [eeveetza/Py1812](https://github.com/eeveetza/Py1812)
(tracks P.1812-8, ships validation profiles). We never copy its code or data
files into this repo — we *run* it and compare numbers, which keeps our
Apache-2.0 workspace clean regardless of that repo's license terms.

## Generating vectors (one-time, per machine)

1. Clone Py1812 next to this repo (not inside it).
2. Obtain the ITU digital maps `DN50.TXT` / `N050.TXT` from the ITU-R SG3
   software page (free download; **not redistributable** — never commit them)
   and run Py1812's `initiate_digital_maps.py`.
3. Run `tools/gen_oracle.py` (added together with the port) with
   `uv run python` — it executes Py1812 over its bundled validation profiles
   plus our own synthetic profiles (flat, single knife edge, urban clutter,
   below-clutter terminals, sea/coastal zones) and writes
   `vectors/*.csv` rows: profile hash, params, expected Lb.
4. `cargo test -p planner-propag --test oracle` compares our Lb against the
   vectors. Acceptance gate: |ΔLb| ≤ 0.1 dB on every vector (matching the
   tolerance the Rust ITM ports advertise against NTIA vectors).

`vectors/` is gitignored because its numbers derive from the non-redistributable
ITU maps. CI runs the oracle suite only where the maps are provisioned; the
unit tests of each sub-model (pure math, no maps) always run.
