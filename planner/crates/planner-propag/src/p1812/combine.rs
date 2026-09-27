//! §4.6: Blending the mechanisms into the basic transmission loss not
//! exceeded for p% time and 50% locations, L_bc (eqs. 57–63).

/// Everything §4.6 consumes; all values in dB / km / mrad / %.
pub struct CombineInputs {
    pub d_km: f64,
    /// Path angular distance θ (eq. 82).
    pub theta_mrad: f64,
    pub omega: f64,
    /// Notional LoS losses (eqs. 10, 11).
    pub lb0p_db: f64,
    pub lb0beta_db: f64,
    /// Diffraction results: L_dp and L_d50 (eqs. 41, and 42's input),
    /// free-space L_bfs (eq. 8), interpolation factor F_i (eq. 40).
    pub ldp_db: f64,
    pub ld50_db: f64,
    pub lbfs_db: f64,
    pub fi: f64,
    /// Troposcatter (eq. 44) and ducting (eq. 46).
    pub lbs_db: f64,
    pub lba_db: f64,
    pub p_pct: f64,
    pub beta0_pct: f64,
}

/// L_bc — basic transmission loss for p% time and 50% locations (eq. 63).
pub fn lbc_db(x: &CombineInputs) -> f64 {
    // (57) angular-distance blend.
    let (theta_big, xi) = (0.3, 0.8);
    let fj = 1.0
        - 0.5 * (1.0 + (3.0 * xi * (x.theta_mrad - theta_big) / theta_big).tanh());
    // (58) great-circle-distance blend.
    let (dsw, kappa) = (20.0, 0.5);
    let fk = 1.0 - 0.5 * (1.0 + (3.0 * kappa * (x.d_km - dsw) / dsw).tanh());

    // (42), (43).
    let lbd50 = x.lbfs_db + x.ld50_db;
    let lbd = x.lb0p_db + x.ldp_db;

    // (59) notional minimum with over-sea sub-path diffraction.
    let lminb0p = if x.p_pct < x.beta0_pct {
        x.lb0p_db + (1.0 - x.omega) * x.ldp_db
    } else {
        lbd50 + (x.lb0beta_db + (1.0 - x.omega) * x.ldp_db - lbd50) * x.fi
    };

    // (60) notional minimum with ducting enhancements, η = 2.5.
    let eta = 2.5;
    let lminbap = eta * ((x.lba_db / eta).exp() + (x.lb0p_db / eta).exp()).ln();

    // (61).
    let lbda = if lminbap > lbd {
        lbd
    } else {
        lminbap + (lbd - lminbap) * fk
    };

    // (62).
    let lbam = lbda + (lminb0p - lbda) * fj;

    // (63) power-sum with troposcatter.
    -5.0 * (10f64.powf(-0.2 * x.lbs_db) + 10f64.powf(-0.2 * lbam)).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CombineInputs {
        CombineInputs {
            d_km: 10.0,
            theta_mrad: 5.0,
            omega: 0.0,
            lb0p_db: 111.0,
            lb0beta_db: 110.0,
            ldp_db: 25.0,
            ld50_db: 27.0,
            lbfs_db: 111.0,
            fi: 1.0,
            lbs_db: 210.0,
            lba_db: 190.0,
            p_pct: 50.0,
            beta0_pct: 1.1,
        }
    }

    #[test]
    fn lbc_never_exceeds_weaker_mechanism_much() {
        // The power sum (63) is bounded by the smaller loss and can undercut
        // it by at most 5·log10(2) ≈ 1.5 dB.
        let x = base();
        let lbc = lbc_db(&x);
        assert!(lbc <= 210.0 + 1e-9);
        // With tropo 70+ dB above, lbc ≈ lbam.
        let lbam_only = {
            let mut y = base();
            y.lbs_db = 400.0;
            lbc_db(&y)
        };
        assert!((lbc - lbam_only).abs() < 0.01);
    }

    #[test]
    fn strong_tropo_path_takes_over() {
        // If troposcatter is the weaker loss, it dominates the combination.
        let mut x = base();
        x.lbs_db = 120.0;
        x.lba_db = 300.0;
        let lbc = lbc_db(&x);
        assert!((lbc - 120.0).abs() < 2.0, "{lbc}");
    }

    #[test]
    fn short_low_angle_path_reduces_to_los_plus_diffraction() {
        // θ ≪ Θ and d ≪ d_sw → F_j ≈ F_k ≈ 1 → L_bam ≈ L_minb0p; with
        // p = 50 → F_i-driven (59). For fi = 1: lminb0p = lb0β + ldp.
        let mut x = base();
        x.theta_mrad = 0.01;
        x.d_km = 1.0;
        x.lbs_db = 400.0; // park tropo
        let lbc = lbc_db(&x);
        let expect = x.lb0beta_db + x.ldp_db; // 135
        assert!((lbc - expect).abs() < 0.5, "{lbc} vs {expect}");
    }
}
