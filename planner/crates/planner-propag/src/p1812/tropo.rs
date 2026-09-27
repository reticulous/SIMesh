//! §4.4: Propagation by tropospheric scatter (eqs. 44–45). Valid for p ≤ 50%.

/// L_bs — basic transmission loss due to troposcatter not exceeded for p%
/// time (eq. 44). θ is the path angular distance in mrad (eq. 82); N0 the
/// path-centre sea-level surface refractivity.
pub fn lbs_db(f_ghz: f64, d_km: f64, theta_mrad: f64, n0: f64, p_pct: f64) -> f64 {
    let lf = 25.0 * f_ghz.log10() - 2.5 * (f_ghz / 2.0).log10().powi(2); // (45)
    190.1 + lf + 20.0 * d_km.log10() + 0.573 * theta_mrad - 0.15 * n0
        - 10.125 * (50.0 / p_pct).log10().powf(0.7) // (44)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tropo_is_a_weak_mechanism() {
        // 50 km, 868 MHz, modest angular distance: loss far above free space.
        let l = lbs_db(0.868, 50.0, 20.0, 325.0, 50.0);
        assert!(l > 150.0, "{l}");
    }

    #[test]
    fn smaller_time_percentage_means_less_loss() {
        let l50 = lbs_db(0.868, 50.0, 20.0, 325.0, 50.0);
        let l1 = lbs_db(0.868, 50.0, 20.0, 325.0, 1.0);
        assert!(l1 < l50, "{l1} vs {l50}");
    }
}
