//! ΔN / N0 extraction from the ITU digital maps (DN50.TXT / N050.TXT,
//! Rec. P.1812-8 §3.5 + Table 4: latitude +90° → −90°, longitude 0° → 360°,
//! 1.5° spacing → 121 × 241 values; bilinear interpolation per P.1144).
//!
//! The maps ship with the Recommendation and are NOT redistributable: the
//! pack compiler reads a locally-downloaded copy at build time and bakes only
//! the two per-region scalars into the manifest (review §8.1). Packs built
//! without the maps fall back to world-median defaults, loudly.

use crate::PackError;
use std::path::Path;

pub const ROWS: usize = 121; // +90 … −90 in 1.5° steps
pub const COLS: usize = 241; // 0 … 360 in 1.5° steps

pub struct ItuMap {
    data: Vec<f64>,
}

impl ItuMap {
    pub fn parse(text: &str) -> Result<Self, PackError> {
        let data: Vec<f64> = text
            .split_whitespace()
            .map(|t| t.parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|e| PackError::Invalid(format!("ITU map parse: {e}")))?;
        if data.len() != ROWS * COLS {
            return Err(PackError::Invalid(format!(
                "ITU map has {} values, expected {}",
                data.len(),
                ROWS * COLS
            )));
        }
        Ok(Self { data })
    }

    pub fn load(path: &Path) -> Result<Self, PackError> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// Bilinear sample at (lat, lon) degrees; lon accepts −180..360.
    pub fn sample(&self, lat_deg: f64, lon_deg: f64) -> f64 {
        let lat = lat_deg.clamp(-90.0, 90.0);
        let mut lon = lon_deg % 360.0;
        if lon < 0.0 {
            lon += 360.0;
        }
        let fr = (90.0 - lat) / 1.5; // row 0 at +90
        let fc = lon / 1.5;
        let r0 = (fr.floor() as usize).min(ROWS - 2);
        let c0 = (fc.floor() as usize).min(COLS - 2);
        let tr = fr - r0 as f64;
        let tc = fc - c0 as f64;
        let at = |r: usize, c: usize| self.data[r * COLS + c];
        let top = at(r0, c0) * (1.0 - tc) + at(r0, c0 + 1) * tc;
        let bot = at(r0 + 1, c0) * (1.0 - tc) + at(r0 + 1, c0 + 1) * tc;
        top * (1.0 - tr) + bot * tr
    }
}

/// Read (ΔN, N0) at a path-centre coordinate from a directory holding
/// DN50.TXT and N050.TXT.
pub fn extract_dn_n0(maps_dir: &Path, lat_deg: f64, lon_deg: f64) -> Result<(f64, f64), PackError> {
    let dn = ItuMap::load(&maps_dir.join("DN50.TXT"))?.sample(lat_deg, lon_deg);
    let n0 = ItuMap::load(&maps_dir.join("N050.TXT"))?.sample(lat_deg, lon_deg);
    Ok((dn, n0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Synthetic map with value = 1000·row + col: verifies indexing and
    /// bilinear weights exactly.
    #[test]
    fn synthetic_indexing_and_bilinear() {
        let mut s = String::new();
        for r in 0..ROWS {
            for c in 0..COLS {
                s.push_str(&format!("{} ", 1000 * r + c));
            }
            s.push('\n');
        }
        let m = ItuMap::parse(&s).unwrap();
        // Exact grid point: lat 90 − 1.5·2 = 87, lon 1.5·3 = 4.5 → row 2 col 3.
        assert_eq!(m.sample(87.0, 4.5), 2003.0);
        // Halfway in longitude between cols 3 and 4 on row 2.
        assert_eq!(m.sample(87.0, 4.5 + 0.75), 2003.5);
        // Halfway in latitude between rows 2 and 3 at col 0.
        assert_eq!(m.sample(87.0 - 0.75, 0.0), 2500.0);
        // Negative longitude wraps: −1.5° ≡ 358.5° = col 239.
        assert_eq!(m.sample(90.0, -1.5), 239.0);
    }

    #[test]
    fn wrong_size_is_rejected() {
        assert!(ItuMap::parse("1 2 3").is_err());
    }

    /// Real-map spot check (skips when the ITU maps are not in the download
    /// cache). Cross-validated 2026-08-30 against Py1812's own npz lookup for
    /// a Berlin path centre: DN 37.4037, N0 319.9004 — we must land within a
    /// whisker of those.
    #[test]
    fn berlin_values_from_real_maps() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../packs/.cache/itu");
        if !dir.join("DN50.TXT").exists() {
            eprintln!("SKIP: ITU maps not present at {}", dir.display());
            return;
        }
        let (dn, n0) = extract_dn_n0(&dir, 52.52, 13.405).unwrap();
        assert!((dn - 37.4037).abs() < 0.05, "Berlin ΔN {dn} vs Py1812 37.4037");
        assert!((n0 - 319.9004).abs() < 0.05, "Berlin N0 {n0} vs Py1812 319.9004");
    }
}
