use serde::{Deserialize, Serialize};

/// Radio-climatic zone along the path (P.1812 distinguishes inland, coastal
/// land, and sea).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Zone {
    Inland,
    Coastal,
    Sea,
}

/// Clutter class per profile point. Classes stay descriptive; the
/// representative clutter *height* is carried per point, so real data (LoD2,
/// nDOM, canopy rasters) always beats class defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClutterClass {
    Open,
    Water,
    LowVegetation,
    Forest,
    Suburban,
    Urban,
    DenseUrban,
    Industrial,
}

impl ClutterClass {
    /// Stable raster code (ClutterClass pack layers store these as pixels).
    pub fn code(self) -> u8 {
        match self {
            ClutterClass::Open => 0,
            ClutterClass::Water => 1,
            ClutterClass::LowVegetation => 2,
            ClutterClass::Forest => 3,
            ClutterClass::Suburban => 4,
            ClutterClass::Urban => 5,
            ClutterClass::DenseUrban => 6,
            ClutterClass::Industrial => 7,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => ClutterClass::Open,
            1 => ClutterClass::Water,
            2 => ClutterClass::LowVegetation,
            3 => ClutterClass::Forest,
            4 => ClutterClass::Suburban,
            5 => ClutterClass::Urban,
            6 => ClutterClass::DenseUrban,
            7 => ClutterClass::Industrial,
            _ => return None,
        })
    }
}

/// One point of a path profile.
///
/// Terrain and clutter are separate on purpose: P.1812 consumes both, the
/// optional ITM cross-check consumes terrain (or terrain+clutter as its
/// DSM-as-terrain mode), and no pack ever stores a merged DSM.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProfilePoint {
    /// Distance from the TX terminal along the path, meters.
    pub d_m: f64,
    /// Bare-earth elevation, meters above the vertical datum.
    pub h_terrain_m: f32,
    /// Representative clutter height above ground, meters (0 = open).
    pub h_clutter_m: f32,
    pub clutter: ClutterClass,
    pub zone: Zone,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Points ordered TX → RX; first point is at the TX (d = 0).
    pub points: Vec<ProfilePoint>,
}

impl Profile {
    pub fn length_m(&self) -> f64 {
        self.points.last().map(|p| p.d_m).unwrap_or(0.0)
    }

    /// Surface-model view (terrain + clutter), the input for DSM-style
    /// evaluation. A view, never a stored layer.
    pub fn dsm_view(&self) -> impl Iterator<Item = f64> + '_ {
        self.points
            .iter()
            .map(|p| p.h_terrain_m as f64 + p.h_clutter_m as f64)
    }
}
