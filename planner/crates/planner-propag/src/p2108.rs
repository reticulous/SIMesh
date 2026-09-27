//! ITU-R P.2108-1 §3.1 — Height Gain Terminal Correction Model.
//!
//! The loss a terminal pays for standing BELOW the clutter around it.
//!
//! WHY THIS IS A SEPARATE MODEL AND NOT PART OF P.1812. It was checked
//! against the Recommendation text in `refs/P1812-8.pdf`: 36 pages, zero
//! occurrences of "P.2108", "height gain" or "terminal surroundings". P.1812
//! builds its profile with the terminals on BARE TERRAIN by construction, in
//! both §3.2.1 (representative clutter heights) and §3.2.2 (surface heights),
//! at any profile spacing. So a node on a fourth-floor balcony below its own
//! roofline gets a free horizon from P.1812 no matter how finely the profile
//! is sampled — that is faithful P.1812, not a resolution artefact, and the
//! missing loss has to come from here.
//!
//! WHAT IT IS WORTH, at the 869.618 MHz this planner runs on:
//!
//! | antenna | roofline | A_h     |
//! |---------|----------|---------|
//! | 15 m    | 20 m     | 14.2 dB |
//! | 15 m    | 22 m     | 17.0 dB |
//! |  2 m    | 20 m     | 24.7 dB |
//! | 18 m    | 20 m     |  7.4 dB |
//! | 20 m    | 20 m     |  0.0 dB |
//!
//! A balcony talking to a street listener is therefore missing about 42 dB
//! across the two ends. That is the right magnitude and the right sign to
//! explain this project's own falsification result: a receive-only node at a
//! known point heard 13 transmitters where the model predicted 72 audible,
//! and predicted margin carried no information about what arrived.
//!
//! Reference implementation and conformance data: NTIA/p2108 (a US Government
//! work). This is written from the Recommendation, not ported, but the test
//! vectors below are the ones that implementation is checked against.

/// An angle that knows it is in degrees.
///
/// The single most damaging way to get this model wrong is to leave
/// `theta_clut` in radians inside the square root — it is the bug NTIA's own
/// tracker records as issue #2. Every language's `atan` returns radians, the
/// formula wants degrees, and the error shrinks ν by √(180/π) ≈ 7.6. At a
/// 15 m antenna under a 22 m roofline that turns 17.0 dB into 3.6 dB: the
/// mistake fails SILENTLY, and it fails toward optimism, which is the
/// direction a coverage planner is already biased in. A newtype makes the
/// unit part of the type rather than part of a comment.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Degrees(pub f64);

impl Degrees {
    #[inline]
    pub fn atan(ratio: f64) -> Self {
        Degrees(ratio.atan().to_degrees())
    }
}

/// What the terminal is standing in. Decides which of §3.1's two branches
/// applies — they are different functions, not a tuning parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalClutter {
    /// Buildings or trees around the terminal: the diffraction branch.
    Obstructed,
    /// Water, open or rural ground: the log branch. Present for completeness
    /// and because misapplying the obstructed branch over a lake would invent
    /// 20 dB of loss that is not there.
    OpenOrWater,
}

/// P.526 single knife-edge diffraction, eq. (31). Shared with P.1812 eq. (12)
/// in form; kept local so this model has no dependency on the P.1812 module
/// and can be validated against P.2108's own vectors alone.
fn j_nu(nu: f64) -> f64 {
    if nu <= -0.78 {
        return 0.0;
    }
    6.9 + 20.0 * (((nu - 0.1).powi(2) + 1.0).sqrt() + nu - 0.1).log10()
}

/// Additional loss (dB) for a terminal at `h_m` above ground with clutter of
/// representative height `r_m` around it.
///
/// `ws_m` is the distance from the terminal to the clutter — P.2108 calls it
/// the street width and offers 27 m as a nominal value. **Pass the measured
/// distance when the caller has real building geometry**: it is the only
/// input that varies with bearing, and therefore the only thing that can make
/// this term produce the directional shadow an operator actually observes. A
/// node below its roofline is blocked in the directions its building extends
/// and open in the others; with the nominal 27 m everywhere, this term is a
/// constant offset over the whole map and cannot express that at all.
///
/// Returns exactly 0.0 when the terminal is at or above the clutter, with no
/// discontinuity approaching it.
///
/// Valid 0.03–3 GHz per §3.1. Outside that range this returns `None` rather
/// than extrapolating: the constants are fitted, not physical.
pub fn height_gain_correction(
    f_ghz: f64,
    h_m: f64,
    r_m: f64,
    ws_m: f64,
    clutter: TerminalClutter,
) -> Option<f64> {
    if !(0.03..=3.0).contains(&f_ghz) || !h_m.is_finite() || !r_m.is_finite() {
        return None;
    }
    // At or above the clutter there is nothing to correct. Not a special
    // case: h_dif goes to zero, ν goes to zero, and the branch below would
    // return J(0) − 6.03 = +0.87 dB, a small positive loss for a terminal
    // that is standing clear. The Recommendation's own condition is h ≥ R.
    if h_m >= r_m {
        return Some(0.0);
    }
    match clutter {
        TerminalClutter::Obstructed => {
            let h_dif = r_m - h_m;
            let ws = if ws_m.is_finite() && ws_m > 0.1 { ws_m } else { 27.0 };
            let theta = Degrees::atan(h_dif / ws);
            let k_nu = 0.342 * f_ghz.sqrt();
            let nu = k_nu * (h_dif * theta.0).sqrt();
            // The −6.03 removes the value J(0) takes at grazing, so the model
            // is continuous with the h ≥ R case above.
            Some((j_nu(nu) - 6.03).max(0.0))
        }
        TerminalClutter::OpenOrWater => {
            // A_h = −K_h2 · log10(h/R), which is negative-signed in the
            // Recommendation and returned here as a positive loss.
            let k_h2 = 21.8 + 6.2 * f_ghz.log10();
            let h = h_m.max(0.1);
            Some((-k_h2 * (h / r_m).log10()).max(0.0))
        }
    }
}

/// The height P.1812 must be run at when this correction is applied.
///
/// P.2108 §3.1 states A_h corrects a basic transmission loss calculated "to or
/// from the height of the representative clutter height". Adding A_h to a
/// P.1812 run at the ANTENNA's height double-counts the geometry: the model
/// has already given the low antenna a worse path, and then the correction
/// charges for the same obstruction again.
///
/// So the composition is: raise the terminal to `r_m`, run P.1812, add A_h.
/// The net penalty is less than A_h alone, because raising the terminal makes
/// the P.1812 term smaller — by how much is a property of the path and must
/// be measured, not assumed.
pub fn model_height_m(h_m: f64, r_m: f64) -> f64 {
    h_m.max(r_m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: f64 = 0.869_618; // the MeshCore channel this planner runs on

    /// The unit bug that silently halves the city's clutter loss.
    ///
    /// `theta_clut` is in DEGREES inside the square root. Every `atan` in
    /// every language returns radians, so this is the mistake anyone writing
    /// this model makes once — NTIA's tracker records it as issue #2. It does
    /// not panic, does not produce a nonsense number, and errs toward LESS
    /// loss, which a coverage tool will happily report as better coverage.
    #[test]
    fn theta_is_in_degrees_and_radians_would_understate_the_loss() {
        let (h, r, ws) = (15.0, 22.0, 27.0);
        let correct = height_gain_correction(F, h, r, ws, TerminalClutter::Obstructed).unwrap();
        assert!((correct - 17.0).abs() < 0.2, "expected ~17.0 dB, got {correct}");

        // The same arithmetic with theta left in radians.
        let h_dif = r - h;
        let nu_rad = 0.342 * F.sqrt() * (h_dif * (h_dif / ws).atan()).sqrt();
        let wrong = j_nu(nu_rad) - 6.03;
        assert!(wrong < 5.0, "the radians bug should collapse the loss, got {wrong}");
        assert!(
            correct - wrong > 10.0,
            "the unit error is worth {} dB and must not be silent",
            correct - wrong
        );
    }

    /// A terminal that clears its clutter pays nothing, and gets there
    /// smoothly.
    ///
    /// A step at h = R would put a cliff in every coverage map along the
    /// contour where antennas happen to match roof height, and an operator
    /// raising a node one metre would see the map lurch.
    #[test]
    fn the_correction_falls_to_zero_at_the_roofline_without_a_step() {
        let r = 20.0;
        let mut last = f64::INFINITY;
        for h in [2.0, 5.0, 10.0, 15.0, 18.0, 19.0, 19.9] {
            let a = height_gain_correction(F, h, r, 27.0, TerminalClutter::Obstructed).unwrap();
            assert!(a < last, "must decrease as the antenna rises: {h} m gave {a}");
            last = a;
        }
        assert!(last < 2.0, "just under the roofline should be small, got {last}");
        for h in [20.0, 20.001, 25.0, 100.0] {
            assert_eq!(
                height_gain_correction(F, h, r, 27.0, TerminalClutter::Obstructed),
                Some(0.0),
                "at or above the clutter the correction is exactly zero"
            );
        }
    }

    /// The street width is the ONLY bearing-dependent input, so it is what
    /// makes this term directional instead of a constant offset.
    ///
    /// This is the whole reason the planner measures it from LoD2 footprints
    /// rather than using the Recommendation's nominal 27 m. With a constant
    /// w_s the correction is identical in every direction from a site, which
    /// cannot reproduce the shadow a node below its own roofline actually
    /// casts — and a term that only shifts every prediction by the same
    /// amount cannot improve a ranking, which is the failure this planner
    /// measured.
    #[test]
    fn a_closer_facade_costs_more_which_is_what_makes_the_term_directional() {
        let (h, r) = (15.0, 22.0);
        let tight = height_gain_correction(F, h, r, 6.0, TerminalClutter::Obstructed).unwrap();
        let nominal = height_gain_correction(F, h, r, 27.0, TerminalClutter::Obstructed).unwrap();
        let open = height_gain_correction(F, h, r, 80.0, TerminalClutter::Obstructed).unwrap();
        assert!(tight > nominal && nominal > open, "{tight} {nominal} {open}");
        assert!(
            tight - open > 6.0,
            "measured geometry must move this by more than rounding: {tight} vs {open}"
        );
    }

    /// Open ground and water take the other branch entirely.
    #[test]
    fn open_ground_uses_the_log_branch_not_the_diffraction_one() {
        let a = height_gain_correction(F, 2.0, 10.0, 27.0, TerminalClutter::OpenOrWater).unwrap();
        let b = height_gain_correction(F, 2.0, 10.0, 27.0, TerminalClutter::Obstructed).unwrap();
        assert!(a > 0.0 && b > 0.0);
        assert!((a - b).abs() > 1.0, "the two branches must not coincide: {a} vs {b}");
        assert_eq!(
            height_gain_correction(F, 12.0, 10.0, 27.0, TerminalClutter::OpenOrWater),
            Some(0.0)
        );
    }

    /// Outside 0.03–3 GHz the constants are not fitted, so refuse.
    #[test]
    fn out_of_band_returns_none_rather_than_extrapolating() {
        assert!(height_gain_correction(0.01, 2.0, 20.0, 27.0, TerminalClutter::Obstructed).is_none());
        assert!(height_gain_correction(5.8, 2.0, 20.0, 27.0, TerminalClutter::Obstructed).is_none());
        assert!(height_gain_correction(0.869, 2.0, 20.0, 27.0, TerminalClutter::Obstructed).is_some());
    }

    /// The composition rule, which is easy to get wrong in the direction of
    /// double-counting.
    #[test]
    fn p1812_is_run_at_the_clutter_height_not_the_antenna_height() {
        assert_eq!(model_height_m(15.0, 22.0), 22.0);
        assert_eq!(model_height_m(30.0, 22.0), 30.0, "a clear antenna is not lowered");
    }
}
