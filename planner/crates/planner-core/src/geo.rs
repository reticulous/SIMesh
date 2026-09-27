use serde::{Deserialize, Serialize};

/// WGS84 position in degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GeoPos {
    pub lat_deg: f64,
    pub lon_deg: f64,
}

/// Planar coordinates in meters in a local metric CRS (ETRS89/UTM zone for
/// Germany; the pack's `RegionMeta.crs_epsg` names it).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Xy {
    pub x: f64,
    pub y: f64,
}

impl Xy {
    pub fn dist_m(&self, other: &Xy) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}
