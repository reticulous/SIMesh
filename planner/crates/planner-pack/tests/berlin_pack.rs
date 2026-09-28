//! End-to-end pack build against the real GLO-30 Berlin tile; skips cleanly
//! when the tile isn't in the download cache (`packs/.cache/glo30/`).

use planner_core::geo::Xy;
use planner_pack::build::{build, inspect, BuildParams};
use planner_pack::LayerKind;
use planner_terrain::cog::CogReader;
use std::path::PathBuf;

#[test]
fn small_berlin_pack_builds_and_reads_back() {
    let tile = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packs/.cache/glo30/Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif");
    if !tile.exists() {
        eprintln!("SKIP: GLO-30 tile not downloaded ({})", tile.display());
        return;
    }
    let out = std::env::temp_dir().join("planner_berlin_pack_test");
    let _ = std::fs::remove_dir_all(&out);

    let mut params = BuildParams::berlin_test(vec![tile], out.clone());
    params.half_km = 2.0; // keep the test fast: 134×134 px
    let manifest = build(&params).expect("pack build");
    assert_eq!(manifest.region.crs_epsg, 32633);
    assert!(manifest.layers.iter().any(|l| l.kind == LayerKind::TerrainDtm));
    assert!(manifest.layers.iter().any(|l| l.kind == LayerKind::ClutterHeight));
    assert!(manifest.licenses[0].notice.contains("Copernicus"));

    // Re-inspect from disk and read the split layers back.
    let m2 = inspect(&out).expect("inspect");
    assert_eq!(m2, manifest);
    let mut dtm = CogReader::open(&out.join("dtm.tif")).expect("read dtm layer");
    let mut clut = CogReader::open(&out.join("clutter_h.tif")).expect("read clutter layer");
    let meta = *dtm.meta();
    assert_eq!(meta.dx, 30.0);
    assert_eq!(meta.dy, -30.0);
    // Center of the pack ≈ Alexanderplatz-ish: plausible Berlin terrain band,
    // with a non-negative clutter layer that is nonzero somewhere urban.
    let center = Xy {
        x: meta.origin.x + meta.dx * (meta.width as f64 - 1.0) / 2.0,
        y: meta.origin.y + meta.dy * (meta.height as f64 - 1.0) / 2.0,
    };
    let t = dtm.sample(center).unwrap().unwrap();
    assert!((20.0..=80.0).contains(&t), "central Berlin DTM {t}");
    let mut any_clutter = false;
    for row in (0..meta.height).step_by(7) {
        for col in (0..meta.width).step_by(7) {
            let c = clut.pixel(col, row).unwrap();
            assert!(c >= 0.0, "clutter must be non-negative, got {c}");
            if c > 3.0 {
                any_clutter = true;
            }
        }
    }
    assert!(any_clutter, "an urban pack should contain visible clutter");
}
