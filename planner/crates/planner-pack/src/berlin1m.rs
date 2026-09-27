//! Berlin open 1 m data ingestion: DGM1 (bare-earth terrain) + bDOM
//! (image-based surface model), both dl-de/zero-2.0, both delivered as XYZ
//! ASCII in 2×2 km tiles named by their SW corner in EPSG:25833 km
//! (e.g. `dgm1_33_392_5820_2_be.xyz`, `dom1_33_392_5820_2_be_2026.xyz`).
//!
//! Real clutter = bDOM − DGM1 (clamped ≥ 0). EPSG:25833 (ETRS89/UTM33) vs
//! the pack's EPSG:32633 (WGS84/UTM33) differ by well under a meter — the
//! coordinates are used as-is, documented here.
//!
//! Verified 2026-08-31 against tile 392_5820: points at half-meter centers,
//! 1 m spacing, south-to-north scan order.

use crate::PackError;
use planner_core::geo::Xy;
use planner_terrain::Grid;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

pub const BERLIN_1M_NOTICE: &str =
    "Geoportal Berlin: ATKIS\u{ae} DGM1 und bDOM \u{2014} Datenlizenz Deutschland \u{2013} Zero \u{2013} Version 2.0 (dl-de/zero-2.0)";

/// Parse one XYZ tile into a north-up 1 m Grid.
pub fn parse_xyz<R: Read>(reader: R) -> Result<Grid, PackError> {
    let mut pts: Vec<(f64, f64, f32)> = Vec::with_capacity(4_000_000);
    let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
    let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
    for line in BufReader::new(reader).lines() {
        let line = line?;
        let mut it = line.split_ascii_whitespace();
        let (Some(xs), Some(ys), Some(zs)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(x), Ok(y), Ok(z)) = (xs.parse::<f64>(), ys.parse::<f64>(), zs.parse::<f32>())
        else {
            continue;
        };
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        pts.push((x, y, z));
    }
    if pts.len() < 4 {
        return Err(PackError::Invalid("XYZ tile has too few points".into()));
    }
    let res = 1.0;
    let width = ((max_x - min_x) / res).round() as usize + 1;
    let height = ((max_y - min_y) / res).round() as usize + 1;
    if width * height < pts.len() {
        return Err(PackError::Invalid(format!(
            "XYZ grid inference failed: {}×{} < {} points",
            width,
            height,
            pts.len()
        )));
    }
    let mut data = vec![f32::NAN; width * height];
    for (x, y, z) in pts {
        let col = ((x - min_x) / res).round() as usize;
        let row = ((max_y - y) / res).round() as usize; // north-up
        data[row * width + col] = z;
    }
    Grid::with_axes(Xy { x: min_x, y: max_y }, res, -res, width, height, data)
        .map_err(PackError::Terrain)
}

/// Tile key like "392_5820" from a Berlin XYZ filename; classifies the
/// product by prefix (dgm1 = terrain, dom1/bdom = surface).
pub fn classify(file_name: &str) -> Option<(&'static str, String)> {
    let lower = file_name.to_ascii_lowercase();
    if !lower.ends_with(".xyz") {
        return None;
    }
    let kind = if lower.starts_with("dgm1") {
        "dgm1"
    } else if lower.starts_with("dom1") || lower.starts_with("bdom") {
        "dom1"
    } else {
        return None;
    };
    // …_33_<E>_<N>_… → key "<E>_<N>"
    let parts: Vec<&str> = lower.trim_end_matches(".xyz").split('_').collect();
    let pos = parts.iter().position(|p| *p == "33")?;
    let (e, n) = (parts.get(pos + 1)?, parts.get(pos + 2)?);
    if e.parse::<u32>().is_err() || n.parse::<u32>().is_err() {
        return None;
    }
    Some((kind, format!("{e}_{n}")))
}

/// Scatter-accumulator: mean of fine samples per target-pack pixel.
/// f32 sums + u32 counts by the footprint rule — a ≤2 500-sample cell of
/// heights around 10² m loses nothing meaningful to f32.
///
/// Also tracks the subset of samples that are actually OBSTRUCTIONS (above
/// `BUILT_THRESHOLD_M`). P.1812's "representative clutter height" (§3.2.1,
/// Table 2: dense urban 20 m, urban 15 m, suburban 10 m) is the height of
/// the thing the wave diffracts over — NOT an area average that dilutes
/// roofs with the courtyards and streets between them. Measured on the
/// Berlin pack, area-averaging put central Berlin at a mean of 3.6 m
/// against a real median building height of 18.4 m, which made the city
/// propagate like open plain.
pub struct MeanAccum {
    pub sum: Vec<f32>,
    pub count: Vec<u32>,
    pub built_sum: Vec<f32>,
    pub built_count: Vec<u32>,
}

/// A sample counts as an obstruction above this height (m).
pub const BUILT_THRESHOLD_M: f32 = 2.0;

impl MeanAccum {
    pub fn new(len: usize) -> Self {
        Self {
            sum: vec![0.0; len],
            count: vec![0; len],
            built_sum: vec![0.0; len],
            built_count: vec![0; len],
        }
    }

    pub fn add(&mut self, idx: usize, v: f32) {
        self.sum[idx] += v;
        self.count[idx] += 1;
        if v > BUILT_THRESHOLD_M {
            self.built_sum[idx] += v;
            self.built_count[idx] += 1;
        }
    }

    /// Representative height for a cell: the mean over obstructing samples
    /// when the cell is meaningfully built up, else the plain mean (open
    /// ground, low vegetation).
    pub fn representative(&self, idx: usize) -> Option<f32> {
        if self.count[idx] == 0 {
            return None;
        }
        let built_fraction = self.built_count[idx] as f32 / self.count[idx] as f32;
        if built_fraction >= 0.15 {
            Some(self.built_sum[idx] / self.built_count[idx] as f32)
        } else {
            Some(self.sum[idx] / self.count[idx] as f32)
        }
    }
}

/// Paired 1 m tiles found in a directory of extracted XYZ files.
pub fn find_pairs(dir: &Path) -> Result<Vec<(String, PathBuf, PathBuf)>, PackError> {
    let mut dgm: HashMap<String, PathBuf> = HashMap::new();
    let mut dom: HashMap<String, PathBuf> = HashMap::new();
    for entry in walk(dir)? {
        let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        match classify(&name) {
            Some(("dgm1", key)) => {
                dgm.insert(key, entry);
            }
            Some(("dom1", key)) => {
                dom.insert(key, entry);
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (key, d) in &dgm {
        if let Some(s) = dom.get(key) {
            out.push((key.clone(), d.clone(), s.clone()));
        } else {
            eprintln!("berlin-1m: tile {key} has DGM1 but no bDOM — skipped");
        }
    }
    for key in dom.keys() {
        if !dgm.contains_key(key) {
            eprintln!("berlin-1m: tile {key} has bDOM but no DGM1 — skipped");
        }
    }
    out.sort();
    Ok(out)
}

fn walk(dir: &Path) -> Result<Vec<PathBuf>, PackError> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let p = entry?.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// Accumulate one tile pair's terrain and clutter means onto a target pack
/// grid (given via closure world→index to keep this free of grid layout
/// assumptions).
pub fn accumulate_pair(
    dgm1_path: &Path,
    dom1_path: &Path,
    target_index: impl FnMut(f64, f64) -> Option<usize>,
    dtm_acc: &mut MeanAccum,
    clut_acc: &mut MeanAccum,
) -> Result<(), PackError> {
    let dgm = parse_xyz(std::fs::File::open(dgm1_path)?)?;
    let dom = parse_xyz(std::fs::File::open(dom1_path)?)?;
    accumulate_grids(&dgm, &dom, target_index, dtm_acc, clut_acc);
    Ok(())
}

/// Accumulation over already-parsed grids — the parallel build parses pairs
/// on worker threads and scatters serially through this.
pub fn accumulate_grids(
    dgm: &planner_terrain::Grid,
    dom: &planner_terrain::Grid,
    mut target_index: impl FnMut(f64, f64) -> Option<usize>,
    dtm_acc: &mut MeanAccum,
    clut_acc: &mut MeanAccum,
) {
    for row in 0..dgm.height {
        let y = dgm.origin.y + row as f64 * dgm.dy_m;
        for col in 0..dgm.width {
            let t = dgm.data[row * dgm.width + col];
            if !t.is_finite() {
                continue;
            }
            let x = dgm.origin.x + col as f64 * dgm.dx_m;
            let Some(idx) = target_index(x, y) else { continue };
            dtm_acc.add(idx, t);
            if let Some(s) = dom.sample_bilinear(Xy { x, y }) {
                if s.is_finite() {
                    clut_acc.add(idx, (s - t).max(0.0));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xyz_parse_orientation_and_values() {
        // 3×2 grid at half-meter centers, south-to-north order like the feed.
        let text = "\
100.5 200.5 10\n101.5 200.5 11\n102.5 200.5 12\n\
100.5 201.5 20\n101.5 201.5 21\n102.5 201.5 22\n";
        let g = parse_xyz(text.as_bytes()).unwrap();
        assert_eq!((g.width, g.height), (3, 2));
        assert_eq!(g.dy_m, -1.0);
        // Row 0 = northern row (y = 201.5).
        assert_eq!(g.data[0], 20.0);
        assert_eq!(g.data[1 * 3 + 2], 12.0);
        assert!((g.origin.y - 201.5).abs() < 1e-9);
    }

    #[test]
    fn filename_classification() {
        assert_eq!(
            classify("dgm1_33_392_5820_2_be.xyz"),
            Some(("dgm1", "392_5820".into()))
        );
        assert_eq!(
            classify("dom1_33_392_5820_2_be_2026.xyz"),
            Some(("dom1", "392_5820".into()))
        );
        assert_eq!(classify("readme.txt"), None);
    }

    #[test]
    fn accumulate_builds_clutter_means() {
        let dir = std::env::temp_dir().join("planner_b1m_test");
        std::fs::create_dir_all(&dir).unwrap();
        // Terrain flat 10; surface 10 except one 4-point building at 22.
        let mut dgm = String::new();
        let mut dom = String::new();
        for y in 0..4 {
            for x in 0..4 {
                dgm.push_str(&format!("{}.5 {}.5 10\n", 100 + x, 200 + y));
                let s = if (1..3).contains(&x) && (1..3).contains(&y) { 22 } else { 10 };
                dom.push_str(&format!("{}.5 {}.5 {}\n", 100 + x, 200 + y, s));
            }
        }
        let dp = dir.join("dgm1_33_100_200_2_be.xyz");
        let sp = dir.join("dom1_33_100_200_2_be_2026.xyz");
        std::fs::write(&dp, dgm).unwrap();
        std::fs::write(&sp, dom).unwrap();

        // Single target cell covering everything.
        let mut dtm = MeanAccum::new(1);
        let mut clut = MeanAccum::new(1);
        accumulate_pair(&dp, &sp, |_, _| Some(0), &mut dtm, &mut clut).unwrap();
        assert_eq!(dtm.count[0], 16);
        assert!((dtm.sum[0] / 16.0 - 10.0).abs() < 1e-5);
        // Mean clutter = 4 building points × 12 m / 16 = 3 m.
        let c = clut.sum[0] / clut.count[0] as f32;
        assert!((c - 3.0).abs() < 0.2, "{c}");
    }
}
