//! Smoke test against a real Copernicus GLO-30 COG (tiled + DEFLATE +
//! overview IFDs). Runs only when the tile is in the download cache
//! (`packs/.cache/glo30/`) — CI without it stays green.

use planner_core::geo::Xy;
use planner_terrain::cog::CogReader;
use std::path::PathBuf;

fn tile_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../packs/.cache/glo30/Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif")
}

#[test]
fn glo30_berlin_tile_reads_plausibly() {
    let path = tile_path();
    if !path.exists() {
        eprintln!("SKIP: GLO-30 tile not downloaded ({})", path.display());
        return;
    }
    let mut cog = CogReader::open(&path).expect("open real GLO-30 COG");
    let m = *cog.meta();
    // 1°×1° tile at 52°N: 3600 rows, 2400 cols (1.5″ lon spacing).
    assert_eq!(m.height, 3600, "rows");
    assert_eq!(m.width, 2400, "cols");
    assert!(m.dy < 0.0, "north-up");
    assert!((m.dx - 1.5 / 3600.0).abs() < 1e-9, "1.5 arcsec lon step, got {}", m.dx);

    // Alexanderplatz ≈ (13.4132 E, 52.5219 N): surface incl. buildings —
    // plausible band 30–90 m (terrain ~37 m + structures).
    let alex = cog.sample(Xy { x: 13.4132, y: 52.5219 }).unwrap().unwrap();
    assert!((30.0..=90.0).contains(&alex), "Alexanderplatz DSM {alex}");

    // Müggelberge (~13.64 E, 52.416 N): highest natural rise in Berlin,
    // ~85–115 m + canopy.
    let muegg = cog.sample(Xy { x: 13.6400, y: 52.4160 }).unwrap().unwrap();
    assert!((60.0..=140.0).contains(&muegg), "Müggelberge DSM {muegg}");
    assert!(muegg > alex, "hills above downtown: {muegg} vs {alex}");

    // A window materializes and stays in a plausible elevation band.
    let g = cog
        .window(Xy { x: 13.30, y: 52.55 }, Xy { x: 13.50, y: 52.45 })
        .unwrap();
    assert!(g.width > 400 && g.height > 300);
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for &v in &g.data {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    assert!(lo > 10.0 && hi < 200.0, "central-Berlin DSM band [{lo}, {hi}]");
}
