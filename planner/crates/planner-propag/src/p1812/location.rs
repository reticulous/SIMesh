//! §4.7–4.9: location variability, building entry loss, and the final basic
//! transmission loss L_b(p, pL) (eqs. 64–69).

use super::math::inv_ccdf;

/// σ_L — standard deviation of location variability (eq. 64); f in GHz,
/// w_a = prediction resolution in meters.
pub fn sigma_l_db(f_ghz: f64, wa_m: f64) -> f64 {
    (0.024 * f_ghz + 0.52) * wa_m.powf(0.28)
}

/// u(h) — height variation of location variability (eq. 65): full below the
/// representative clutter height R, fading out over the next 10 m.
pub fn u_h(h_m: f64, r_m: f64) -> f64 {
    if h_m < r_m {
        1.0
    } else if h_m < r_m + 10.0 {
        1.0 - (h_m - r_m) / 10.0
    } else {
        0.0
    }
}

/// σ_i — combined indoor standard deviation (eq. 66).
pub fn sigma_indoor_db(sigma_l: f64, sigma_be: f64) -> f64 {
    (sigma_l * sigma_l + sigma_be * sigma_be).sqrt()
}

/// L_b — basic transmission loss not exceeded for p% time and pL% locations
/// (eq. 69). `l_loc`/`sigma_loc` per eqs. (67)/(68): outdoors (0, u(h)·σL),
/// indoors (L_be, σ_i).
pub fn lb_db(lb0p: f64, lbc: f64, l_loc: f64, sigma_loc: f64, pl_pct: f64) -> f64 {
    // Attachment 2: for eq. (69) the argument must be limited to [0.01, 0.99].
    let x = (pl_pct / 100.0).clamp(0.01, 0.99);
    lb0p.max(lbc + l_loc - inv_ccdf(x) * sigma_loc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigma_l_at_868mhz_100m() {
        // (0.024·0.868 + 0.52)·100^0.28 ≈ 0.5408·3.63 ≈ 1.96 dB.
        let s = sigma_l_db(0.868, 100.0);
        assert!((s - 1.96).abs() < 0.03, "{s}");
    }

    #[test]
    fn height_variation_ramp() {
        assert_eq!(u_h(2.0, 15.0), 1.0);
        assert!((u_h(20.0, 15.0) - 0.5).abs() < 1e-12);
        assert_eq!(u_h(25.0, 15.0), 0.0);
        assert_eq!(u_h(30.0, 15.0), 0.0);
    }

    #[test]
    fn median_locations_leave_lbc_untouched() {
        // pL = 50 → I(0.5) ≈ 0 → L_b = max(L_b0p, L_bc).
        let lb = lb_db(100.0, 130.0, 0.0, 2.0, 50.0);
        assert!((lb - 130.0).abs() < 0.01, "{lb}");
    }

    #[test]
    fn harder_location_targets_cost_more_and_floor_at_los() {
        let lb50 = lb_db(100.0, 130.0, 0.0, 5.0, 50.0);
        let lb90 = lb_db(100.0, 130.0, 0.0, 5.0, 90.0);
        let lb10 = lb_db(100.0, 130.0, 0.0, 5.0, 10.0);
        // pL = 90: loss not exceeded at 90% of locations → larger loss value.
        assert!(lb90 > lb50 && lb10 < lb50, "{lb10} {lb50} {lb90}");
        // The free-space/LoS floor (69) can't be undercut.
        let floored = lb_db(129.0, 130.0, 0.0, 5.0, 1.0);
        assert!(floored >= 129.0);
    }

    #[test]
    fn indoor_adds_median_and_spread() {
        let out = lb_db(100.0, 130.0, 0.0, 2.0, 90.0);
        let sigma_i = sigma_indoor_db(2.0, 4.0);
        let ind = lb_db(100.0, 130.0, 12.0, sigma_i, 90.0);
        assert!(ind > out + 11.0, "{ind} vs {out}");
    }
}
