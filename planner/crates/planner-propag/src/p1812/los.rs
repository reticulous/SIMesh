//! §4.2: Line-of-sight propagation including short-term effects (eqs. 8–11).
//! Evaluated for every path, LoS or not (§4.2 opening sentence).

/// Free-space basic transmission loss L_bfs (eq. 8), with the slope distance
/// d_fs of eq. (8a). Uses the Rec's own 92.4 constant (not the higher-precision
/// 92.45) so oracle comparisons match to the millidecibel.
pub fn lbfs_db(f_ghz: f64, d_km: f64, hts_masl: f64, hrs_masl: f64) -> f64 {
    let dfs = (d_km.powi(2) + ((hts_masl - hrs_masl) / 1000.0).powi(2)).sqrt(); // (8a)
    92.4 + 20.0 * f_ghz.log10() + 20.0 * dfs.log10() // (8)
}

/// Multipath/focusing correction E_sp at time percentage p (eq. 9a).
pub fn esp_db(dlt_km: f64, dlr_km: f64, p_pct: f64) -> f64 {
    2.6 * (1.0 - (-(dlt_km + dlr_km) / 10.0).exp()) * (p_pct / 50.0).log10()
}

/// L_b0p — LoS loss not exceeded for p% time (eq. 10).
pub fn lb0p_db(f_ghz: f64, d_km: f64, hts: f64, hrs: f64, dlt: f64, dlr: f64, p: f64) -> f64 {
    lbfs_db(f_ghz, d_km, hts, hrs) + esp_db(dlt, dlr, p)
}

/// L_b0β — LoS loss not exceeded for β0% time (eq. 11; eq. 9b is eq. 9a at β0).
pub fn lb0beta_db(f_ghz: f64, d_km: f64, hts: f64, hrs: f64, dlt: f64, dlr: f64, beta0: f64) -> f64 {
    lbfs_db(f_ghz, d_km, hts, hrs) + esp_db(dlt, dlr, beta0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lbfs_868mhz_1km_level() {
        // 92.4 + 20 log10(0.868) + 20 log10(1) = 91.170 dB (Rec constant).
        let l = lbfs_db(0.868, 1.0, 30.0, 30.0);
        assert!((l - 91.170).abs() < 0.005, "{l}");
    }

    #[test]
    fn esp_zero_at_median_negative_below() {
        assert_eq!(esp_db(5.0, 5.0, 50.0), 0.0);
        // p < 50 → negative correction (signal exceeded more easily).
        assert!(esp_db(5.0, 5.0, 1.0) < 0.0);
    }

    #[test]
    fn slope_distance_counts_height_delta() {
        // 1 km path with 1000 m height difference → d_fs = √2 km.
        let l = lbfs_db(1.0, 1.0, 1030.0, 30.0);
        let expect = 92.4 + 20.0 * (2f64.sqrt()).log10();
        assert!((l - expect).abs() < 1e-9);
    }
}
