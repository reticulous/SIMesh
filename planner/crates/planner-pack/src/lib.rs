//! Pack manifest — the contract between the pack compiler (`build`, which
//! reads files already on disk and never the network) and the fully-offline
//! readers.
//!
//! Terrain and clutter are separate layers by schema rule; ΔN/N0 are baked as
//! region scalars because the ITU maps are not redistributable; every
//! third-party source carries its license notice verbatim so the app can
//! display attributions offline.

pub mod berlin1m;
pub mod build;
pub mod itu_maps;
pub mod lod2;
pub mod nodes;
pub mod osm;
pub mod places;
pub mod roads;
pub mod worldcover;
pub mod zensus;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PackError {
    #[error("manifest parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("manifest invalid: {0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("terrain: {0}")]
    Terrain(#[from] planner_terrain::TerrainError),
    #[error("projection: {0}")]
    Proj(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LayerKind {
    /// Bare-earth DTM raster (COG).
    TerrainDtm,
    /// Merged surface model (terrain + structures + canopy), e.g. raw GLO-30.
    /// v0 BOOTSTRAP ONLY: violates the split-layer schema rule and is
    /// accepted until the DTM + clutter split lands in the compiler; the
    /// engine treats it as terrain (DSM-as-terrain mode, review §8.1).
    SurfaceDsm,
    /// Clutter height above ground (buildings + canopy), raster (COG).
    ClutterHeight,
    /// Clutter class raster (small enum), for per-class calibration offsets.
    ClutterClass,
    /// Per-building records (`buildings.jsonl`, one `lod2::Lod2Building` per
    /// line) from LoD2 or OpenStreetMap, each saying where its height came
    /// from.
    Buildings,
    /// Population grid (Zensus 100 m / WorldPop) for household weighting.
    Population,
    /// Basemap tiles (PMTiles) for the UI.
    BasemapPmtiles,
    /// Optional 1 m focus tile (DGM1/bDOM cutout) around chosen sites.
    FocusTile,
    /// Road and rail polylines for orientation (OSM/ODbL), projected to the
    /// pack CRS. Display only — never an input to propagation.
    Roads,
    /// Searchable place/street/postal-code index with postal boundary rings
    /// (OSM/ODbL), projected to the pack CRS. Display and navigation only.
    Places,
    /// Per-cell provenance of the terrain/clutter split (see `DataQuality`).
    /// Not an input to propagation — an input to TRUSTING it. A pack states
    /// one `res_m`, but its high-resolution sources rarely cover the whole
    /// region, and without this layer a synthesized 30 m clutter estimate is
    /// indistinguishable on screen from lidar at 1 m.
    DataQuality,
    /// Deployed nodes projected to the pack CRS (`nodes.rs`). The compiler
    /// never writes this layer: nodes belong to nodesets, not to ground. A
    /// reader still accepts one found in a pack.
    Nodes,
    /// Fraction of each cell covered by a building footprint (LoD2 or
    /// OpenStreetMap), 0..1.
    ///
    /// The street/building boundary, at the resolution the footprints were
    /// rasterized (1 m), reduced to one number per pack cell. It exists
    /// because [`ClutterHeight`](LayerKind::ClutterHeight) CONFLATES two
    /// independent facts — how tall the buildings in a cell are, and how much
    /// of the cell they cover — into a single blended height, and a consumer
    /// given only that number cannot tell a narrow street lined with 22 m
    /// Vorderhäuser from an open yard with one shed in it.
    ///
    /// Splitting them is what makes it possible to answer per-geometry rather
    /// than per-cell: a cell at 0.0 is street, courtyard, park or water; at
    /// 1.0 it is building interior; in between it straddles a facade and must
    /// be reported as straddling rather than averaged into a value that
    /// describes neither side.
    BuiltFraction,
    /// Representative height of the buildings in a cell, metres above ground.
    ///
    /// The mean over BUILT samples only, with no built-fraction scaling — the
    /// unblended half of what `ClutterHeight` mixes. This is the obstacle a
    /// path crossing the building actually meets, and it is what a profile
    /// wants; `ClutterHeight` remains the right input for an area sweep that
    /// treats every cell as a receiver.
    BuildingTop,
    /// A layer this binary does not know about.
    ///
    /// Packs and binaries are versioned SEPARATELY — a pack is built on one
    /// machine and served from others — so a new layer kind must not be able
    /// to brick an older reader. Without this, adding `BuiltFraction` made
    /// every manifest carrying it fail `serde` outright, `PackManifest::
    /// from_json` return `PackError::Parse`, and `planner web` exit(1) on
    /// startup: one unrecognised layer name would take down the whole server
    /// rather than the one feature it belongs to.
    ///
    /// Every consumer looks a layer up by kind and treats absence as "this
    /// pack cannot answer that", which is exactly the right behaviour here,
    /// so unknown layers become invisible instead of fatal.
    ///
    /// The original name is NOT kept: this enum is `Copy` and every reader
    /// matches on the kind rather than printing it. A manifest read through
    /// this type and written back out would therefore lose the name — nothing
    /// does that (manifests are only ever written fresh by the compiler), and
    /// this comment is here so it stays that way.
    Unknown,
}

/// Unknown kinds deserialize to [`LayerKind::Unknown`] rather than failing.
///
/// Hand-written because `#[serde(other)]` is only permitted on internally or
/// adjacently tagged enums, and this one serializes as a plain string. The
/// name table is derived from `Serialize` at run time instead of being
/// repeated here, so a new variant cannot silently fail to parse the name the
/// compiler actually writes.
impl<'de> serde::Deserialize<'de> for LayerKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(LayerKind::KNOWN
            .iter()
            .copied()
            .find(|k| serde_json::to_value(k).ok().and_then(|v| v.as_str().map(str::to_owned))
                == Some(s.clone()))
            .unwrap_or(LayerKind::Unknown))
    }
}

impl LayerKind {
    /// Every kind this binary understands. `Unknown` is deliberately absent —
    /// it is the fallback, not a name that can appear in a manifest.
    pub const KNOWN: &'static [LayerKind] = &[
        LayerKind::TerrainDtm,
        LayerKind::SurfaceDsm,
        LayerKind::ClutterHeight,
        LayerKind::ClutterClass,
        LayerKind::Buildings,
        LayerKind::Population,
        LayerKind::BasemapPmtiles,
        LayerKind::FocusTile,
        LayerKind::Roads,
        LayerKind::Places,
        LayerKind::DataQuality,
        LayerKind::Nodes,
        LayerKind::BuiltFraction,
        LayerKind::BuildingTop,
    ];
}

/// Where a pack cell's terrain/clutter split actually came from.
///
/// Stored as raster pixel values in the `DataQuality` layer. The codes are
/// stable wire values, not a ranking: [`DataQuality::rank`] orders them worst
/// to best, and a source only overwrites a cell whose current code ranks
/// lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum DataQuality {
    /// Copernicus GLO-30 (30 m) resampled to the pack grid, with terrain and
    /// clutter SYNTHESIZED by morphological opening. This is a bootstrap, not
    /// a measurement: individual buildings do not exist in it at all.
    Glo30Pseudo = 0,
    /// Clutter heights merged from LoD2 building models. Terrain is still the
    /// GLO-30 pseudo-DTM underneath.
    Lod2Buildings = 1,
    /// Real lidar terrain AND surface at 1 m (e.g. Berlin DGM1 + bDOM),
    /// averaged into pack cells. The only tier where both halves are measured.
    Lidar1m = 2,
    /// Clutter heights merged from OpenStreetMap footprints, most of the
    /// cell's built area carrying a class-default height (no `height` or
    /// `building:levels` tag). The footprints are mapped; the heights are
    /// guesses by building type.
    OsmDefaultHeights = 3,
    /// Clutter heights merged from OpenStreetMap footprints, most of the
    /// cell's built area carrying a tagged `height` or `building:levels`.
    OsmTaggedHeights = 4,
}

impl DataQuality {
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => DataQuality::Glo30Pseudo,
            1 => DataQuality::Lod2Buildings,
            2 => DataQuality::Lidar1m,
            3 => DataQuality::OsmDefaultHeights,
            4 => DataQuality::OsmTaggedHeights,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            DataQuality::Glo30Pseudo => "GLO-30 synthesized",
            DataQuality::Lod2Buildings => "LoD2 buildings",
            DataQuality::Lidar1m => "lidar 1 m",
            DataQuality::OsmDefaultHeights => "OSM buildings, default heights",
            DataQuality::OsmTaggedHeights => "OSM buildings, tagged heights",
        }
    }
    /// Worst to best: GLO-30 < OSM default heights < OSM tagged heights <
    /// LoD2 < lidar.
    pub fn rank(self) -> u8 {
        match self {
            DataQuality::Glo30Pseudo => 0,
            DataQuality::OsmDefaultHeights => 1,
            DataQuality::OsmTaggedHeights => 2,
            DataQuality::Lod2Buildings => 3,
            DataQuality::Lidar1m => 4,
        }
    }
    pub const ALL: &'static [DataQuality] = &[
        DataQuality::Glo30Pseudo,
        DataQuality::OsmDefaultHeights,
        DataQuality::OsmTaggedHeights,
        DataQuality::Lod2Buildings,
        DataQuality::Lidar1m,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerMeta {
    pub kind: LayerKind,
    /// Path relative to the pack root.
    pub path: String,
    /// Ground resolution where applicable.
    pub res_m: Option<f64>,
}

/// Region-level scalars and identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionMeta {
    /// [min_lon, min_lat, max_lon, max_lat], WGS84.
    pub bbox: [f64; 4],
    /// Local metric CRS all rasters share (e.g. 25833 for Berlin/Brandenburg).
    pub crs_epsg: u32,
    /// ΔN (N-units/km) extracted from the ITU maps at pack build time.
    pub delta_n: f64,
    /// N0 (N-units), same provenance.
    pub n0: f64,
}

/// Verbatim attribution the app must be able to show offline (e.g. the GLO-30
/// notice, dl-de/by-2-0 attributions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicenseNotice {
    pub source: String,
    pub notice: String,
}

/// Named, versioned calibration profile reference (review §8.2: fitted
/// offsets ship as data, never silently baked into models).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationRef {
    /// e.g. "berlin-2026Q4".
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackManifest {
    pub name: String,
    pub version: String,
    pub region: RegionMeta,
    pub layers: Vec<LayerMeta>,
    pub licenses: Vec<LicenseNotice>,
    pub calibration: Vec<CalibrationRef>,
}

impl PackManifest {
    pub fn from_json(s: &str) -> Result<Self, PackError> {
        let m: Self = serde_json::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("manifest serializes")
    }

    pub fn validate(&self) -> Result<(), PackError> {
        if !self
            .layers
            .iter()
            .any(|l| matches!(l.kind, LayerKind::TerrainDtm | LayerKind::SurfaceDsm))
        {
            return Err(PackError::Invalid(
                "pack has no terrain layer (TerrainDtm or bootstrap SurfaceDsm)".into(),
            ));
        }
        let [minx, miny, maxx, maxy] = self.region.bbox;
        if minx >= maxx || miny >= maxy {
            return Err(PackError::Invalid("bbox is empty or inverted".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PackManifest {
        PackManifest {
            name: "berlin-brandenburg".into(),
            version: "0.0.1".into(),
            region: RegionMeta {
                bbox: [11.2, 51.3, 14.8, 53.6],
                crs_epsg: 25833,
                delta_n: 41.3,
                n0: 320.5,
            },
            layers: vec![
                LayerMeta { kind: LayerKind::TerrainDtm, path: "dtm.tif".into(), res_m: Some(30.0) },
                LayerMeta { kind: LayerKind::ClutterHeight, path: "clutter_h.tif".into(), res_m: Some(10.0) },
            ],
            licenses: vec![LicenseNotice {
                source: "Copernicus GLO-30".into(),
                notice: "© DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH …".into(),
            }],
            calibration: vec![],
        }
    }

    #[test]
    fn manifest_roundtrips() {
        let m = sample();
        let j = m.to_json();
        let back = PackManifest::from_json(&j).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_without_terrain_is_invalid() {
        let mut m = sample();
        m.layers.retain(|l| l.kind != LayerKind::TerrainDtm);
        assert!(m.validate().is_err());
    }

    /// A layer kind from a NEWER compiler must not take down an older reader.
    ///
    /// Packs and binaries version separately: a pack is built on one machine
    /// and served from several others, which are not upgraded in lockstep.
    /// Before this, adding `BuiltFraction` meant every manifest naming it
    /// failed `serde` outright — `from_json` returned `PackError::Parse` and
    /// `planner web` called `exit(1)` on startup. One unrecognised string
    /// took down the entire server rather than the single feature it belonged
    /// to, and the failure would have arrived on the operator's next pack
    /// rebuild rather than at the commit that caused it.
    #[test]
    fn a_layer_kind_from_a_newer_compiler_is_ignored_and_not_fatal() {
        let m = sample();
        let mut v: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
        v["layers"].as_array_mut().unwrap().push(serde_json::json!({
            "kind": "SomethingInventedNextYear",
            "path": "future.tif",
            "res_m": 5.0
        }));
        let parsed = PackManifest::from_json(&serde_json::to_string(&v).unwrap())
            .expect("an unknown layer kind must not fail the whole manifest");
        assert!(parsed.layers.iter().any(|l| l.kind == LayerKind::Unknown));
        // And it stays invisible: every reader looks a layer up BY KIND, so
        // an unknown one must never be mistaken for a known one.
        for known in LayerKind::KNOWN {
            let n = parsed.layers.iter().filter(|l| l.kind == *known).count();
            let want = m.layers.iter().filter(|l| l.kind == *known).count();
            assert_eq!(n, want, "{known:?} count changed when an unknown layer was added");
        }
    }

    /// Every known kind must survive the hand-written `Deserialize`.
    ///
    /// It resolves names by round-tripping through `Serialize` rather than
    /// repeating a match, so a new variant cannot fail to parse the name the
    /// compiler writes — but only if it is listed in `KNOWN`. A variant added
    /// to the enum and forgotten here would deserialize to `Unknown` and its
    /// layer would silently vanish from every pack.
    #[test]
    fn every_known_layer_kind_parses_back_to_itself() {
        for k in LayerKind::KNOWN {
            let j = serde_json::to_string(k).unwrap();
            let back: LayerKind = serde_json::from_str(&j).unwrap();
            assert_eq!(back, *k, "{k:?} serialized as {j} and did not come back");
        }
        assert!(
            !LayerKind::KNOWN.contains(&LayerKind::Unknown),
            "Unknown is the fallback, not a name a manifest may carry"
        );
    }
}
