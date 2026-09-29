//! ESA WorldCover 10 m → ClutterClass layer.
//!
//! Source: AWS Open Data `esa-worldcover`, v200 (2021), 3°×3° COG tiles
//! (e.g. ESA_WorldCover_10m_2021_v200_N51E012_Map.tif, 76 MB), EPSG:4326,
//! uint8 classes; CC BY 4.0. Classes are CATEGORICAL — sampling is nearest
//! neighbor, never bilinear. The pack layer stores our stable
//! `ClutterClass::code()` values (per-class calibration offsets key on them).

use crate::PackError;
use planner_core::profile::ClutterClass;
use planner_terrain::cog::CogReader;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::PathBuf;

/// WorldCover v200 class code → our clutter class.
pub fn map_class(wc: u8) -> ClutterClass {
    match wc {
        10 => ClutterClass::Forest,        // tree cover
        20 | 30 => ClutterClass::LowVegetation, // shrub, grassland
        40 => ClutterClass::Open,          // cropland
        50 => ClutterClass::Urban,         // built-up
        60 | 70 | 100 => ClutterClass::Open, // bare, snow, moss/lichen
        80 => ClutterClass::Water,         // permanent water
        90 | 95 => ClutterClass::LowVegetation, // wetland, mangrove
        _ => ClutterClass::Open,
    }
}

pub const WORLDCOVER_NOTICE: &str = "\u{a9} ESA WorldCover project 2021 / Contains modified \
Copernicus Sentinel data (2021) processed by the ESA WorldCover consortium \u{2014} CC BY 4.0";

pub struct WorldCoverTiles<R: Read + Seek> {
    readers: Vec<CogReader<R>>,
}

impl WorldCoverTiles<BufReader<File>> {
    pub fn open(paths: &[PathBuf]) -> Result<Self, PackError> {
        let mut readers = Vec::new();
        for p in paths {
            let mut r = CogReader::open(p)?;
            // Row-sweep access + many parallel workers: band-sized cache.
            r.tune_for_row_sweep();
            readers.push(r);
        }
        Ok(Self { readers })
    }
}

impl<R: Read + Seek> WorldCoverTiles<R> {
    /// Nearest-neighbor class at (lon, lat); None outside all tiles.
    pub fn class_at(&mut self, lon: f64, lat: f64) -> Result<Option<ClutterClass>, PackError> {
        for t in self.readers.iter_mut() {
            let m = *t.meta();
            let col = ((lon - m.origin.x) / m.dx).round();
            let row = ((lat - m.origin.y) / m.dy).round();
            if col < 0.0 || row < 0.0 || col >= m.width as f64 || row >= m.height as f64 {
                continue;
            }
            let v = t.pixel(col as u32, row as u32)?;
            return Ok(Some(map_class(v as u8)));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use planner_core::geo::Xy;
    use std::path::PathBuf;

    #[test]
    fn class_mapping_is_total_and_sane() {
        assert_eq!(map_class(10), ClutterClass::Forest);
        assert_eq!(map_class(50), ClutterClass::Urban);
        assert_eq!(map_class(80), ClutterClass::Water);
        assert_eq!(map_class(30), ClutterClass::LowVegetation);
        assert_eq!(map_class(255), ClutterClass::Open); // unknown → open
    }

    /// Real-tile spot checks (skip when the tile isn't downloaded):
    /// Alexanderplatz = built-up, Großer Müggelsee = water, Grunewald =
    /// forest — three unambiguous Berlin ground truths.
    #[test]
    fn berlin_ground_truths() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../testbed/geodata/.cache/worldcover/ESA_WorldCover_10m_2021_v200_N51E012_Map.tif");
        if !path.exists() {
            eprintln!("SKIP: WorldCover tile not downloaded ({})", path.display());
            return;
        }
        let mut wc = WorldCoverTiles::open(&[path]).unwrap();
        assert_eq!(
            wc.class_at(13.4132, 52.5219).unwrap(),
            Some(ClutterClass::Urban),
            "Alexanderplatz"
        );
        assert_eq!(
            wc.class_at(13.6493, 52.4371).unwrap(),
            Some(ClutterClass::Water),
            "Großer Müggelsee"
        );
        assert_eq!(
            wc.class_at(13.2210, 52.4830).unwrap(),
            Some(ClutterClass::Forest),
            "Grunewald"
        );
        // Outside the tile → None.
        assert_eq!(wc.class_at(2.35, 48.85).unwrap(), None, "Paris is elsewhere");
        let _ = Xy { x: 0.0, y: 0.0 };
    }
}
