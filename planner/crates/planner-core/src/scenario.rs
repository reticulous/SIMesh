use crate::geo::GeoPos;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SiteId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshFlavor {
    MeshCore,
    Meshtastic,
    CustomFw,
    Reticulum,
}

/// One radio configuration = one flood/airtime domain's air settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioConfig {
    pub freq_mhz: f64,
    pub bw_khz: f64,
    pub spreading_factor: Option<u8>,
    /// Community preset name (e.g. a MeshCore/Meshtastic channel preset).
    pub preset: String,
    pub duty_cycle_pct: f64,
}

/// A cell is a flood/airtime domain (review §8.3) — cells are first-class in
/// every scenario even while the v1 solver optimizes one cell at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub id: CellId,
    pub name: String,
    pub flavor: MeshFlavor,
    pub radio: RadioConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiteStatus {
    Candidate,
    Planned,
    Installed,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Site {
    pub id: SiteId,
    pub cell: CellId,
    pub name: String,
    pub pos: GeoPos,
    /// Antenna height above ground, meters.
    pub h_agl_m: f64,
    /// Floor number for household installs; UIs derive h_agl_m ≈ floor × 3 m
    /// when set, tying into crowdsourced building:levels (review §8.1).
    pub floor: Option<u8>,
    /// Indoor install ("node behind a window") — enables the optional P.2109
    /// building-entry-loss term.
    pub indoor: bool,
    pub status: SiteStatus,
    /// Device preset id (antenna gain / sensitivity table).
    pub device: String,
    /// Transmit power configured for THIS node (dBm). Per-node by design:
    /// a single network-wide power is what degrades a CSMA mesh, because
    /// carrier-sense range grows faster than useful data range and merges
    /// separate cells into one collision domain. None = use the device
    /// preset's power.
    pub tx_power_dbm: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub cells: Vec<Cell>,
    pub sites: Vec<Site>,
}

impl Scenario {
    pub fn sites_in(&self, cell: CellId) -> impl Iterator<Item = &Site> + '_ {
        self.sites.iter().filter(move |s| s.cell == cell)
    }
}
