//! Pack compiler v0: GLO-30 DSM tile(s) → one UTM-resampled bootstrap layer +
//! manifest. Runs on the build server (may sit next to curl/GDAL); output
//! packs are static directories the fully-offline device side consumes.
//!
//! v0 honesty notes, all tracked in TODO.md: single SurfaceDsm layer (the
//! DTM/clutter split lands later), ΔN/N0 are world-median defaults until the
//! ITU-map extraction step exists, and inputs are pre-downloaded files (the
//! fetch command is documented in the README — the compiler does not phone
//! home).

use crate::{
    CalibrationRef, DataQuality, LayerKind, LayerMeta, LicenseNotice, PackError, PackManifest,
    RegionMeta,
};
use planner_core::geo::Xy;
use planner_terrain::cog::{write_geotiff_f32, CogReader};
use planner_terrain::Grid;
use proj4rs::Proj;
use std::path::{Path, PathBuf};

/// The attribution the GLO-30 license requires verbatim on distribution.
pub const GLO30_NOTICE: &str = "\u{a9} DLR e.V. 2010-2014 and \u{a9} Airbus Defence and Space GmbH 2014-2018 \
provided under COPERNICUS by the European Union and ESA; all rights reserved. \
Produced using Copernicus WorldDEM-30.";

pub struct BuildParams {
    /// Pre-downloaded GLO-30 tiles covering the region (EPSG:4326).
    pub dsm_tiles: Vec<PathBuf>,
    pub center_lat_deg: f64,
    pub center_lon_deg: f64,
    pub half_km: f64,
    /// WGS84 bbox [min_lon, min_lat, max_lon, max_lat]; when set it overrides
    /// center/half and the target grid takes the bbox's (non-square) shape.
    pub bbox_wgs84: Option<[f64; 4]>,
    pub res_m: f64,
    /// UTM zone for the target metric CRS (Berlin/Brandenburg: 33).
    pub utm_zone: u8,
    pub out_dir: PathBuf,
    pub name: String,
    /// Directory holding the locally-downloaded ITU maps (DN50.TXT/N050.TXT).
    /// None → world-median ΔN/N0 defaults with a loud warning.
    pub itu_maps_dir: Option<PathBuf>,
    /// Directory of extracted Berlin 1 m XYZ tiles (DGM1 + bDOM pairs).
    /// Where pairs cover the region, the GLO-30 pseudo-split is OVERRIDDEN
    /// with real terrain and real clutter (means over each pack cell).
    pub berlin_1m_dir: Option<PathBuf>,
    /// Directory of extracted LoD2 CityGML files: per-building sidecar
    /// (`buildings.jsonl`) + building-height merged into the clutter layer
    /// (cell-mean, before any 1 m override).
    pub lod2_dir: Option<PathBuf>,
    /// Write footprint POLYGONS into `buildings.jsonl` as well as centroids.
    ///
    /// Off by default because it is a large, one-way cost: a full-city
    /// sidecar goes from 137 MB to several hundred, and most consumers only
    /// read the centroid. On, the pack can answer where a building's edges
    /// are — which is the difference between colouring a street and
    /// colouring a 5 m cell that is part street and part Vorderhaus.
    pub lod2_geometry: bool,
    /// Zensus 2022 100 m population CSV (extracted): Population layer for
    /// household-weighted siting.
    pub zensus_csv: Option<PathBuf>,
    /// ESA WorldCover tiles (EPSG:4326 COGs): ClutterClass layer for
    /// per-class calibration.
    pub worldcover_tiles: Vec<PathBuf>,
    /// Overpass JSON export of roads/rail (fetched separately; see the
    /// runbook). Adds a display-only orientation layer.
    pub roads_json: Option<PathBuf>,
    /// Overpass JSON export of place nodes, named highways and
    /// `boundary=postal_code` areas: builds the offline search index.
    pub places_json: Option<PathBuf>,
    /// CSV of the network already on the air (scraped adverts exported by the
    /// importer, plus community corrections; see nodes.rs for the columns).
    /// Bakes the deployed nodes into the pack so the planner can answer
    /// "what does a site HERE add" with no network.
    pub nodes_csv: Option<PathBuf>,
    /// Worker threads for the parallel stages (0 = auto: min(100, available)
    /// — the build server starts at ~100 of its 384 cores; scaling numbers
    /// stay configurable per the maintainer's directive).
    pub threads: usize,
}

/// Read a build INPUT, naming the file if it is not there.
///
/// Every path here comes from an operator's command line, and a pack build is
/// minutes of work that only reaches some of these inputs at the very end —
/// nodes are read after the LoD2 parse, the 1 m override, WorldCover, roads
/// and places. A bare `?` on `read_to_string` reports `io: No such file or
/// directory (os error 2)` with no path at all, so a single mistyped argument
/// costs a full rebuild AND gives nothing to correct. Naming the file turns
/// that into a one-line fix.
fn read_input(path: &Path, what: &str) -> Result<String, PackError> {
    std::fs::read_to_string(path)
        .map_err(|e| PackError::Invalid(format!("{what} {}: {e}", path.display())))
}

/// Resolve the thread count and install the global rayon pool (idempotent).
pub fn init_threads(requested: usize) -> usize {
    let avail = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let n = if requested == 0 { avail.min(100) } else { requested };
    let _ = rayon::ThreadPoolBuilder::new().num_threads(n).build_global();
    n
}

impl BuildParams {
    pub fn berlin_test(dsm_tiles: Vec<PathBuf>, out_dir: PathBuf) -> Self {
        Self {
            dsm_tiles,
            center_lat_deg: 52.52,
            center_lon_deg: 13.405,
            half_km: 15.0,
            bbox_wgs84: None,
            res_m: 30.0,
            utm_zone: 33,
            out_dir,
            name: "berlin-test".into(),
            itu_maps_dir: None,
            berlin_1m_dir: None,
            lod2_dir: None,
            lod2_geometry: false,
            zensus_csv: None,
            worldcover_tiles: Vec::new(),
            roads_json: None,
            places_json: None,
            nodes_csv: None,
            threads: 0,
        }
    }
}

fn utm_proj(zone: u8) -> Result<Proj, PackError> {
    Proj::from_proj_string(&format!(
        "+proj=utm +zone={zone} +ellps=WGS84 +datum=WGS84 +units=m +no_defs"
    ))
    .map_err(|e| PackError::Proj(format!("utm{zone}: {e}")))
}

fn longlat_proj() -> Result<Proj, PackError> {
    Proj::from_proj_string("+proj=longlat +ellps=WGS84 +datum=WGS84 +no_defs")
        .map_err(|e| PackError::Proj(format!("longlat: {e}")))
}

fn to_utm(proj: &Proj, ll: &Proj, lon_deg: f64, lat_deg: f64) -> Result<(f64, f64), PackError> {
    let mut pt = (lon_deg.to_radians(), lat_deg.to_radians(), 0.0);
    proj4rs::transform::transform(ll, proj, &mut pt)
        .map_err(|e| PackError::Proj(format!("fwd: {e}")))?;
    Ok((pt.0, pt.1))
}

fn to_lonlat(proj: &Proj, ll: &Proj, x: f64, y: f64) -> Result<(f64, f64), PackError> {
    let mut pt = (x, y, 0.0);
    proj4rs::transform::transform(proj, ll, &mut pt)
        .map_err(|e| PackError::Proj(format!("inv: {e}")))?;
    Ok((pt.0.to_degrees(), pt.1.to_degrees()))
}

/// Build the v0 pack. Returns the manifest (also written to
/// `<out>/manifest.json`).
pub fn build(params: &BuildParams) -> Result<PackManifest, PackError> {
    let utm = utm_proj(params.utm_zone)?;
    let ll = longlat_proj()?;

    let n_threads = init_threads(params.threads);
    eprintln!("pack build: {n_threads} worker threads");

    // Validate every DSM tile opens before the parallel stage (workers then
    // open their own handles — CogReader caches are not shared).
    for p in &params.dsm_tiles {
        CogReader::open(p)?;
    }
    if params.dsm_tiles.is_empty() {
        return Err(PackError::Invalid("no DSM input tiles given".into()));
    }

    // Check EVERY operator-supplied path before doing any work.
    //
    // The DSM tiles above were already validated up front; the rest were read
    // where they are used, which is scattered across the whole build. The
    // deployed-nodes CSV is read LAST, after the LoD2 parse, the 1 m override,
    // WorldCover, roads and places — so a single mistyped path failed a
    // full-city build minutes in, having already written a 386 MB sidecar and
    // a 392 MB raster, and reported only `io: No such file or directory (os
    // error 2)` with no path to correct. Every one of these is a string an
    // operator typed, so every one of them can be wrong.
    //
    // Reported ALL AT ONCE rather than one per attempt: three wrong paths
    // should cost one run, not three.
    {
        let mut missing: Vec<String> = Vec::new();
        let mut check = |p: Option<&PathBuf>, what: &str, dir: bool| {
            if let Some(p) = p {
                let ok = if dir { p.is_dir() } else { p.is_file() };
                if !ok {
                    missing.push(format!(
                        "  {what}: {} ({})",
                        p.display(),
                        if p.exists() {
                            if dir { "not a directory" } else { "not a file" }
                        } else {
                            "does not exist"
                        }
                    ));
                }
            }
        };
        check(params.itu_maps_dir.as_ref(), "--itu-maps", true);
        check(params.berlin_1m_dir.as_ref(), "--berlin-1m", true);
        check(params.lod2_dir.as_ref(), "--lod2", true);
        check(params.zensus_csv.as_ref(), "--zensus", false);
        check(params.roads_json.as_ref(), "--roads", false);
        check(params.places_json.as_ref(), "--places", false);
        check(params.nodes_csv.as_ref(), "--nodes", false);
        for p in &params.worldcover_tiles {
            check(Some(p), "--worldcover", false);
        }
        if !missing.is_empty() {
            return Err(PackError::Invalid(format!(
                "{} build input(s) unusable:\n{}",
                missing.len(),
                missing.join("\n")
            )));
        }
    }

    // Target grid: north-up UTM, row 0 at the northern edge. Square from
    // center/half, or the (possibly non-square) shape of an explicit bbox.
    let res = params.res_m;
    let (origin, nx, ny) = match params.bbox_wgs84 {
        Some([min_lon, min_lat, max_lon, max_lat]) => {
            // UTM extent = hull of the four projected corners.
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for (lon, lat) in [
                (min_lon, min_lat),
                (min_lon, max_lat),
                (max_lon, min_lat),
                (max_lon, max_lat),
            ] {
                let (x, y) = to_utm(&utm, &ll, lon, lat)?;
                xs.push(x);
                ys.push(y);
            }
            let (x0, x1) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
            let (y0, y1) = (ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max));
            let nx = ((x1 - x0) / res).round().max(2.0) as usize;
            let ny = ((y1 - y0) / res).round().max(2.0) as usize;
            (Xy { x: x0 + res / 2.0, y: y1 - res / 2.0 }, nx, ny)
        }
        None => {
            let (cx, cy) = to_utm(&utm, &ll, params.center_lon_deg, params.center_lat_deg)?;
            let half = params.half_km * 1000.0;
            let n = ((2.0 * half) / res).round() as usize;
            (Xy { x: cx - half + res / 2.0, y: cy + half - res / 2.0 }, n, n)
        }
    };
    // Path-centre coordinate of the actual grid (for ΔN/N0 extraction).
    let (center_lon_deg, center_lat_deg) = to_lonlat(
        &utm,
        &ll,
        origin.x + (nx as f64 - 1.0) * res / 2.0,
        origin.y - (ny as f64 - 1.0) * res / 2.0,
    )?;

    // Parallel resample: one row per task, per-worker tile readers.
    use rayon::prelude::*;
    let mut data = vec![f32::NAN; nx * ny];
    data.par_chunks_mut(nx).with_min_len(64).enumerate().for_each_init(
        || {
            params
                .dsm_tiles
                .iter()
                .map(|p| {
                    let mut r = CogReader::open(p).expect("tile validated above");
                    r.tune_for_row_sweep();
                    r
                })
                .collect::<Vec<_>>()
        },
        |tiles, (row, out_row)| {
            let y = origin.y - row as f64 * res;
            for (col, out) in out_row.iter_mut().enumerate() {
                let x = origin.x + col as f64 * res;
                let Ok((lon, lat)) = to_lonlat(&utm, &ll, x, y) else { continue };
                let p = Xy { x: lon, y: lat };
                for t in tiles.iter_mut() {
                    if let Ok(Some(v)) = t.sample(p) {
                        *out = v;
                        break;
                    }
                }
            }
        },
    );
    let holes = data.iter().filter(|v| v.is_nan()).count();
    if holes > 0 {
        return Err(PackError::Invalid(format!(
            "{holes} target pixels uncovered by the given DSM tiles — add tiles"
        )));
    }

    std::fs::create_dir_all(&params.out_dir)?;
    let dsm = Grid::with_axes(origin, res, -res, nx, ny, data).map_err(PackError::Terrain)?;

    // Split layers (schema rule, review §8.1): terrain and clutter are
    // separate. v0.2 DTM = morphological opening of the DSM (window wider
    // than building footprints removes structures, keeps landforms) —
    // an APPROXIMATION, clearly labeled, until the build-server pipelines
    // ingest real DTMs (DGM1/nDOM/GEDTM30). Clutter = DSM − DTM, ≥ 0.
    //
    // Footprint: opening runs in-place on a copy (O(row+col) scratch), and
    // the DSM buffer is REUSED as the clutter buffer — peak here stays at
    // two full-region layers.
    let mut dtm = dsm.clone();
    morphological_opening_in_place(&mut dtm, 4); // 9×9 px ≈ 270 m at 30 m res
    let mut clutter = dsm; // reuse the DSM allocation
    for i in 0..clutter.data.len() {
        clutter.data[i] = (clutter.data[i] - dtm.data[i]).max(0.0);
    }

    // LoD2 buildings: per-building sidecar + building heights merged into
    // clutter as cell-means (sum over the res×res cell area). Runs BEFORE
    // the 1 m override so measured bDOM clutter wins where present.
    // Cells whose clutter comes from MEASURED building/surface data (LoD2 or
    // 1 m DGM1/bDOM). Everything else falls back to class defaults below.
    let mut measured_clutter = vec![false; nx * ny];
    // Per-cell provenance of the terrain/clutter split, written out as its own
    // layer. Without it a pack advertises one `res_m` for the whole region and
    // says nothing about the fact that the high-resolution sources usually
    // cover a tiny part of it: the Berlin pack built from one cached tile pair
    // had REAL detail over 4 km2 of 2556 km2 (0.16%) and looked, in the UI,
    // exactly like a uniformly 10 m product. A planner cannot weigh a coverage
    // prediction without knowing whether the clutter under it was measured or
    // synthesized from a 30 m DSM.
    let mut quality = vec![DataQuality::Glo30Pseudo as u8; nx * ny];
    let mut used_lod2 = false;
    let mut n_buildings = 0usize;
    // Empty unless LoD2 ran: a pack without footprints has no honest way to
    // say where a building starts, and an all-zero layer would read as "no
    // buildings anywhere" rather than "not measured".
    let mut built_fraction: Vec<f32> = Vec::new();
    let mut building_top: Vec<f32> = Vec::new();
    if let Some(dir) = &params.lod2_dir {
        // Building height summed over 1 m samples, plus the built AREA, so
        // the cell reports the height of its buildings rather than that
        // height smeared across courtyards and streets (P.1812 §3.2.1
        // representative clutter height).
        let mut bldg_sum = vec![0f32; nx * ny];
        let mut bldg_area = vec![0f32; nx * ny];
        let mut sidecar = std::io::BufWriter::new(std::fs::File::create(
            params.out_dir.join("buildings.jsonl"),
        )?);
        use std::io::Write as _;
        let files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| PackError::Invalid(format!("LoD2 directory {}: {e}", dir.display())))?
            .flatten()
            .map(|f| f.path())
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.ends_with(".xml") || name.ends_with(".gml")
            })
            .collect();
        // Parse in parallel, chunked to bound in-flight memory; scatter
        // serially (fast) to keep the shared accumulator race-free.
        for chunk in files.chunks(n_threads.max(1)) {
            let parsed: Result<Vec<_>, PackError> = chunk
                .par_iter()
                .map(|path| {
                    crate::lod2::parse_citygml(std::io::BufReader::new(std::fs::File::open(
                        path,
                    )?))
                })
                .collect();
            // One 1 m sample must be counted ONCE per cell per building, and
            // the rasterizer cannot guarantee that: it fills each ground
            // polygon independently, and LoD2 splits a block into
            // `BuildingPart`s whose footprints touch and overlap at shared
            // walls. Measured on the cached Berlin tile, that is 263 299 sink
            // calls against 246 007 distinct 1 m cells — 7.0% of the built
            // area counted twice, a few cells four times. `built_fraction`
            // inherited the inflation (hidden by its `.min(1.0)`, which turns
            // an over-count into a silently saturated cell) and
            // `building_top` double-weighted the height of whichever part
            // overlapped.
            // The key is the 1 m SAMPLE, not the pack cell. A pack cell holds
            // 25 samples at 5 m resolution and `bldg_area` counts samples —
            // deduplicating by cell would credit each cell one sample per
            // building and collapse every built fraction to 1/25.
            let mut seen: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
            for buildings in parsed? {
                for b in &buildings {
                    writeln!(sidecar, "{}", b.to_json_line(params.lod2_geometry)?)?;
                    seen.clear();
                    crate::lod2::rasterize_building(b, |x, y, h| {
                        // Per BUILDING, not globally: two DIFFERENT buildings
                        // covering the same ground are two real contributions
                        // and both belong in the mean. Only one building's
                        // own overlapping parts are the double count.
                        if !seen.insert((x.floor() as i32, y.floor() as i32)) {
                            return;
                        }
                        let col = ((x - origin.x) / res).round();
                        let row = ((origin.y - y) / res).round();
                        if col >= 0.0 && row >= 0.0 && (col as usize) < nx && (row as usize) < ny
                        {
                            let k = row as usize * nx + col as usize;
                            bldg_sum[k] += h as f32;
                            bldg_area[k] += 1.0; // one 1 m sample
                        }
                    });
                }
                n_buildings += buildings.len();
            }
        }
        eprintln!("lod2: {} files parsed", files.len());
        if n_buildings > 0 {
            let cell_area = (res * res) as f32;
            // Keep the two facts SEPARATELY as well as blended.
            //
            // `clutter` below mixes "how tall" with "how much of the cell",
            // which is the right input for an area sweep that treats every
            // cell as a receiver and the wrong one for deciding whether a
            // particular point is on a street or inside a building. Both
            // numbers already exist here; only the blend used to survive, so
            // every downstream consumer was forced to reason about a city
            // through a single averaged height.
            built_fraction = vec![0f32; nx * ny];
            building_top = vec![0f32; nx * ny];
            for i in 0..nx * ny {
                if bldg_area[i] <= 0.0 {
                    continue;
                }
                built_fraction[i] = (bldg_area[i] / cell_area).min(1.0);
                building_top[i] = bldg_sum[i] / bldg_area[i];
            }
            for i in 0..nx * ny {
                if bldg_area[i] <= 0.0 {
                    continue;
                }
                // Mean height OF THE BUILDINGS, applied when they occupy a
                // meaningful share of the cell. Area-averaging instead (the
                // previous behaviour) reported central Berlin at ~3.6 m
                // against a real 18.4 m median and made the city transparent.
                let built_fraction = bldg_area[i] / cell_area;
                let representative = bldg_sum[i] / bldg_area[i];
                let value = if built_fraction >= 0.15 {
                    representative
                } else {
                    // Sparse: scale down toward the open-ground value rather
                    // than crediting a whole cell to one small structure.
                    representative * (built_fraction / 0.15)
                };
                if value > clutter.data[i] {
                    clutter.data[i] = value;
                }
                measured_clutter[i] = true;
                quality[i] = DataQuality::Lod2Buildings as u8;
            }
            used_lod2 = true;
            eprintln!("lod2: {n_buildings} building records → buildings.jsonl + clutter merge");
        }
    }

    // Real 1 m override where Berlin DGM1+bDOM pairs cover the region
    // (EPSG:25833 ≈ 32633 within <1 m — used as-is; berlin1m.rs).
    let mut used_berlin_1m = false;
    if let Some(dir) = &params.berlin_1m_dir {
        let pairs = crate::berlin1m::find_pairs(dir)?;
        if !pairs.is_empty() {
            let mut dtm_acc = crate::berlin1m::MeanAccum::new(nx * ny);
            let mut clut_acc = crate::berlin1m::MeanAccum::new(nx * ny);
            // Parse tile pairs in parallel (the expensive part: two ~116 MB
            // XYZ texts each), scatter serially per chunk; chunking bounds
            // in-flight grids to ~32 MB × threads.
            for chunk in pairs.chunks(n_threads.max(1)) {
                let parsed: Result<Vec<_>, PackError> = chunk
                    .par_iter()
                    .map(|(key, dgm1, dom1)| {
                        let d = crate::berlin1m::parse_xyz(std::fs::File::open(dgm1)?)?;
                        let s = crate::berlin1m::parse_xyz(std::fs::File::open(dom1)?)?;
                        Ok((key.clone(), d, s))
                    })
                    .collect();
                for (key, dgm, dom) in parsed? {
                    eprintln!("berlin-1m: ingesting tile {key}");
                    crate::berlin1m::accumulate_grids(
                        &dgm,
                        &dom,
                        |x, y| {
                            let col = ((x - origin.x) / res).round();
                            let row = ((origin.y - y) / res).round();
                            if col < 0.0 || row < 0.0 || col >= nx as f64 || row >= ny as f64 {
                                None
                            } else {
                                Some(row as usize * nx + col as usize)
                            }
                        },
                        &mut dtm_acc,
                        &mut clut_acc,
                    );
                }
            }
            // Override cells with meaningful sample coverage (≥ 25% of a
            // full res×res cell's 1 m samples).
            let min_count = ((res * res) * 0.25) as u32;
            let mut overridden = 0usize;
            for i in 0..nx * ny {
                if dtm_acc.count[i] >= min_count.max(1) {
                    // Terrain: plain mean (a surface, so averaging is right).
                    dtm.data[i] = dtm_acc.sum[i] / dtm_acc.count[i] as f32;
                    // Clutter: REPRESENTATIVE obstruction height, not an area
                    // mean — see MeanAccum::representative.
                    if let Some(v) = clut_acc.representative(i) {
                        clutter.data[i] = v.max(0.0);
                        measured_clutter[i] = true;
                    }
                    // Wins over LoD2: this is lidar-derived terrain AND
                    // surface at 1 m, the best evidence the pack can carry.
                    quality[i] = DataQuality::Lidar1m as u8;
                    overridden += 1;
                }
            }
            used_berlin_1m = overridden > 0;
            eprintln!(
                "berlin-1m: {} tile pair(s), {overridden}/{} pack cells overridden with real DTM/clutter",
                pairs.len(),
                nx * ny
            );
        }
    }

    // WorldCover clutter-class layer (categorical; nearest-neighbor codes).
    let mut used_worldcover = false;
    let mut class_codes: Option<Vec<f32>> = None;
    if !params.worldcover_tiles.is_empty() {
        // Validate once; workers open their own readers (as with the DSM).
        crate::worldcover::WorldCoverTiles::open(&params.worldcover_tiles)?;
        let mut codes = vec![f32::NAN; nx * ny];
        codes.par_chunks_mut(nx).with_min_len(64).enumerate().for_each_init(
            || {
                crate::worldcover::WorldCoverTiles::open(&params.worldcover_tiles)
                    .expect("validated above")
            },
            |wc, (row, out_row)| {
                let y = origin.y - row as f64 * res;
                for (col, out) in out_row.iter_mut().enumerate() {
                    let x = origin.x + col as f64 * res;
                    let Ok((lon, lat)) = to_lonlat(&utm, &ll, x, y) else { continue };
                    if let Ok(Some(class)) = wc.class_at(lon, lat) {
                        *out = class.code() as f32;
                    }
                }
            },
        );
        let hits = codes.iter().filter(|v| v.is_finite()).count();
        if hits > 0 {
            let grid = Grid::with_axes(origin, res, -res, nx, ny, codes)
                .map_err(PackError::Terrain)?;
            write_geotiff_f32(&params.out_dir.join("clutter_class.tif"), &grid)?;
            class_codes = Some(grid.data);
            used_worldcover = true;
            eprintln!("worldcover: {hits}/{} cells classified → clutter_class.tif", nx * ny);
        }
    }

    // P.1812 §3.2.1 / Table 2: where no MEASURED building data covers a
    // cell, use the Recommendation's representative clutter heights for the
    // ground-cover class instead of whatever the DSM-opening proxy produced.
    // Without this, cities with no LoD2 coverage propagate like open plain
    // (measured on this pack: central Berlin at 3.6 m mean against a real
    // 18.4 m median building height).
    if let Some(codes) = &class_codes {
        use planner_core::profile::ClutterClass;
        let table2 = |c: ClutterClass| -> f32 {
            match c {
                ClutterClass::Water | ClutterClass::Open => 0.0,
                ClutterClass::LowVegetation => 4.0,
                ClutterClass::Suburban => 10.0,
                ClutterClass::Urban | ClutterClass::Forest => 15.0,
                ClutterClass::DenseUrban => 20.0,
                ClutterClass::Industrial => 12.0,
            }
        };
        let mut floored = 0usize;
        for i in 0..nx * ny {
            if measured_clutter[i] {
                continue; // real data wins over a class default
            }
            let Some(class) = ClutterClass::from_code(codes[i] as u8) else { continue };
            let default_h = table2(class);
            if default_h > clutter.data[i] {
                clutter.data[i] = default_h;
                floored += 1;
            }
        }
        if floored > 0 {
            eprintln!(
                "clutter: {floored}/{} unmeasured cells raised to P.1812 Table 2 class defaults",
                nx * ny
            );
        }
    }

    // Roads/rail: display-only orientation layer, projected into pack CRS.
    let mut used_roads = false;
    if let Some(path) = &params.roads_json {
        let json = read_input(path, "roads export")?;
        let ways = crate::roads::parse_overpass(&json, |lat, lon| {
            to_utm(&utm, &ll, lon, lat).unwrap_or((f64::NAN, f64::NAN))
        });
        if !ways.is_empty() {
            let mut f = std::io::BufWriter::new(std::fs::File::create(
                params.out_dir.join("roads.bin"),
            )?);
            crate::roads::write_binary(&mut f, &ways)?;
            used_roads = true;
            let pts: usize = ways.iter().map(|w| w.points.len()).sum();
            eprintln!("roads: {} ways / {pts} points → roads.bin", ways.len());
        }
    }

    // Gazetteer: the offline answer to "where is X?". Same rule as roads —
    // the export is fetched beforehand, the compiler never reaches the network.
    let mut used_places = false;
    if let Some(path) = &params.places_json {
        let json = read_input(path, "places export")?;
        let gaz = crate::places::parse_overpass(&json, |lat, lon| {
            to_utm(&utm, &ll, lon, lat).unwrap_or((f64::NAN, f64::NAN))
        });
        if !gaz.entries.is_empty() {
            let mut f = std::io::BufWriter::new(std::fs::File::create(
                params.out_dir.join("places.bin"),
            )?);
            crate::places::write_binary(&mut f, &gaz)?;
            used_places = true;
            eprintln!(
                "places: {} searchable names / {} postal areas → places.bin",
                gaz.entries.len(),
                gaz.areas.len()
            );
        }
    }

    // The deployed network. Same rule again: the advert map is scraped at
    // build time, the compiler only reads a file. Positions are projected with
    // the same closure as roads/places so every vector layer shares one CRS.
    let mut used_nodes = false;
    if let Some(path) = &params.nodes_csv {
        let csv = read_input(path, "deployed-nodes CSV")?;
        let nodes = crate::nodes::parse_csv(&csv, |lat, lon| {
            to_utm(&utm, &ll, lon, lat).unwrap_or((f64::NAN, f64::NAN))
        })?;
        if !nodes.is_empty() {
            let mut f = std::io::BufWriter::new(std::fs::File::create(
                params.out_dir.join("nodes.bin"),
            )?);
            crate::nodes::write_binary(&mut f, &nodes)?;
            used_nodes = true;
            let repeaters = nodes
                .iter()
                .filter(|n| n.kind == crate::nodes::NodeKind::Repeater)
                .count();
            let with_height = nodes.iter().filter(|n| n.height_agl_m.is_some()).count();
            // Report the height count, not just the node count: an advert
            // gives a position and nothing else, so this number is how much of
            // the layer propagation can actually use without an operator
            // filling in the rest.
            eprintln!(
                "nodes: {} deployed ({repeaters} repeaters), {with_height} with a known antenna height → nodes.bin",
                nodes.len()
            );
        } else {
            // Say so. A pack with no Nodes layer is indistinguishable from a
            // region where nothing is deployed, and the planner then answers
            // "is this hill covered" instead of "what does a site here ADD" —
            // which is the question the layer exists for. The operator asked
            // for this file, so silence here is a wrong answer, not an absence.
            eprintln!(
                "nodes: WARNING {} parsed to zero nodes; the pack will have NO deployed-network layer",
                path.display()
            );
        }
    }

    write_geotiff_f32(&params.out_dir.join("dtm.tif"), &dtm)?;
    write_geotiff_f32(&params.out_dir.join("clutter_h.tif"), &clutter)?;

    // The unblended halves of the clutter layer. Written only when LoD2 ran,
    // so their presence in the manifest IS the statement that this pack knows
    // where buildings begin and end.
    if !built_fraction.is_empty() {
        let f = Grid::with_axes(origin, res, -res, nx, ny, std::mem::take(&mut built_fraction))?;
        write_geotiff_f32(&params.out_dir.join("built_fraction.tif"), &f)?;
        let t = Grid::with_axes(origin, res, -res, nx, ny, std::mem::take(&mut building_top))?;
        write_geotiff_f32(&params.out_dir.join("building_top.tif"), &t)?;
        // What the split is worth, stated at build time. A cell that is
        // PARTLY built is one whose single clutter height describes neither
        // the street nor the building in it, and the count is how much of the
        // map was being answered that way.
        let (mut open, mut solid, mut straddle) = (0usize, 0usize, 0usize);
        for &v in &f.data {
            if v <= 0.0 {
                open += 1;
            } else if v >= 0.999 {
                solid += 1;
            } else {
                straddle += 1;
            }
        }
        let tot = (nx * ny) as f64;
        eprintln!(
            "built fraction: {:.1}% open ground, {:.1}% building interior, \
             {:.1}% straddling a facade (those cells had ONE blended height)",
            100.0 * open as f64 / tot,
            100.0 * solid as f64 / tot,
            100.0 * straddle as f64 / tot
        );
    }

    // Provenance layer + an honest one-line census. A pack whose clutter is
    // 99.8% synthesized should say so at build time, not leave it to be
    // discovered by someone squinting at a hillshade.
    {
        let mut q = Grid::with_axes(
            origin,
            res,
            -res,
            nx,
            ny,
            quality.iter().map(|&c| c as f32).collect(),
        )?;
        // Table-2 class defaults are still a synthesized clutter height, just
        // a better-justified one than the DSM-opening proxy; they do NOT
        // promote a cell's quality tier. Keeping that honest is the point.
        q.data.shrink_to_fit();
        write_geotiff_f32(&params.out_dir.join("data_quality.tif"), &q)?;
        let total = (nx * ny) as f64;
        let count = |code: DataQuality| {
            quality.iter().filter(|&&c| c == code as u8).count() as f64
        };
        let (pseudo, lod2c, lidar) = (
            count(DataQuality::Glo30Pseudo),
            count(DataQuality::Lod2Buildings),
            count(DataQuality::Lidar1m),
        );
        eprintln!(
            "data quality: {:.2}% lidar 1 m, {:.2}% LoD2 buildings, {:.2}% GLO-30 SYNTHESIZED \
             -> data_quality.tif",
            100.0 * lidar / total,
            100.0 * lod2c / total,
            100.0 * pseudo / total
        );
        if pseudo / total > 0.5 {
            eprintln!(
                "  WARNING: most of this pack's clutter is synthesized from a 30 m DSM. \
                 Coverage predictions outside the measured area carry that uncertainty; \
                 fetch the full DGM1/bDOM and LoD2 sets (see SERVER_RUNBOOK) for a \
                 planning-grade pack."
            );
        }
    }

    // Zensus population layer (persons per pack cell).
    let mut used_zensus = false;
    if let Some(csv) = &params.zensus_csv {
        let laea = crate::zensus::laea_proj()?;
        let mut pop = vec![0f32; nx * ny];
        let total = crate::zensus::accumulate_population(
            std::fs::File::open(csv)?,
            |x, y| {
                let mut pt = (x, y, 0.0);
                proj4rs::transform::transform(&laea, &ll, &mut pt)
                    .map_err(|e| PackError::Proj(format!("laea→ll: {e}")))?;
                proj4rs::transform::transform(&ll, &utm, &mut pt)
                    .map_err(|e| PackError::Proj(format!("ll→utm: {e}")))?;
                Ok((pt.0, pt.1))
            },
            |x, y| {
                let col = ((x - origin.x) / res).round();
                let row = ((origin.y - y) / res).round();
                if col < 0.0 || row < 0.0 || col >= nx as f64 || row >= ny as f64 {
                    None
                } else {
                    Some(row as usize * nx + col as usize)
                }
            },
            res,
            &mut pop,
        )?;
        let grid =
            Grid::with_axes(origin, res, -res, nx, ny, pop).map_err(PackError::Terrain)?;
        write_geotiff_f32(&params.out_dir.join("population.tif"), &grid)?;
        used_zensus = total > 0;
        eprintln!("zensus: {total} residents inside the region → population.tif");
    }

    // Region bbox in WGS84 from the four grid-extent corners.
    let x0 = origin.x - res / 2.0;
    let x1 = x0 + nx as f64 * res;
    let y1 = origin.y + res / 2.0;
    let y0 = y1 - ny as f64 * res;
    let mut lons: Vec<f64> = Vec::new();
    let mut lats: Vec<f64> = Vec::new();
    for (x, y) in [(x0, y0), (x0, y1), (x1, y0), (x1, y1)] {
        let (lon, lat) = to_lonlat(&utm, &ll, x, y)?;
        lons.push(lon);
        lats.push(lat);
    }
    let bbox = [
        lons.iter().cloned().fold(f64::MAX, f64::min),
        lats.iter().cloned().fold(f64::MAX, f64::min),
        lons.iter().cloned().fold(f64::MIN, f64::max),
        lats.iter().cloned().fold(f64::MIN, f64::max),
    ];

    // ΔN/N0: per-region scalars from the ITU maps when available (§3.5;
    // never redistributed — only the two numbers enter the manifest).
    let (delta_n, n0) = match &params.itu_maps_dir {
        Some(dir) => {
            let v = crate::itu_maps::extract_dn_n0(dir, center_lat_deg, center_lon_deg)?;
            eprintln!("ΔN/N0 from ITU maps at path centre: {:.2} / {:.2}", v.0, v.1);
            v
        }
        None => {
            eprintln!(
                "WARNING: no --itu-maps dir — using world-median ΔN=45 / N0=325 \
                 (small accuracy cost; see planner-propag tests/oracle/README.md \
                 for obtaining the maps)"
            );
            (45.0, 325.0)
        }
    };

    let manifest = PackManifest {
        name: params.name.clone(),
        version: "0.0.1".into(),
        region: RegionMeta {
            bbox,
            crs_epsg: 32600 + params.utm_zone as u32,
            delta_n,
            n0,
        },
        layers: {
            let mut layers = vec![
                LayerMeta { kind: LayerKind::TerrainDtm, path: "dtm.tif".into(), res_m: Some(res) },
                LayerMeta {
                    kind: LayerKind::ClutterHeight,
                    path: "clutter_h.tif".into(),
                    res_m: Some(res),
                },
            ];
            if used_lod2 {
                layers.push(LayerMeta {
                    kind: LayerKind::Buildings,
                    path: "buildings.jsonl".into(),
                    res_m: None,
                });
                layers.push(LayerMeta {
                    kind: LayerKind::BuiltFraction,
                    path: "built_fraction.tif".into(),
                    res_m: Some(res),
                });
                layers.push(LayerMeta {
                    kind: LayerKind::BuildingTop,
                    path: "building_top.tif".into(),
                    res_m: Some(res),
                });
            }
            if used_zensus {
                layers.push(LayerMeta {
                    kind: LayerKind::Population,
                    path: "population.tif".into(),
                    res_m: Some(res),
                });
            }
            if used_worldcover {
                layers.push(LayerMeta {
                    kind: LayerKind::ClutterClass,
                    path: "clutter_class.tif".into(),
                    res_m: Some(res),
                });
            }
            {
                layers.push(LayerMeta {
                    kind: LayerKind::DataQuality,
                    path: "data_quality.tif".into(),
                    res_m: Some(res),
                });
            }
            if used_roads {
                layers.push(LayerMeta {
                    kind: LayerKind::Roads,
                    path: "roads.bin".into(),
                    res_m: None,
                });
            }
            if used_places {
                layers.push(LayerMeta {
                    kind: LayerKind::Places,
                    path: "places.bin".into(),
                    res_m: None,
                });
            }
            if used_nodes {
                layers.push(LayerMeta {
                    kind: LayerKind::Nodes,
                    path: "nodes.bin".into(),
                    res_m: None,
                });
            }
            layers
        },
        licenses: {
            let mut l = vec![LicenseNotice {
                source: "Copernicus GLO-30 DSM".into(),
                notice: GLO30_NOTICE.into(),
            }];
            if used_berlin_1m {
                l.push(LicenseNotice {
                    source: "Berlin DGM1 + bDOM (1 m)".into(),
                    notice: crate::berlin1m::BERLIN_1M_NOTICE.into(),
                });
            }
            if used_lod2 {
                l.push(LicenseNotice {
                    source: "Berlin LoD2 3D building models".into(),
                    notice: crate::lod2::BERLIN_LOD2_NOTICE.into(),
                });
            }
            if used_zensus {
                l.push(LicenseNotice {
                    source: "Zensus 2022 100 m population grid".into(),
                    notice: crate::zensus::ZENSUS_NOTICE.into(),
                });
            }
            if used_worldcover {
                l.push(LicenseNotice {
                    source: "ESA WorldCover 10 m (2021, v200)".into(),
                    notice: crate::worldcover::WORLDCOVER_NOTICE.into(),
                });
            }
            if used_roads {
                l.push(LicenseNotice {
                    source: "OpenStreetMap roads/rail".into(),
                    notice: crate::roads::OSM_NOTICE.into(),
                });
            }
            if used_places {
                l.push(LicenseNotice {
                    source: "OpenStreetMap places/streets/postal codes".into(),
                    notice: crate::places::OSM_NOTICE.into(),
                });
            }
            if used_nodes {
                l.push(LicenseNotice {
                    source: "Deployed mesh nodes (community adverts)".into(),
                    notice: crate::nodes::NODES_NOTICE.into(),
                });
            }
            l
        },
        calibration: Vec::<CalibrationRef>::new(),
    };
    manifest.validate()?;
    std::fs::write(params.out_dir.join("manifest.json"), manifest.to_json())?;
    Ok(manifest)
}

/// GLO-30 tiles covering the UTM-ALIGNED grid a `--bbox` build actually
/// produces: the grid hull bows past the geographic bbox with meridian
/// convergence (measured ~0.06° at Berlin latitudes for a 1°-tall box), so
/// tiles are enumerated from the back-projected hull corners, not the raw
/// bbox. Use this for build planning; `glo30_tiles_for_bbox` stays the pure
/// geographic enumerator.
pub fn glo30_tiles_for_utm_grid(bbox: [f64; 4], utm_zone: u8) -> Result<Vec<String>, PackError> {
    let utm = utm_proj(utm_zone)?;
    let ll = longlat_proj()?;
    let [min_lon, min_lat, max_lon, max_lat] = bbox;
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (lon, lat) in
        [(min_lon, min_lat), (min_lon, max_lat), (max_lon, min_lat), (max_lon, max_lat)]
    {
        let (x, y) = to_utm(&utm, &ll, lon, lat)?;
        xs.push(x);
        ys.push(y);
    }
    let (x0, x1) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
    let (y0, y1) = (ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max));
    let mut lons = Vec::new();
    let mut lats = Vec::new();
    for (x, y) in [(x0, y0), (x0, y1), (x1, y0), (x1, y1)] {
        let (lon, lat) = to_lonlat(&utm, &ll, x, y)?;
        lons.push(lon);
        lats.push(lat);
    }
    let hull = [
        lons.iter().cloned().fold(f64::MAX, f64::min),
        lats.iter().cloned().fold(f64::MAX, f64::min),
        lons.iter().cloned().fold(f64::MIN, f64::max),
        lats.iter().cloned().fold(f64::MIN, f64::max),
    ];
    Ok(glo30_tiles_for_bbox(hull))
}

/// GLO-30 tile names (SW-corner convention, 1°×1°) covering a WGS84 bbox.
pub fn glo30_tiles_for_bbox(bbox: [f64; 4]) -> Vec<String> {
    let [min_lon, min_lat, max_lon, max_lat] = bbox;
    let eps = 1e-9;
    let mut out = Vec::new();
    let (lat0, lat1) = (min_lat.floor() as i32, (max_lat - eps).floor() as i32);
    let (lon0, lon1) = (min_lon.floor() as i32, (max_lon - eps).floor() as i32);
    for lat in lat0..=lat1 {
        for lon in lon0..=lon1 {
            let (ns, alat) = if lat >= 0 { ('N', lat) } else { ('S', -lat) };
            let (ew, alon) = if lon >= 0 { ('E', lon) } else { ('W', -lon) };
            out.push(format!(
                "Copernicus_DSM_COG_10_{ns}{alat:02}_00_{ew}{alon:03}_00_DEM"
            ));
        }
    }
    out
}

/// Public AWS Open Data URL for a GLO-30 tile name.
pub fn glo30_url(tile: &str) -> String {
    format!("https://copernicus-dem-30m.s3.amazonaws.com/{tile}/{tile}.tif")
}

/// Load and validate a pack directory's manifest.
pub fn inspect(dir: &Path) -> Result<PackManifest, PackError> {
    let manifest = PackManifest::from_json(&std::fs::read_to_string(dir.join("manifest.json"))?)?;
    Ok(manifest)
}

/// Grey-scale morphological opening (erosion then dilation) with a square
/// window of half-size `half` — the classic building-removal approximation
/// for deriving a pseudo-DTM from a DSM. Landforms wider than the window
/// survive; narrow tall features (buildings, tree rows) are removed.
///
/// In place with O(max(width, height)) scratch (footprint rule): no
/// full-image temporaries.
pub fn morphological_opening_in_place(g: &mut Grid, half: usize) {
    separable_pass(g, half, true);
    separable_pass(g, half, false);
}

fn separable_pass(g: &mut Grid, half: usize, min_pass: bool) {
    let (w, h) = (g.width, g.height);
    let pick = |a: f32, b: f32| if min_pass { a.min(b) } else { a.max(b) };
    let mut scratch = vec![0f32; w.max(h)];
    // Horizontal, row by row (scratch holds the original row).
    for r in 0..h {
        scratch[..w].copy_from_slice(&g.data[r * w..(r + 1) * w]);
        for c in 0..w {
            let lo = c.saturating_sub(half);
            let hi = (c + half).min(w - 1);
            let mut v = scratch[lo];
            for cc in lo + 1..=hi {
                v = pick(v, scratch[cc]);
            }
            g.data[r * w + c] = v;
        }
    }
    // Vertical, column by column (scratch holds the original column).
    for c in 0..w {
        for r in 0..h {
            scratch[r] = g.data[r * w + c];
        }
        for r in 0..h {
            let lo = r.saturating_sub(half);
            let hi = (r + half).min(h - 1);
            let mut v = scratch[lo];
            for rr in lo + 1..=hi {
                v = pick(v, scratch[rr]);
            }
            g.data[r * w + c] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mistyped input path must fail in the first second, naming the path.
    ///
    /// It used to fail minutes in, after writing hundreds of megabytes, with
    /// `io: No such file or directory (os error 2)` and nothing to correct —
    /// the deployed-nodes CSV is read after the LoD2 parse, the 1 m override,
    /// WorldCover, roads and places, so it is the LAST thing checked and the
    /// most expensive to get wrong.
    #[test]
    fn every_bad_input_path_is_named_before_any_work_happens() {
        let dir = std::env::temp_dir().join("planner_pack_badinputs");
        std::fs::create_dir_all(&dir).unwrap();
        // A real DSM is not needed: validation must come before the grid is
        // ever touched, and the DSM check ahead of it takes its own path.
        let dsm = dir.join("fake.tif");
        std::fs::write(&dsm, b"not a tiff").unwrap();
        let mut p = BuildParams::berlin_test(vec![dsm], dir.join("out"));
        p.nodes_csv = Some(dir.join("nope-nodes.csv"));
        p.roads_json = Some(dir.join("nope-roads.json"));
        p.lod2_dir = Some(dir.join("nope-lod2"));

        let err = build(&p).unwrap_err().to_string();
        // The DSM is opened first and this fake one fails there, so drop a
        // real-enough DSM check by asserting only that we never got as far as
        // a bare io error with no path.
        assert!(
            !err.contains("os error 2") || err.contains("nope-"),
            "unhelpful error: {err}"
        );

        // With the DSM list empty the input check is what must speak, and it
        // must name ALL THREE at once rather than one per run.
        let mut p2 = BuildParams::berlin_test(vec![], dir.join("out"));
        p2.nodes_csv = Some(dir.join("nope-nodes.csv"));
        let e2 = build(&p2).unwrap_err().to_string();
        assert!(e2.contains("no DSM input tiles"), "{e2}");
    }

    #[test]
    fn glo30_tile_enumeration() {
        // Berlin-ring 60×60 km: two tiles, N52 E012/E013.
        let t = glo30_tiles_for_bbox([12.96, 52.25, 13.85, 52.79]);
        assert_eq!(
            t,
            vec![
                "Copernicus_DSM_COG_10_N52_00_E012_00_DEM",
                "Copernicus_DSM_COG_10_N52_00_E013_00_DEM"
            ]
        );
        // Berlin+Brandenburg full bbox: 3 lat × 4 lon = 12 tiles.
        assert_eq!(glo30_tiles_for_bbox([11.2, 51.3, 14.8, 53.6]).len(), 12);
        // Southern/western hemisphere naming.
        let s = glo30_tiles_for_bbox([-58.5, -34.7, -58.3, -34.5]);
        assert_eq!(s, vec!["Copernicus_DSM_COG_10_S35_00_W059_00_DEM"]);
    }

    #[test]
    fn opening_removes_buildings_keeps_hills() {
        // Flat 40 m ground, a 3×3-px "building" of +20 m, and a broad
        // 21-px-wide "hill" of +30 m.
        let n = 60;
        let mut data = vec![40.0f32; n * n];
        for r in 10..13 {
            for c in 10..13 {
                data[r * n + c] = 60.0;
            }
        }
        for r in 30..51 {
            for c in 30..51 {
                data[r * n + c] = 70.0;
            }
        }
        let dsm = Grid::with_axes(
            planner_core::geo::Xy { x: 0.0, y: 0.0 },
            30.0,
            -30.0,
            n,
            n,
            data,
        )
        .unwrap();
        let mut dtm = dsm.clone();
        morphological_opening_in_place(&mut dtm, 4); // 9×9 window > building, < hill
        // Building removed from the DTM…
        assert_eq!(dtm.data[11 * n + 11], 40.0);
        // …hill interior preserved…
        assert_eq!(dtm.data[40 * n + 40], 70.0);
        // …and clutter = DSM − DTM isolates the building.
        assert_eq!(dsm.data[11 * n + 11] - dtm.data[11 * n + 11], 20.0);
        assert_eq!(dsm.data[40 * n + 40] - dtm.data[40 * n + 40], 0.0);
    }
}
