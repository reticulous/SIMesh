//! The pack compiler: GLO-30 surface tiles resampled onto a UTM grid and split
//! into terrain and clutter, with whatever else the build is given merged in
//! (buildings from LoD2 where its tiles cover the grid and from OpenStreetMap
//! everywhere else, Berlin's 1 m lidar, WorldCover land
//! cover, the Zensus grid, OpenStreetMap roads and places), and the manifest.
//!
//! Every input is a file already on disk; the compiler never reaches the
//! network. `planner-job pack-build` is its caller.

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

/// One progress report from [`build`].
///
/// A step reports as it starts (`done` = steps finished before it) and as it
/// ends (`done` one higher); `total` is the number of steps this build has.
/// A long step may report in between with `part = Some((i, n))`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress<'a> {
    pub step: &'a str,
    pub done: usize,
    pub total: usize,
    pub part: Option<(u64, u64)>,
}

pub type ProgressFn = dyn Fn(&Progress) + Send + Sync;

/// The steps a build runs, in order, and where it has got to.
struct Steps<'a> {
    names: Vec<&'static str>,
    done: usize,
    report: Option<&'a ProgressFn>,
}

impl Steps<'_> {
    fn send(&self, step: &str, part: Option<(u64, u64)>) {
        if let Some(f) = self.report {
            f(&Progress { step, done: self.done, total: self.names.len(), part });
        }
    }
    fn begin(&self, step: &'static str) {
        debug_assert!(self.names.contains(&step), "step {step} not planned");
        self.send(step, None);
    }
    fn part(&self, step: &'static str, i: u64, n: u64) {
        self.send(step, Some((i, n)));
    }
    fn end(&mut self, step: &'static str) {
        self.done += 1;
        self.send(step, None);
    }
}

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
    /// (cell-mean, before any 1 m override). Only the 1 km tiles meeting the
    /// grid are read, and only buildings meeting it are kept. The tiles read
    /// are the ground LoD2 covers: with `osm_buildings` as well, an
    /// OpenStreetMap building whose centroid lies in one of them is left out.
    pub lod2_dir: Option<PathBuf>,
    /// Write LoD2 footprint POLYGONS into `buildings.jsonl` as well as
    /// centroids. OpenStreetMap buildings always carry theirs.
    ///
    /// A full-city LoD2 sidecar goes from 137 MB to about 300 MB with them.
    /// With them the pack can answer where a building's edges are — which is
    /// the difference between colouring a street and colouring a 5 m cell
    /// that is part street and part Vorderhaus.
    pub lod2_geometry: bool,
    /// Zensus 2022 100 m population CSV (extracted): Population layer for
    /// household-weighted siting.
    pub zensus_csv: Option<PathBuf>,
    /// ESA WorldCover tiles (EPSG:4326 COGs): ClutterClass layer for
    /// per-class calibration.
    pub worldcover_tiles: Vec<PathBuf>,
    /// OpenStreetMap PBF extract holding the region (`osm.rs`): the roads
    /// layer and the gazetteer, and the buildings when `osm_buildings`.
    pub osm_pbf: Option<PathBuf>,
    /// Buildings from `osm_pbf`: the same sidecar, clutter merge,
    /// `BuiltFraction` and `BuildingTop` that LoD2 fills, on the ground the
    /// LoD2 tiles of `lod2_dir` do not cover.
    pub osm_buildings: bool,
    /// Worker threads for the parallel stages (0 = auto: min(100, available)).
    pub threads: usize,
    /// Where progress goes; `None` reports nothing. Diagnostics go to
    /// standard error regardless.
    pub progress: Option<Box<ProgressFn>>,
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
            osm_pbf: None,
            osm_buildings: false,
            threads: 0,
            progress: None,
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

    if params.dsm_tiles.is_empty() {
        return Err(PackError::Invalid("no DSM input tiles given".into()));
    }
    if params.osm_buildings && params.osm_pbf.is_none() {
        return Err(PackError::Invalid(
            "OpenStreetMap buildings asked for without an OpenStreetMap extract".into(),
        ));
    }

    // Check EVERY input path before doing any work, and report them ALL AT
    // ONCE. Most inputs are read where they are used, scattered across the
    // whole build, so a wrong path found there fails a build minutes in with
    // a bare `No such file or directory` — three wrong paths should cost one
    // run, not three, and each should be named.
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
        for p in &params.dsm_tiles {
            check(Some(p), "dsm_tiles", false);
        }
        check(params.itu_maps_dir.as_ref(), "itu_maps_dir", true);
        check(params.berlin_1m_dir.as_ref(), "berlin_1m_dir", true);
        check(params.lod2_dir.as_ref(), "lod2_dir", true);
        check(params.zensus_csv.as_ref(), "zensus_csv", false);
        check(params.osm_pbf.as_ref(), "osm_pbf", false);
        for p in &params.worldcover_tiles {
            check(Some(p), "worldcover_tiles", false);
        }
        if !missing.is_empty() {
            return Err(PackError::Invalid(format!(
                "{} build input(s) unusable:\n{}",
                missing.len(),
                missing.join("\n")
            )));
        }
    }

    // Validate every DSM tile opens before the parallel stage (workers then
    // open their own handles — CogReader caches are not shared).
    for p in &params.dsm_tiles {
        CogReader::open(p)
            .map_err(|e| PackError::Invalid(format!("DSM tile {}: {e}", p.display())))?;
    }

    let mut steps = Steps {
        names: [
            Some("terrain"),
            params.osm_pbf.as_ref().map(|_| "osm"),
            (params.lod2_dir.is_some() || params.osm_buildings).then_some("buildings"),
            params.berlin_1m_dir.as_ref().map(|_| "lidar"),
            (!params.worldcover_tiles.is_empty()).then_some("landcover"),
            Some("clutter"),
            params.zensus_csv.as_ref().map(|_| "population"),
            Some("manifest"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        done: 0,
        report: params.progress.as_deref(),
    };
    steps.begin("terrain");

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
    // The grid's outer edges in the pack CRS, [min_e, min_n, max_e, max_n].
    let x0 = origin.x - res / 2.0;
    let x1 = x0 + nx as f64 * res;
    let y1 = origin.y + res / 2.0;
    let y0 = y1 - ny as f64 * res;
    let grid_extent = [x0, y0, x1, y1];
    // The same in WGS84, from the four corners.
    let bbox = {
        let mut lons: Vec<f64> = Vec::new();
        let mut lats: Vec<f64> = Vec::new();
        for (x, y) in [(x0, y0), (x0, y1), (x1, y0), (x1, y1)] {
            let (lon, lat) = to_lonlat(&utm, &ll, x, y)?;
            lons.push(lon);
            lats.push(lat);
        }
        [
            lons.iter().cloned().fold(f64::MAX, f64::min),
            lats.iter().cloned().fold(f64::MAX, f64::min),
            lons.iter().cloned().fold(f64::MIN, f64::max),
            lats.iter().cloned().fold(f64::MIN, f64::max),
        ]
    };
    let cell_of = |x: f64, y: f64| -> Option<usize> {
        let col = ((x - origin.x) / res).round();
        let row = ((origin.y - y) / res).round();
        if col < 0.0 || row < 0.0 || col >= nx as f64 || row >= ny as f64 {
            None
        } else {
            Some(row as usize * nx + col as usize)
        }
    };
    let project = |lat: f64, lon: f64| to_utm(&utm, &ll, lon, lat).unwrap_or((f64::NAN, f64::NAN));

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
    steps.end("terrain");

    // OpenStreetMap: roads and places are written here; buildings wait for
    // the merge below, and for the final terrain under them.
    let mut used_roads = false;
    let mut used_places = false;
    let mut osm_buildings: Vec<crate::lod2::Lod2Building> = Vec::new();
    if let Some(pbf) = &params.osm_pbf {
        steps.begin("osm");
        let layers = crate::osm::read_pbf(pbf, bbox, params.osm_buildings, &project, &|i, n| {
            steps.part("osm", i, n)
        })?;
        if !layers.roads.is_empty() {
            let mut f = std::io::BufWriter::new(std::fs::File::create(
                params.out_dir.join("roads.bin"),
            )?);
            crate::roads::write_binary(&mut f, &layers.roads)?;
            used_roads = true;
            let pts: usize = layers.roads.iter().map(|w| w.points.len()).sum();
            eprintln!("roads: {} ways / {pts} points → roads.bin", layers.roads.len());
        }
        if !layers.places.entries.is_empty() {
            let mut f = std::io::BufWriter::new(std::fs::File::create(
                params.out_dir.join("places.bin"),
            )?);
            crate::places::write_binary(&mut f, &layers.places)?;
            used_places = true;
            eprintln!(
                "places: {} searchable names / {} postal areas → places.bin",
                layers.places.entries.len(),
                layers.places.areas.len()
            );
        }
        osm_buildings = layers
            .buildings
            .into_iter()
            .filter(|b| meets(crate::lod2::extent(b), grid_extent))
            .collect();
        steps.end("osm");
    }

    // Buildings (LoD2 where its tiles cover the grid, OpenStreetMap on the
    // rest): per-building sidecar + building heights merged into clutter.
    // Runs BEFORE the 1 m override so measured bDOM clutter wins where present.
    //
    // Cells whose clutter comes from MEASURED building/surface data. Everything
    // else falls back to class defaults below.
    let mut measured_clutter = vec![false; nx * ny];
    // Per-cell provenance of the terrain/clutter split, written out as its own
    // layer. Without it a pack advertises one `res_m` for the whole region and
    // says nothing about the fact that the high-resolution sources usually
    // cover a tiny part of it: a Berlin pack with one 1 m tile pair has REAL
    // detail over 4 km2 of 2556 km2 (0.16%) and looks, in the UI, exactly like
    // a uniformly 10 m product. A planner cannot weigh a coverage prediction
    // without knowing whether the clutter under it was measured or
    // synthesized from a 30 m DSM.
    let mut quality = vec![DataQuality::Glo30Pseudo as u8; nx * ny];
    let raise = |q: &mut u8, to: DataQuality| {
        if DataQuality::from_code(*q).map_or(true, |cur| cur.rank() < to.rank()) {
            *q = to as u8;
        }
    };
    let mut n_lod2 = 0usize;
    // Empty unless buildings were merged: a pack without footprints has no
    // honest way to say where a building starts, and an all-zero layer would
    // read as "no buildings anywhere" rather than "not measured".
    let mut built_fraction: Vec<f32> = Vec::new();
    let mut building_top: Vec<f32> = Vec::new();
    if params.lod2_dir.is_some() || params.osm_buildings {
        steps.begin("buildings");
        let mut acc = BuiltAccum::new(nx * ny);
        // The ground LoD2 covers: each tile read, by its name, or for a file
        // named otherwise the extent of its buildings.
        let mut covered: Vec<[f64; 4]> = Vec::new();
        if let Some(dir) = &params.lod2_dir {
            let mut sidecar = std::io::BufWriter::new(std::fs::File::create(
                params.out_dir.join("buildings.jsonl"),
            )?);
            use std::io::Write as _;
            // Only the 1 km tiles meeting the grid: a district of a city-wide
            // LoD2 set reads megabytes, not the whole set.
            let files: Vec<PathBuf> = std::fs::read_dir(dir)
                .map_err(|e| PackError::Invalid(format!("LoD2 directory {}: {e}", dir.display())))?
                .flatten()
                .map(|f| f.path())
                .filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    (name.ends_with(".xml") || name.ends_with(".gml"))
                        && crate::lod2::tile_extent(name).map_or(true, |t| meets(t, grid_extent))
                })
                .collect();
            // Parse in parallel, chunked to bound in-flight memory; scatter
            // serially (fast) to keep the shared accumulator race-free.
            let mut parsed_files = 0u64;
            for chunk in files.chunks(n_threads.max(1)) {
                let parsed: Result<Vec<_>, PackError> = chunk
                    .par_iter()
                    .map(|path| {
                        crate::lod2::parse_citygml(std::io::BufReader::new(
                            std::fs::File::open(path)?,
                        ))
                    })
                    .collect();
                for (path, buildings) in chunk.iter().zip(parsed?) {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let tile = crate::lod2::tile_extent(name);
                    let mut reach = tile;
                    for b in &buildings {
                        let e = crate::lod2::extent(b);
                        if tile.is_none() {
                            reach = Some(reach.map_or(e, |r| union(r, e)));
                        }
                        if !meets(e, grid_extent) {
                            continue;
                        }
                        writeln!(sidecar, "{}", b.to_json_line(params.lod2_geometry)?)?;
                        acc.add(b, &cell_of);
                        n_lod2 += 1;
                    }
                    covered.extend(reach);
                }
                parsed_files += chunk.len() as u64;
                steps.part("buildings", parsed_files, files.len() as u64);
            }
            sidecar.flush()?;
            eprintln!("lod2: {} files parsed, {n_lod2} buildings", files.len());
        }
        let before = osm_buildings.len();
        osm_buildings.retain(|b| !lod2_covers(&covered, b));
        if before > osm_buildings.len() {
            eprintln!(
                "osm buildings: {} of {before} left out, on ground LoD2 covers",
                before - osm_buildings.len()
            );
        }
        for b in &osm_buildings {
            acc.add(b, &cell_of);
        }
        if n_lod2 + osm_buildings.len() > 0 {
            let cell_area = (res * res) as f32;
            // Keep the two facts SEPARATELY as well as blended.
            //
            // `clutter` below mixes "how tall" with "how much of the cell",
            // which is the right input for an area sweep that treats every
            // cell as a receiver and the wrong one for deciding whether a
            // particular point is on a street or inside a building.
            built_fraction = vec![0f32; nx * ny];
            building_top = vec![0f32; nx * ny];
            for i in 0..nx * ny {
                if acc.area[i] <= 0.0 {
                    continue;
                }
                built_fraction[i] = (acc.area[i] / cell_area).min(1.0);
                building_top[i] = acc.sum[i] / acc.area[i];
            }
            for i in 0..nx * ny {
                if acc.area[i] <= 0.0 {
                    continue;
                }
                // Mean height OF THE BUILDINGS, applied when they occupy a
                // meaningful share of the cell. An area average reports
                // central Berlin at ~3.6 m against a real 18.4 m median and
                // makes the city transparent.
                let built_fraction = acc.area[i] / cell_area;
                let representative = acc.sum[i] / acc.area[i];
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
                // Most of the cell's built area decides: LoD2, else OSM
                // heights tagged, else OSM defaults.
                let q = if 2.0 * acc.lod2[i] >= acc.area[i] {
                    DataQuality::Lod2Buildings
                } else if 2.0 * acc.tagged[i] >= acc.area[i] {
                    DataQuality::OsmTaggedHeights
                } else {
                    DataQuality::OsmDefaultHeights
                };
                raise(&mut quality[i], q);
            }
            if n_lod2 > 0 {
                eprintln!("lod2: {n_lod2} building records → buildings.jsonl + clutter merge");
            }
            if !osm_buildings.is_empty() {
                let census = crate::osm::height_census(&osm_buildings);
                let n = |s| census.get(&s).copied().unwrap_or(0);
                use planner_buildings::HeightSource as H;
                eprintln!(
                    "osm buildings: {} footprints ({} height tag, {} levels, {} class default) → clutter merge",
                    osm_buildings.len(),
                    n(H::OsmHeight),
                    n(H::OsmLevels),
                    n(H::Default)
                );
            }
        }
        steps.end("buildings");
    }
    let used_lod2 = n_lod2 > 0;
    let used_osm_buildings = !osm_buildings.is_empty();

    // Real 1 m override where Berlin DGM1+bDOM pairs cover the region
    // (EPSG:25833 ≈ 32633 within <1 m — used as-is; berlin1m.rs).
    let mut used_berlin_1m = false;
    if let Some(dir) = &params.berlin_1m_dir {
        steps.begin("lidar");
        // Only the tiles meeting the grid. A key is the tile's south-west
        // corner in km; tiles are 1 or 2 km, so 2 km is the safe extent.
        let pairs: Vec<_> = crate::berlin1m::find_pairs(dir)?
            .into_iter()
            .filter(|(key, _, _)| {
                let mut it = key.split('_').map(|v| v.parse::<f64>().ok());
                match (it.next().flatten(), it.next().flatten()) {
                    (Some(e), Some(n)) => {
                        meets([e * 1000.0, n * 1000.0, (e + 2.0) * 1000.0, (n + 2.0) * 1000.0], grid_extent)
                    }
                    _ => true,
                }
            })
            .collect();
        if !pairs.is_empty() {
            let mut dtm_acc = crate::berlin1m::MeanAccum::new(nx * ny);
            let mut clut_acc = crate::berlin1m::MeanAccum::new(nx * ny);
            // Parse tile pairs in parallel (the expensive part: two ~116 MB
            // XYZ texts each), scatter serially per chunk; chunking bounds
            // in-flight grids to ~32 MB × threads.
            let mut pairs_done = 0u64;
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
                    crate::berlin1m::accumulate_grids(&dgm, &dom, cell_of, &mut dtm_acc, &mut clut_acc);
                }
                pairs_done += chunk.len() as u64;
                steps.part("lidar", pairs_done, pairs.len() as u64);
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
                    // Wins over any building source: this is lidar-derived
                    // terrain AND surface at 1 m, the best evidence the pack
                    // can carry.
                    raise(&mut quality[i], DataQuality::Lidar1m);
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
        steps.end("lidar");
    }

    // WorldCover clutter-class layer (categorical; nearest-neighbor codes).
    let mut used_worldcover = false;
    let mut class_codes: Option<Vec<f32>> = None;
    if !params.worldcover_tiles.is_empty() {
        steps.begin("landcover");
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
        steps.end("landcover");
    }

    steps.begin("clutter");
    // P.1812 §3.2.1 / Table 2: where no MEASURED building data covers a
    // cell, use the Recommendation's representative clutter heights for the
    // ground-cover class instead of whatever the DSM-opening proxy produced.
    // Without this, cities with no building data propagate like open plain
    // (central Berlin at 3.6 m mean against a real 18.4 m median building
    // height).
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

    // The OpenStreetMap sidecar, now that the terrain under each building is
    // final (the 1 m override may have replaced it), after LoD2's lines when
    // this build wrote any (the LoD2 step starts the file afresh).
    if used_osm_buildings {
        use std::io::Write as _;
        let lod2_started = params.lod2_dir.is_some();
        let mut sidecar = std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(lod2_started)
                .truncate(!lod2_started)
                .open(params.out_dir.join("buildings.jsonl"))?,
        );
        for b in &mut osm_buildings {
            // The nearest cell: a building on the grid's edge may have its
            // centroid just off it.
            let col = ((b.e - origin.x) / res).round().clamp(0.0, (nx - 1) as f64) as usize;
            let row = ((origin.y - b.n) / res).round().clamp(0.0, (ny - 1) as f64) as usize;
            b.ground_z = dtm.data[row * nx + col] as f64;
            writeln!(sidecar, "{}", b.to_json_line(true)?)?;
        }
        sidecar.flush()?;
        eprintln!("osm buildings: {} records → buildings.jsonl", osm_buildings.len());
    }
    drop(osm_buildings);

    write_geotiff_f32(&params.out_dir.join("dtm.tif"), &dtm)?;
    write_geotiff_f32(&params.out_dir.join("clutter_h.tif"), &clutter)?;

    // The unblended halves of the clutter layer. Written only when buildings
    // were merged, so their presence in the manifest IS the statement that
    // this pack knows where buildings begin and end.
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
        let census: Vec<String> = DataQuality::ALL
            .iter()
            .rev()
            .map(|&q| format!("{:.2}% {}", 100.0 * count(q) / total, q.label()))
            .collect();
        eprintln!("data quality: {} -> data_quality.tif", census.join(", "));
        if count(DataQuality::Glo30Pseudo) / total > 0.5 {
            eprintln!(
                "  WARNING: most of this pack's clutter is synthesized from a 30 m DSM. \
                 Coverage predictions outside the measured area carry that uncertainty; \
                 building footprints or 1 m lidar, where they exist, make a \
                 planning-grade pack."
            );
        }
    }
    steps.end("clutter");

    // Zensus population layer (persons per pack cell).
    let mut used_zensus = false;
    if let Some(csv) = &params.zensus_csv {
        steps.begin("population");
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
            cell_of,
            res,
            &mut pop,
        )?;
        let grid =
            Grid::with_axes(origin, res, -res, nx, ny, pop).map_err(PackError::Terrain)?;
        write_geotiff_f32(&params.out_dir.join("population.tif"), &grid)?;
        used_zensus = total > 0;
        eprintln!("zensus: {total} residents inside the region → population.tif");
        steps.end("population");
    }

    steps.begin("manifest");
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
                "WARNING: no ITU maps — using world-median ΔN=45 / N0=325 \
                 (small accuracy cost)"
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
            if used_lod2 || used_osm_buildings {
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
            if used_osm_buildings {
                l.push(LicenseNotice {
                    source: "OpenStreetMap buildings".into(),
                    notice: crate::osm::OSM_BUILDINGS_NOTICE.into(),
                });
            }
            l
        },
        calibration: Vec::<CalibrationRef>::new(),
    };
    manifest.validate()?;
    std::fs::write(params.out_dir.join("manifest.json"), manifest.to_json())?;
    steps.end("manifest");
    Ok(manifest)
}

/// Whether two extents `[min_x, min_y, max_x, max_y]` meet.
fn meets(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

/// The extent holding both.
fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Whether a building stands on ground LoD2 covers: its centroid inside one
/// of the covered extents (half-open, so a centroid on a shared tile edge
/// belongs to one tile).
fn lod2_covers(covered: &[[f64; 4]], b: &crate::lod2::Lod2Building) -> bool {
    covered.iter().any(|t| t[0] <= b.e && b.e < t[2] && t[1] <= b.n && b.n < t[3])
}

/// Building heights summed over 1 m samples per pack cell, plus the built
/// AREA, so a cell reports the height of its buildings rather than that
/// height smeared across courtyards and streets (P.1812 §3.2.1
/// representative clutter height).
struct BuiltAccum {
    sum: Vec<f32>,
    /// 1 m samples of building in the cell.
    area: Vec<f32>,
    /// Of those, the samples whose height is tagged or measured rather than a
    /// class default.
    tagged: Vec<f32>,
    /// Of those, the samples of LoD2 buildings.
    lod2: Vec<f32>,
    seen: std::collections::HashSet<(i32, i32)>,
}

impl BuiltAccum {
    fn new(n: usize) -> Self {
        BuiltAccum {
            sum: vec![0.0; n],
            area: vec![0.0; n],
            tagged: vec![0.0; n],
            lod2: vec![0.0; n],
            seen: std::collections::HashSet::new(),
        }
    }

    /// One 1 m sample must be counted ONCE per cell per building, and the
    /// rasterizer cannot guarantee that: it fills each ground polygon
    /// independently, and LoD2 splits a block into `BuildingPart`s whose
    /// footprints touch and overlap at shared walls (on the cached Berlin tile,
    /// 263 299 sink calls against 246 007 distinct 1 m cells). The key is the
    /// 1 m SAMPLE, not the pack cell — deduplicating by cell would credit
    /// each cell one sample per building and collapse every built fraction to
    /// 1/(res²). And it is per BUILDING, not global: two DIFFERENT buildings
    /// covering the same ground are two real contributions.
    fn add(&mut self, b: &crate::lod2::Lod2Building, cell_of: &impl Fn(f64, f64) -> Option<usize>) {
        self.seen.clear();
        let tagged = b.source.is_tagged();
        let lod2 = b.source == planner_buildings::HeightSource::Lod2;
        crate::lod2::rasterize_building(b, |x, y, h| {
            if !self.seen.insert((x.floor() as i32, y.floor() as i32)) {
                return;
            }
            if let Some(k) = cell_of(x, y) {
                self.sum[k] += h as f32;
                self.area[k] += 1.0;
                if tagged {
                    self.tagged[k] += 1.0;
                }
                if lod2 {
                    self.lod2[k] += 1.0;
                }
            }
        });
    }
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

    /// A mistyped input path must fail in the first second, naming the path,
    /// and every wrong path at once — before any tile is opened or any layer
    /// written.
    #[test]
    fn every_bad_input_path_is_named_before_any_work_happens() {
        let dir = std::env::temp_dir().join("planner_pack_badinputs");
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = BuildParams::berlin_test(vec![dir.join("nope-dsm.tif")], dir.join("out"));
        p.osm_pbf = Some(dir.join("nope-berlin.osm.pbf"));
        p.zensus_csv = Some(dir.join("nope-zensus.csv"));
        p.lod2_dir = Some(dir.join("nope-lod2"));
        let err = build(&p).unwrap_err().to_string();
        assert!(err.starts_with("manifest invalid: 4 build input(s) unusable"), "{err}");
        for name in ["nope-dsm.tif", "nope-berlin.osm.pbf", "nope-zensus.csv", "nope-lod2"] {
            assert!(err.contains(name), "{name} not named: {err}");
        }

        let p2 = BuildParams::berlin_test(vec![], dir.join("out"));
        let e2 = build(&p2).unwrap_err().to_string();
        assert!(e2.contains("no DSM input tiles"), "{e2}");

        let mut p3 = BuildParams::berlin_test(vec![dir.join("nope-dsm.tif")], dir.join("out"));
        p3.osm_buildings = true;
        let e3 = build(&p3).unwrap_err().to_string();
        assert!(e3.contains("without an OpenStreetMap extract"), "{e3}");
    }

    /// An OpenStreetMap building on a LoD2 tile is LoD2's; one beside the
    /// tiles is OpenStreetMap's. A centroid on the edge two tiles share
    /// belongs to one of them only.
    #[test]
    fn lod2_tiles_decide_where_openstreetmap_buildings_stay() {
        let at = |e: f64, n: f64| crate::lod2::Lod2Building {
            id: "w1".into(),
            e,
            n,
            ground_z: 0.0,
            area_m2: 100.0,
            height_m: 9.0,
            source: planner_buildings::HeightSource::Default,
            rings: Vec::new(),
        };
        let covered = [
            crate::lod2::tile_extent("LoD2_33_390_5820_1_BE.xml").unwrap(),
            crate::lod2::tile_extent("LoD2_33_391_5820_1_BE.xml").unwrap(),
        ];
        assert!(lod2_covers(&covered, &at(390_500.0, 5_820_500.0)));
        assert!(lod2_covers(&covered, &at(391_000.0, 5_820_000.0)));
        assert!(!lod2_covers(&covered, &at(392_000.0, 5_820_500.0)));
        assert!(!lod2_covers(&covered, &at(390_500.0, 5_821_000.0)));
        assert!(!lod2_covers(&[], &at(390_500.0, 5_820_500.0)));
        assert_eq!(union([0.0, 1.0, 2.0, 3.0], [-1.0, 2.0, 1.0, 5.0]), [-1.0, 1.0, 2.0, 5.0]);
    }

    #[test]
    fn a_step_reports_as_it_starts_and_as_it_ends() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let f: Box<ProgressFn> = Box::new(move |p: &Progress| {
            sink.lock().unwrap().push((p.step.to_string(), p.done, p.total, p.part));
        });
        let mut s = Steps { names: vec!["terrain", "osm", "manifest"], done: 0, report: Some(&*f) };
        s.begin("terrain");
        s.end("terrain");
        s.begin("osm");
        s.part("osm", 1, 3);
        s.end("osm");
        let got = seen.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                ("terrain".to_string(), 0, 3, None),
                ("terrain".to_string(), 1, 3, None),
                ("osm".to_string(), 1, 3, None),
                ("osm".to_string(), 1, 3, Some((1, 3))),
                ("osm".to_string(), 2, 3, None),
            ]
        );
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
