//! Fitted calibration profiles: measured-minus-predicted offsets, shipped as
//! DATA next to a pack rather than baked into the model (review §8.2).
//!
//! P.1812's *implementation* here is oracle-validated against Py1812
//! (108/108 vectors within 0.1 dB). That says nothing about whether its
//! *predictions* match a real 868 MHz mesh over Berlin rooftops, which is a
//! separate, empirical question. This module is the place the answer gets
//! recorded — versioned, named, and with its assumptions written down, so a
//! number can always be traced back to the measurements and the guesses that
//! produced it.

use crate::geo::Xy;
use crate::profile::ClutterClass;
use serde::{Deserialize, Serialize};

/// One measured link, reduced to what a fit actually needs.
///
/// Deliberately not `planner_import::Observation`: the importers carry
/// source-specific fields and optionality that the fit has no business
/// knowing about, and keeping this type in `planner-core` lets the fit be
/// unit-tested without dragging in the import crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkSample {
    /// Transmitter position in the pack's CRS.
    pub tx: Xy,
    /// Receiver position in the pack's CRS.
    pub rx: Xy,
    /// Basic transmission loss implied by the measurement (dB). How it was
    /// derived from RSSI/SNR — and what had to be assumed about transmit
    /// power and antenna gain — belongs in [`FitAssumptions`].
    pub measured_loss_db: f64,
    /// Antenna heights above ground actually used, when known (m).
    pub tx_h_agl_m: Option<f64>,
    pub rx_h_agl_m: Option<f64>,
}

/// The guesses a fit had to make, recorded so a profile is never mistaken
/// for a pure measurement.
///
/// Community mesh data almost never reports transmit power or antenna gain,
/// so a loss figure derived from RSSI rests on assumed values. If those
/// assumptions are wrong by x dB, every offset below is wrong by x dB in the
/// same direction — which is exactly why they are stored, not discarded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitAssumptions {
    pub tx_power_dbm: f64,
    pub antenna_gain_dbi: f64,
    /// Set when SNR had to be converted to RSSI through an assumed noise
    /// floor (dBm), rather than RSSI being reported directly.
    pub assumed_noise_floor_dbm: Option<f64>,
    /// Free-text notes (source, filtering, anything a reader must know).
    pub notes: String,
}

/// Fitted offset for one clutter class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassOffset {
    pub class: ClutterClass,
    /// Add this to a P.1812 prediction to match observation (dB). Positive
    /// means the model under-predicts loss for this class.
    pub offset_db: f64,
    /// Samples behind this offset. Small n is the normal case early on, and
    /// the reason [`CalibrationProfile::offset_for`] refuses to apply one.
    pub n: usize,
    /// Median absolute deviation of the residuals (dB) — spread, not error
    /// of the mean. A large MAD says the class is not being explained by a
    /// constant offset.
    pub mad_db: f64,
}

/// Below this many samples a class offset is noise, and is not applied.
pub const MIN_SAMPLES_PER_CLASS: usize = 8;

/// A named, versioned set of per-clutter-class offsets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationProfile {
    /// e.g. "berlin-2026Q4" — matches `PackManifest.calibration[].name`.
    pub name: String,
    /// RFC3339 UTC, so a profile can be ordered against a pack build.
    pub created_utc: String,
    /// Frequency the measurements were taken at (MHz). Offsets are not
    /// transferable across bands.
    pub freq_mhz: f64,
    pub assumptions: FitAssumptions,
    pub n_observations: usize,
    pub per_class: Vec<ClassOffset>,
    /// Median |residual| over all samples before and after applying the
    /// fitted offsets (dB). If `after` is not clearly below `before`, the
    /// fit did not find structure and should not be shipped.
    pub median_abs_residual_before_db: f64,
    pub median_abs_residual_after_db: f64,
}

impl CalibrationProfile {
    /// Offset to add to a prediction for `class`, or 0.0 when this profile
    /// has no usable estimate for it.
    ///
    /// Returning 0.0 rather than an interpolated guess is deliberate: an
    /// unmeasured class should fall back to the uncalibrated model, which is
    /// at least a known quantity, instead of borrowing a neighbouring
    /// class's bias.
    pub fn offset_for(&self, class: ClutterClass) -> f64 {
        self.per_class
            .iter()
            .find(|c| c.class == class && c.n >= MIN_SAMPLES_PER_CLASS)
            .map(|c| c.offset_db)
            .unwrap_or(0.0)
    }

    /// True when the fit measurably reduced residual error. A profile that
    /// fails this is still worth keeping as a record — it is evidence that a
    /// constant per-class offset is the wrong correction — but it must not
    /// be applied to predictions.
    pub fn is_useful(&self) -> bool {
        self.n_observations > 0
            && self.median_abs_residual_after_db < self.median_abs_residual_before_db
            && self.per_class.iter().any(|c| c.n >= MIN_SAMPLES_PER_CLASS)
    }
}

/// Median of a slice, by value. Returns 0.0 for an empty slice.
pub fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Median absolute deviation about `center`.
pub fn mad(values: &[f64], center: f64) -> f64 {
    let mut d: Vec<f64> = values.iter().map(|x| (x - center).abs()).collect();
    median(&mut d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(per_class: Vec<ClassOffset>, before: f64, after: f64) -> CalibrationProfile {
        CalibrationProfile {
            name: "t".into(),
            created_utc: "2026-08-31T00:00:00Z".into(),
            freq_mhz: 868.0,
            assumptions: FitAssumptions {
                tx_power_dbm: 22.0,
                antenna_gain_dbi: 2.0,
                assumed_noise_floor_dbm: None,
                notes: String::new(),
            },
            n_observations: per_class.iter().map(|c| c.n).sum(),
            per_class,
            median_abs_residual_before_db: before,
            median_abs_residual_after_db: after,
        }
    }

    #[test]
    fn thin_classes_are_not_applied() {
        let p = profile(
            vec![
                ClassOffset { class: ClutterClass::Urban, offset_db: 7.0, n: 40, mad_db: 2.0 },
                // One lucky sample must not move anybody's prediction.
                ClassOffset { class: ClutterClass::Forest, offset_db: -30.0, n: 1, mad_db: 0.0 },
            ],
            9.0,
            3.0,
        );
        assert_eq!(p.offset_for(ClutterClass::Urban), 7.0);
        assert_eq!(p.offset_for(ClutterClass::Forest), 0.0, "n=1 must not be applied");
        // A class that was never measured falls back to the raw model.
        assert_eq!(p.offset_for(ClutterClass::Water), 0.0);
        assert!(p.is_useful());
    }

    #[test]
    fn a_fit_that_does_not_help_is_not_useful() {
        let good = ClassOffset { class: ClutterClass::Urban, offset_db: 1.0, n: 50, mad_db: 9.0 };
        assert!(!profile(vec![good.clone()], 5.0, 5.0).is_useful(), "no improvement");
        assert!(!profile(vec![good.clone()], 5.0, 6.0).is_useful(), "made it worse");
        assert!(profile(vec![good], 5.0, 4.0).is_useful());
        // Nothing measured at all is never useful.
        assert!(!profile(vec![], 5.0, 1.0).is_useful());
    }

    #[test]
    fn median_and_mad_behave() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&mut []), 0.0);
        // MAD is robust: one wild outlier must not move it.
        assert_eq!(mad(&[10.0, 11.0, 12.0, 1000.0], 11.0), 1.0);
    }

    #[test]
    fn profile_round_trips_through_json() {
        let p = profile(
            vec![ClassOffset { class: ClutterClass::DenseUrban, offset_db: -2.5, n: 12, mad_db: 4.0 }],
            8.0,
            5.0,
        );
        let s = serde_json::to_string_pretty(&p).unwrap();
        let back: CalibrationProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
