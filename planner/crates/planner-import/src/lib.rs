//! Importers. Node lists (`meshcore_map`, `potatomesh::parse_node_rows`)
//! become nodesets through `planner-job nodes-import`; measurements normalize
//! into [`Observation`], where the position-quality flag gates what an
//! observation may calibrate.

pub mod meshcore_map;
pub mod potatomesh;
pub mod sensing;
pub mod rangetest;

use planner_core::geo::GeoPos;
use serde::{Deserialize, Serialize};

/// How trustworthy a position is — gates calibration use (review §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionQuality {
    GpsFix,
    FixedSite,
    /// Deliberately truncated (e.g. public Meshtastic MQTT broker).
    Truncated,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceTag {
    MeshtasticRangeTest,
    PotatoMesh,
    MeshCoreCli,
    Mqtt,
    CustomFw,
}

/// One link measurement: RX heard TX. The common denominator across all four
/// mesh flavors; fields absent at a source stay None rather than being faked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub time_unix: Option<i64>,
    pub tx_id: String,
    pub rx_id: String,
    pub tx_pos: Option<GeoPos>,
    pub rx_pos: Option<GeoPos>,
    pub pos_quality: PositionQuality,
    pub snr_db: Option<f32>,
    /// Present in current Meshtastic rangetest.csv (`rx rssi`, verified
    /// 2026-08-30 against firmware master); absent in older files and some
    /// sources.
    pub rssi_dbm: Option<f32>,
    pub freq_mhz: Option<f64>,
    pub source: SourceTag,
}
