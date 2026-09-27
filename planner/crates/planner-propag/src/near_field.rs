//! Loss inside P.1812's lower validity bound — the first 250 m.
//!
//! P.1812 refuses any path shorter than 0.25 km (§1, enforced in
//! `p1812::lb_from_arrays`). Until now the sweep took that refusal as an
//! answer: cells inside the bound stayed NaN, the map drew a hole around
//! every transmitter, and the gap census counted the hole as SERVED on the
//! reasoning that a receiver that close to a repeater is the best-served
//! receiver on the map. That reasoning is right in open country and wrong in
//! a city, where the first 250 m contains the courtyard wall, the block
//! opposite, and the transmitter's own Dachgeschoss — which is exactly the
//! geometry that decides whether a mesh node hears its neighbour.
//!
//! WHY NOT P.1411. ITU-R P.1411 is the recommendation for this range and it
//! would be the obvious answer. It is parametric: street width, building
//! separation, road orientation, a LoS/NLoS classification. Those parameters
//! exist to STAND IN for geometry the modeller does not have. This pack has
//! 1 026 069 real footprint polygons with real roof heights, so using a model
//! that reduces them to an average street width would be throwing away the
//! measurement to obtain a parameter fitted to somebody else's city.
//!
//! WHAT THIS DOES INSTEAD. Free-space loss, plus knife-edge diffraction over
//! the obstacle that actually blocks the ray, taken from the profile the
//! caller already built from those polygons. Two ITU pieces, both already
//! trusted here: P.525 free space, and P.526 eq. (31) — the same `J(ν)` that
//! P.1812 eq. (12) uses and that this crate's oracle validates.
//!
//! It is deliberately a FLOOR-RESPECTING model: the result is never less than
//! free space, because nothing propagates better than an unobstructed path,
//! and a near-field model that returned less would silently manufacture
//! coverage in the one region where an operator is most likely to check the
//! answer against reality by walking outside.

/// P.526 single knife edge, eq. (31). Local copy so this module stands alone.
fn j_nu(nu: f64) -> f64 {
    if nu <= -0.78 {
        return 0.0;
    }
    6.9 + 20.0 * (((nu - 0.1).powi(2) + 1.0).sqrt() + nu - 0.1).log10()
}

/// Free-space basic transmission loss (ITU-R P.525), dB.
pub fn free_space_db(f_mhz: f64, d_km: f64) -> f64 {
    32.44 + 20.0 * f_mhz.log10() + 20.0 * d_km.max(1e-9).log10()
}

/// What the caller must supply: the same profile arrays the sweep already
/// builds, sliced to this path.
pub struct NearFieldPath<'a> {
    /// Distance from the transmitter, km, ascending, starting at 0.
    pub d_km: &'a [f64],
    /// Bare terrain, m above sea level.
    pub h_masl: &'a [f64],
    /// Terrain plus whatever stands on it, m above sea level.
    pub g_masl: &'a [f64],
    pub f_mhz: f64,
    pub tx_h_agl_m: f64,
    pub rx_h_agl_m: f64,
}

/// Basic transmission loss for a path shorter than P.1812's floor.
///
/// Returns `None` for a degenerate path (fewer than three points, or zero
/// length) rather than a number: at zero range the free-space term diverges
/// and any answer would be invented.
pub fn loss_db(p: &NearFieldPath<'_>) -> Option<f64> {
    let n = p.d_km.len();
    if n < 3 || p.h_masl.len() != n || p.g_masl.len() != n {
        return None;
    }
    let d_total = p.d_km[n - 1] - p.d_km[0];
    if !(d_total > 0.0) {
        return None;
    }
    let lfs = free_space_db(p.f_mhz, d_total);

    // The ray, as a straight line between the two antennas. Earth curvature
    // is ignored on purpose: the bulge over 250 m is d²/(2·a_e) ≈ 3.7 mm,
    // four orders of magnitude below the height data's own resolution, and
    // carrying it here would imply a precision the pack does not have.
    let tx = p.h_masl[0] + p.tx_h_agl_m;
    let rx = p.h_masl[n - 1] + p.rx_h_agl_m;
    let lambda = 299.792_458 / p.f_mhz; // metres

    // Deygout's principal edge: the single obstacle with the largest ν, not
    // a sum over every rooftop. Summing edges over-counts badly in a city —
    // a flat-roofed block presents its near AND far facade to the ray, and
    // charging for both invents a second obstruction that is not there.
    let mut worst = f64::NEG_INFINITY;
    for i in 1..n - 1 {
        let d1 = (p.d_km[i] - p.d_km[0]) * 1000.0;
        let d2 = (p.d_km[n - 1] - p.d_km[i]) * 1000.0;
        if d1 <= 0.0 || d2 <= 0.0 {
            continue;
        }
        // Height of the obstacle above the direct ray.
        let ray = tx + (rx - tx) * (d1 / (d1 + d2));
        let h = p.g_masl[i] - ray;
        // Exact form, not the paraxial one: over 250 m an obstacle can
        // subtend tens of degrees from a terminal, where h/d is nowhere near
        // small and the small-angle version overstates ν.
        let delta = (d1 * d1 + h * h).sqrt() + (d2 * d2 + h * h).sqrt() - (d1 + d2);
        let nu = h.signum() * (4.0 * delta.abs() / lambda).sqrt();
        if nu > worst {
            worst = nu;
        }
    }

    let diff = if worst.is_finite() { j_nu(worst) } else { 0.0 };
    // Never better than free space. `j_nu` is already 0 for a clear path, so
    // this is a guard against a pathological profile rather than a clamp on
    // the physics.
    Some(lfs + diff.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: f64 = 869.618;

    fn flat(n: usize, len_m: f64, ground: f64) -> (Vec<f64>, Vec<f64>) {
        let d = (0..n).map(|i| i as f64 * len_m / (n - 1) as f64 / 1000.0).collect();
        (d, vec![ground; n])
    }

    /// An unobstructed short path must be free space and nothing else.
    ///
    /// If it were not, every near-field cell would carry a fabricated excess
    /// in exactly the region an operator can check by walking outside — the
    /// fastest possible way to lose their trust in the whole map.
    #[test]
    fn a_clear_path_is_exactly_free_space() {
        let (d, h) = flat(21, 200.0, 35.0);
        let p = NearFieldPath {
            d_km: &d,
            h_masl: &h,
            g_masl: &h,
            f_mhz: F,
            tx_h_agl_m: 15.0,
            rx_h_agl_m: 2.0,
        };
        let got = loss_db(&p).unwrap();
        let want = free_space_db(F, 0.2);
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
    }

    /// A building across the street costs real decibels, and more of them the
    /// taller it is. This is the whole reason the region is modelled at all.
    #[test]
    fn a_block_in_the_way_costs_more_the_taller_it_is() {
        let (d, h) = flat(41, 200.0, 35.0);
        let mut last = free_space_db(F, 0.2);
        for roof in [1.0, 5.0, 12.0, 22.0, 35.0] {
            let mut g = h.clone();
            // A block occupying the middle 30 m of the path.
            for i in 0..g.len() {
                let x = d[i] * 1000.0;
                if (85.0..115.0).contains(&x) {
                    g[i] = 35.0 + roof;
                }
            }
            let p = NearFieldPath {
                d_km: &d,
                h_masl: &h,
                g_masl: &g,
                f_mhz: F,
                tx_h_agl_m: 15.0,
                rx_h_agl_m: 2.0,
            };
            let got = loss_db(&p).unwrap();
            assert!(got >= last - 1e-9, "taller must not be cheaper: {roof} m gave {got}");
            last = got;
        }
        // A 35 m block against a 15 m antenna and a 2 m listener is a deep
        // obstruction; if this is not worth double figures the geometry is
        // not reaching the model.
        assert!(last - free_space_db(F, 0.2) > 10.0, "excess was only {}", last - free_space_db(F, 0.2));
    }

    /// Never below free space, whatever the profile says.
    #[test]
    fn the_answer_never_beats_an_unobstructed_path() {
        let (d, h) = flat(31, 150.0, 40.0);
        // A trench: ground falling away under the ray.
        let g: Vec<f64> = h.iter().enumerate().map(|(i, v)| v - i as f64).collect();
        let p = NearFieldPath {
            d_km: &d,
            h_masl: &h,
            g_masl: &g,
            f_mhz: F,
            tx_h_agl_m: 10.0,
            rx_h_agl_m: 2.0,
        };
        assert!(loss_db(&p).unwrap() >= free_space_db(F, 0.15) - 1e-9);
    }

    /// A degenerate path returns None rather than a number.
    #[test]
    fn a_zero_length_path_is_refused_not_answered() {
        let d = vec![0.0, 0.0, 0.0];
        let h = vec![30.0; 3];
        let p = NearFieldPath {
            d_km: &d,
            h_masl: &h,
            g_masl: &h,
            f_mhz: F,
            tx_h_agl_m: 5.0,
            rx_h_agl_m: 2.0,
        };
        assert!(loss_db(&p).is_none());
    }

    /// Only ONE edge is charged. A flat-roofed block presents its near and
    /// far facade to the ray; summing them invents an obstruction.
    #[test]
    fn a_flat_roof_is_one_obstacle_not_two() {
        let (d, h) = flat(81, 240.0, 30.0);
        let mut one = h.clone();
        let mut two = h.clone();
        for i in 0..d.len() {
            let x = d[i] * 1000.0;
            if (100.0..104.0).contains(&x) {
                one[i] = 52.0;
                two[i] = 52.0;
            }
            // The far facade of the same building.
            if (136.0..140.0).contains(&x) {
                two[i] = 52.0;
            }
        }
        let mk = |g: &Vec<f64>| {
            loss_db(&NearFieldPath {
                d_km: &d,
                h_masl: &h,
                g_masl: g,
                f_mhz: F,
                tx_h_agl_m: 12.0,
                rx_h_agl_m: 2.0,
            })
            .unwrap()
        };
        let (a, b) = (mk(&one), mk(&two));
        assert!(
            (a - b).abs() < 3.0,
            "the second facade of one building must not double the loss: {a} vs {b}"
        );
    }
}
