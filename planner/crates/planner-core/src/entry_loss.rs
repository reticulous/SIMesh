//! Building entry loss (ITU-R P.2109-2) and the receiver situation it belongs to.
//!
//! WHY THIS EXISTS. Every coverage figure this tool produced before now was
//! for an OUTDOOR receiver, because `LinkParams::entry_loss` was never set.
//! That assumption was doing a lot of unstated work: residents are indoors,
//! and a household mesh node is indoors, so "3.09 M residents reached" was
//! really "3.09 M residents reached if they stand outside".
//!
//! The size of the correction is not small. At 868 MHz P.2109 puts the median
//! entry loss of a traditional building near 14 dB and of a thermally
//! efficient one near 31 dB — on a link budget of ~145 dB, the second one
//! removes most of a network's usable range.
//!
//! WHAT THIS IS NOT. P.2109 is a statistical model of a building STOCK, not a
//! prediction for one building. It answers "across buildings of this type,
//! what loss is not exceeded for P% of them", so it belongs in a planning
//! tool exactly the way a fade margin does: as a stated assumption with a
//! probability attached, never as a per-building fact.
//!
//! HOW IT REACHES THE PROPAGATION MODEL. P.1812-8 §4.7-4.9 takes an indoor
//! terminal as a median loss plus a standard deviation, and combines the
//! latter with the outdoor location variability (eq. 67b/68b). P.2109's
//! distribution is not Gaussian — it is two lognormal terms plus a floor — so
//! reducing it to (median, sigma) is a deliberate approximation, done here
//! from the 16th/84th percentiles rather than by pretending sigma_1 is the
//! answer. See `EntryLoss::from_p2109`.

use crate::model::EntryLoss;
use serde::{Deserialize, Serialize};

/// Building stock class. P.2109 fits two, and the split matters enormously in
/// Germany, where Altbau and post-EnEV construction sit side by side on the
/// same street.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildingStock {
    /// Masonry/timber, single glazing, no metallised coatings. P.2109 calls
    /// this "traditional".
    Traditional,
    /// Metallised/low-emissivity glazing, foil-backed insulation. P.2109 calls
    /// this "thermally efficient"; it is what modern German new-build and deep
    /// retrofits are.
    ThermallyEfficient,
}

impl BuildingStock {
    /// P.2109-2 Table 1 coefficients (r, s, t, u, v, w, x, y, z).
    fn coeffs(self) -> [f64; 9] {
        match self {
            BuildingStock::Traditional => [12.64, 3.72, 0.96, 9.6, 2.0, 9.1, -3.0, 4.5, -2.0],
            BuildingStock::ThermallyEfficient => {
                [28.19, -3.00, 8.48, 13.5, 3.8, 27.8, -2.9, 9.4, -2.1]
            }
        }
    }
}

/// Standard normal quantile, Phi^-1(p).
///
/// Acklam's rational approximation, refined once by Halley's method; the
/// refinement takes the error to roughly 1e-15, far past anything a dB figure
/// can carry. Written here rather than pulled in as a dependency because
/// planner-core deliberately has none beyond serde.
///
/// NOTE ON THE ITU CONVENTION. P.2109 writes `Q^-1(P)` and calls it the
/// "inverse complementary normal". Taken literally that makes entry loss
/// DECREASE as P rises, which contradicts the text's own definition of
/// L_BEL(P) as the loss "not exceeded for probability P". The standard
/// quantile is used here so that a higher probability means a higher loss;
/// at P = 0.5 the two conventions agree exactly, so the median — the number
/// this tool actually plans with — is unaffected either way.
pub fn norm_ppf(p: f64) -> f64 {
    if !(0.0..=1.0).contains(&p) || p.is_nan() {
        return f64::NAN;
    }
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    const P_LOW: f64 = 0.02425;

    let x = if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - P_LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    // One Halley step against erfc, which the standard library does not give
    // us — erfc is approximated here only to refine an already-good root.
    let e = 0.5 * erfc(-x / std::f64::consts::SQRT_2) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

/// Complementary error function, Numerical Recipes' Chebyshev fit (|eps| < 1.2e-7).
/// Used only inside the quantile refinement above.
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 2.0 / (2.0 + z);
    let ty = 4.0 * t - 2.0;
    const COF: [f64; 28] = [
        -1.3026537197817094,
        6.4196979235649026e-1,
        1.9476473204185836e-2,
        -9.561514786808631e-3,
        -9.46595344482036e-4,
        3.66839497852761e-4,
        4.2523324806907e-5,
        -2.0278578112534e-5,
        -1.624290004647e-6,
        1.303655835580e-6,
        1.5626441722e-8,
        -8.5238095915e-8,
        6.529054439e-9,
        5.059343495e-9,
        -9.91364156e-10,
        -2.27365122e-10,
        9.6467911e-11,
        2.394038e-12,
        -6.886027e-12,
        8.94487e-13,
        3.13092e-13,
        -1.12708e-13,
        3.81e-16,
        7.106e-15,
        -1.523e-15,
        -9.4e-17,
        1.21e-16,
        -2.8e-17,
    ];
    let (mut d, mut dd) = (0.0f64, 0.0f64);
    for j in (1..COF.len()).rev() {
        let tmp = d;
        d = ty * d - dd + COF[j];
        dd = tmp;
    }
    let ans = t * (-z * z + 0.5 * (COF[0] + ty * d) - dd).exp();
    if x >= 0.0 {
        ans
    } else {
        2.0 - ans
    }
}

/// Building entry loss not exceeded for probability `prob`, per P.2109-2.
///
/// `f_ghz` is the carrier, `elev_deg` the elevation angle of the path at the
/// building (0 for a terrestrial link, which is what a mesh is; the term only
/// matters for satellite geometry).
pub fn p2109_bel_db(f_ghz: f64, elev_deg: f64, prob: f64, stock: BuildingStock) -> f64 {
    let [r, s, t, u, v, w, x, y, z] = stock.coeffs();
    let lf = f_ghz.log10();
    // Horizontal-incidence term, plus the elevation correction (eq. 2-3).
    let l_h = r + s * lf + t * lf * lf;
    let l_e = 0.212 * elev_deg.abs();
    let mu1 = l_h + l_e;
    let mu2 = w + x * lf;
    let sigma1 = u + v * lf;
    let sigma2 = y + z * lf;
    let q = norm_ppf(prob);
    let a = q * sigma1 + mu1;
    let b = q * sigma2 + mu2;
    // The -3 dB floor is the external-wall-independent leakage path (eq. 1):
    // no building is perfectly opaque, so the distribution cannot run away.
    const C_FLOOR_DB: f64 = -3.0;
    10.0 * (10f64.powf(0.1 * a) + 10f64.powf(0.1 * b) + 10f64.powf(0.1 * C_FLOOR_DB)).log10()
}

impl EntryLoss {
    /// Reduce P.2109's distribution to the (median, sigma) pair P.1812 wants.
    ///
    /// Sigma comes from the 16th/84th percentile half-spread rather than from
    /// P.2109's own `sigma_1`: the distribution is a sum of two lognormals and
    /// a floor, so `sigma_1` is a parameter of one component, not the spread
    /// of the result. Using it directly would overstate the variability of the
    /// traditional stock and understate nothing usefully.
    pub fn from_p2109(f_ghz: f64, elev_deg: f64, stock: BuildingStock) -> Self {
        let median = p2109_bel_db(f_ghz, elev_deg, 0.5, stock);
        let lo = p2109_bel_db(f_ghz, elev_deg, 0.16, stock);
        let hi = p2109_bel_db(f_ghz, elev_deg, 0.84, stock);
        EntryLoss { median_db: median, sigma_db: ((hi - lo) / 2.0).max(0.0) }
    }
}

/// Where the receiving node actually is.
///
/// This is the knob that decides whether a coverage map answers "can a
/// handheld on the pavement hear this" or "can a node on a shelf in a flat
/// hear this", and those are different maps. Leaving it implicit — as this
/// tool did — silently answers the first while the operator reads the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiverSituation {
    /// Handheld or vehicle at street level, outdoors.
    Street,
    /// On a balcony, a window sill, or an external wall — outdoors for
    /// propagation purposes, but well above the street and clear of the
    /// immediate ground clutter. This is where most household mesh nodes that
    /// work at all actually live.
    Balcony,
    /// Inside a building of traditional construction (German Altbau).
    IndoorTraditional,
    /// Inside a thermally efficient building (post-EnEV new build, deep
    /// retrofit, metallised glazing).
    IndoorThermallyEfficient,
}

impl ReceiverSituation {
    /// Receiver height above ground to plan at (m).
    ///
    /// These are planning conventions, not measurements: 1.5 m is the standard
    /// handheld height used throughout ITU-R work; 8 m is roughly a third-floor
    /// window in a Berlin Altbau (about 3 m per storey, so second floor plus
    /// the ground floor's extra height); the indoor cases share it because the
    /// node is in the same flat as the balcony, just further from the wall.
    pub fn rx_h_agl_m(self) -> f64 {
        match self {
            ReceiverSituation::Street => 1.5,
            ReceiverSituation::Balcony
            | ReceiverSituation::IndoorTraditional
            | ReceiverSituation::IndoorThermallyEfficient => 8.0,
        }
    }

    /// Entry loss to apply, or None for an outdoor terminal.
    pub fn entry_loss(self, f_ghz: f64) -> Option<EntryLoss> {
        match self {
            ReceiverSituation::Street | ReceiverSituation::Balcony => None,
            ReceiverSituation::IndoorTraditional => {
                Some(EntryLoss::from_p2109(f_ghz, 0.0, BuildingStock::Traditional))
            }
            ReceiverSituation::IndoorThermallyEfficient => Some(EntryLoss::from_p2109(
                f_ghz,
                0.0,
                BuildingStock::ThermallyEfficient,
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ReceiverSituation::Street => "street (outdoor, 1.5 m)",
            ReceiverSituation::Balcony => "balcony/window (outdoor, 8 m)",
            ReceiverSituation::IndoorTraditional => "indoor, traditional building",
            ReceiverSituation::IndoorThermallyEfficient => "indoor, thermally efficient",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            ReceiverSituation::Street => "street",
            ReceiverSituation::Balcony => "balcony",
            ReceiverSituation::IndoorTraditional => "indoor-traditional",
            ReceiverSituation::IndoorThermallyEfficient => "indoor-efficient",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "street" => ReceiverSituation::Street,
            "balcony" | "window" => ReceiverSituation::Balcony,
            "indoor-traditional" | "indoor" => ReceiverSituation::IndoorTraditional,
            "indoor-efficient" => ReceiverSituation::IndoorThermallyEfficient,
            _ => return None,
        })
    }

    pub const ALL: [ReceiverSituation; 4] = [
        ReceiverSituation::Street,
        ReceiverSituation::Balcony,
        ReceiverSituation::IndoorTraditional,
        ReceiverSituation::IndoorThermallyEfficient,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_normal_quantile_matches_known_values() {
        // Textbook quantiles; tolerance far tighter than anything a dB needs.
        for (p, want) in [
            (0.5, 0.0),
            (0.84134474606854, 1.0),
            (0.97724986805182, 2.0),
            (0.9, 1.2815515655446),
            (0.1, -1.2815515655446),
            (0.025, -1.9599639845401),
        ] {
            let got = norm_ppf(p);
            assert!(
                (got - want).abs() < 1e-9,
                "norm_ppf({p}) = {got}, want {want}"
            );
        }
        assert!(norm_ppf(-0.1).is_nan());
        assert!(norm_ppf(1.5).is_nan());
    }

    #[test]
    fn entry_loss_at_868_mhz_is_far_larger_for_efficient_buildings() {
        let f = 0.868;
        let trad = p2109_bel_db(f, 0.0, 0.5, BuildingStock::Traditional);
        let eff = p2109_bel_db(f, 0.0, 0.5, BuildingStock::ThermallyEfficient);
        // Published medians near 0.9 GHz: traditional in the low teens,
        // thermally efficient around thirty. If these drift the coefficients
        // have been mistyped.
        assert!((10.0..18.0).contains(&trad), "traditional median {trad:.1} dB");
        assert!((26.0..36.0).contains(&eff), "efficient median {eff:.1} dB");
        assert!(
            eff - trad > 12.0,
            "the stock split must matter: {trad:.1} vs {eff:.1} dB"
        );
    }

    #[test]
    fn entry_loss_rises_with_the_probability_it_is_not_exceeded() {
        // The ITU text defines L_BEL(P) as the loss NOT EXCEEDED for P, so it
        // must increase with P. Taking the "inverse complementary" wording
        // literally inverts this; see the note on norm_ppf.
        let f = 0.868;
        let mut last = f64::NEG_INFINITY;
        for p in [0.05, 0.25, 0.5, 0.75, 0.95] {
            let v = p2109_bel_db(f, 0.0, p, BuildingStock::Traditional);
            assert!(v > last, "not monotonic at P={p}: {v:.2} after {last:.2}");
            last = v;
        }
    }

    #[test]
    fn the_leakage_floor_stops_the_loss_running_away_at_low_probability() {
        // No building is perfectly opaque: even at P -> 0 the model must not
        // predict an arbitrarily transparent wall.
        let v = p2109_bel_db(0.868, 0.0, 1e-6, BuildingStock::ThermallyEfficient);
        assert!(v > -3.5, "floor breached: {v:.2} dB");
        assert!(v < 5.0, "unexpectedly high at P->0: {v:.2} dB");
    }

    #[test]
    fn a_terrestrial_path_pays_no_elevation_term() {
        let a = p2109_bel_db(0.868, 0.0, 0.5, BuildingStock::Traditional);
        let b = p2109_bel_db(0.868, 30.0, 0.5, BuildingStock::Traditional);
        // 0.212 dB per degree, so 30 degrees is ~6 dB — the term is real, it
        // just does not apply to a mesh, which is why callers pass 0.
        assert!((b - a) > 3.0, "elevation term missing: {a:.2} -> {b:.2}");
    }

    #[test]
    fn the_sigma_handed_to_p1812_is_the_spread_of_the_result() {
        let e = EntryLoss::from_p2109(0.868, 0.0, BuildingStock::Traditional);
        let median = p2109_bel_db(0.868, 0.0, 0.5, BuildingStock::Traditional);
        assert!((e.median_db - median).abs() < 1e-12);
        // P.2109's own sigma_1 for traditional at this frequency is ~9.5 dB;
        // the spread of the combined distribution is smaller, which is the
        // whole reason this is derived from percentiles instead.
        assert!(e.sigma_db > 0.0, "sigma must be positive");
        assert!(e.sigma_db < 9.5, "sigma {:.2} should be below sigma_1", e.sigma_db);
    }

    #[test]
    fn outdoor_situations_carry_no_entry_loss_and_indoor_ones_do() {
        assert!(ReceiverSituation::Street.entry_loss(0.868).is_none());
        assert!(ReceiverSituation::Balcony.entry_loss(0.868).is_none());
        let t = ReceiverSituation::IndoorTraditional.entry_loss(0.868).unwrap();
        let e = ReceiverSituation::IndoorThermallyEfficient
            .entry_loss(0.868)
            .unwrap();
        assert!(e.median_db > t.median_db);
        // A street handheld and a node in a flat are different heights too;
        // conflating them is what made the earlier coverage maps ambiguous.
        assert!(
            ReceiverSituation::Street.rx_h_agl_m() < ReceiverSituation::Balcony.rx_h_agl_m()
        );
    }

    #[test]
    fn every_situation_round_trips_through_its_id() {
        for s in ReceiverSituation::ALL {
            assert_eq!(ReceiverSituation::from_id(s.id()), Some(s), "{}", s.id());
        }
        assert_eq!(ReceiverSituation::from_id("nonsense"), None);
    }
}
