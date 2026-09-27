use crate::profile::Profile;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Polarization {
    Vertical,
    Horizontal,
}

/// Parameters for one basic-transmission-loss computation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkParams {
    pub freq_mhz: f64,
    /// TX antenna height above ground, meters. For household installs derive
    /// from floor number: h ≈ floors × 3 m + mount offset (review §8.1).
    pub tx_h_agl_m: f64,
    pub rx_h_agl_m: f64,
    /// Time percentage p — loss not exceeded for p% of time (P.1812: 1..=50).
    pub time_pct: f64,
    /// Location percentage pL — loss not exceeded for pL% of locations.
    /// Emergency planning defaults to conservative values, not 50 (review §8.1).
    pub loc_pct: f64,
    pub polarization: Polarization,
    /// ΔN, N-units/km. Comes from pack region metadata — the ITU digital maps
    /// are not redistributable, so the pack compiler bakes per-region scalars.
    pub delta_n: f64,
    /// Sea-level surface refractivity N0, N-units. Same provenance as delta_n.
    pub n0: f64,
    /// Path-centre latitude (deg), for the ducting incidence β0 (P.1812 eq. 5).
    pub path_center_lat_deg: f64,
    /// Prediction resolution w_a (m) for location variability σL (P.1812
    /// eq. 64) — the pixel width the pL% statistic applies to.
    pub location_resolution_m: f64,
    /// Indoor terminal: median building entry loss and its standard deviation
    /// (Rec. P.2040/P.2109 values; P.1812 §4.8). None = outdoor.
    pub entry_loss: Option<EntryLoss>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EntryLoss {
    pub median_db: f64,
    pub sigma_db: f64,
}

impl LinkParams {
    /// EU868 starting point with world-median refractivity; packs override
    /// delta_n/n0 with their region scalars.
    pub fn eu868_defaults() -> Self {
        Self {
            freq_mhz: 869.525,
            tx_h_agl_m: 10.0,
            rx_h_agl_m: 2.0,
            time_pct: 50.0,
            loc_pct: 90.0,
            polarization: Polarization::Vertical,
            delta_n: 45.0,
            n0: 325.0,
            path_center_lat_deg: 52.5,
            location_resolution_m: 100.0,
            entry_loss: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Loss {
    /// Basic transmission loss Lb, dB, at the requested p / pL.
    pub lb_db: f64,
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("profile unusable: {0}")]
    BadProfile(String),
    #[error("parameter outside the model's validity range: {0}")]
    OutOfRange(String),
    #[error("model not implemented yet: {0}")]
    Unimplemented(&'static str),
}

/// A propagation model is a pure function profile → loss. UIs and the
/// optimizer depend only on this trait; which model runs is preset data.
pub trait PathLossModel {
    fn id(&self) -> &'static str;
    fn basic_transmission_loss(
        &self,
        profile: &Profile,
        params: &LinkParams,
    ) -> Result<Loss, ModelError>;
}
