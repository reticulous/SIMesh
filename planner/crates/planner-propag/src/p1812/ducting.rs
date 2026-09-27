//! §4.5: Propagation by ducting / layer reflection (eqs. 46–56a).

pub struct DuctingInputs {
    pub f_ghz: f64,
    pub d_km: f64,
    pub dlt_km: f64,
    pub dlr_km: f64,
    /// Horizon elevation angles from the path analysis (mrad).
    pub theta_t_mrad: f64,
    pub theta_r_mrad: f64,
    /// Antenna centre heights amsl (for the over-sea coupling terms, eq. 49).
    pub hts_masl: f64,
    pub hrs_masl: f64,
    /// Ducting-model effective antenna heights and terrain roughness
    /// (eqs. 92a/92b/93).
    pub hte_m: f64,
    pub hre_m: f64,
    pub hm_m: f64,
    pub ae_km: f64,
    pub beta0_pct: f64,
    /// Longest continuous inland section (km) — re-derives τ (eq. 3).
    pub d_lm_km: f64,
    /// Sea fraction ω and terminal-to-coast distances (Table 5, §3.4).
    pub omega: f64,
    pub dct_km: f64,
    pub dcr_km: f64,
    pub p_pct: f64,
}

/// L_ba — ducting/layer-reflection basic transmission loss not exceeded for
/// p% time (eq. 46).
pub fn lba_db(x: &DuctingInputs) -> f64 {
    let f = x.f_ghz;

    // (47a) empirical low-frequency correction.
    let alf = if f < 0.5 {
        45.375 - 137.0 * f + 92.5 * f * f
    } else {
        0.0
    };

    // (48/48a) site-shielding diffraction losses.
    let a_s = |theta_mrad: f64, dl_km: f64| -> f64 {
        let theta2 = theta_mrad - 0.1 * dl_km; // (48a)
        if theta2 > 0.0 {
            20.0 * (1.0 + 0.361 * theta2 * (f * dl_km).sqrt()).log10()
                + 0.264 * theta2 * f.powf(1.0 / 3.0)
        } else {
            0.0
        }
    };
    let ast = a_s(x.theta_t_mrad, x.dlt_km);
    let asr = a_s(x.theta_r_mrad, x.dlr_km);

    // (49) over-sea surface-duct coupling; needed only for mostly-sea paths.
    let a_c = |dc_km: f64, dl_km: f64, h_masl: f64| -> f64 {
        if x.omega >= 0.75 && dc_km <= dl_km && dc_km <= 5.0 {
            -3.0 * (-0.25 * dc_km * dc_km).exp() * (1.0 + (0.07 * (50.0 - h_masl)).tanh())
        } else {
            0.0
        }
    };
    let act = a_c(x.dct_km, x.dlt_km, x.hts_masl);
    let acr = a_c(x.dcr_km, x.dlr_km, x.hrs_masl);

    // (47) total fixed coupling loss.
    let af = 102.45 + 20.0 * f.log10() + 20.0 * (x.dlt_km + x.dlr_km).log10()
        + alf + ast + asr + act + acr;

    // (51) specific attenuation.
    let gamma_d = 5e-5 * x.ae_km * f.powf(1.0 / 3.0);

    // (52/52a) corrected angular distance.
    let theta_t1 = x.theta_t_mrad.min(0.1 * x.dlt_km);
    let theta_r1 = x.theta_r_mrad.min(0.1 * x.dlr_km);
    let theta1 = 1000.0 * x.d_km / x.ae_km + theta_t1 + theta_r1;

    // (3), (55a), (55) geometric correction μ2.
    let tau = 1.0 - (-0.000412 * x.d_lm_km.powf(2.41)).exp();
    let eps = 3.5;
    let alpha = (-0.6 - tau * x.d_km.powf(3.1) * eps * 1e-9).max(-3.4);
    let mu2 = ((500.0 / x.ae_km) * x.d_km * x.d_km
        / (x.hte_m.sqrt() + x.hre_m.sqrt()).powi(2))
    .powf(alpha)
    .min(1.0);

    // (56/56a) terrain-roughness correction μ3.
    let d_i = (x.d_km - x.dlt_km - x.dlr_km).min(40.0);
    let mu3 = if x.hm_m <= 10.0 {
        1.0
    } else {
        (-4.6e-5 * (x.hm_m - 10.0) * (43.0 + 6.0 * d_i)).exp()
    };

    // (54) adjusted time percentage.
    let beta = x.beta0_pct * mu2 * mu3;

    // (53a) Γ and (53) A(p).
    let log_beta = beta.log10();
    let gamma = 1.076 / (2.0058 - log_beta).powf(1.012)
        * (-(9.51 - 4.8 * log_beta + 0.198 * log_beta * log_beta) * 1e-6
            * x.d_km.powf(1.13))
        .exp();
    let a_p = -12.0
        + (1.2 + 3.7e-3 * x.d_km) * (x.p_pct / beta).log10()
        + 12.0 * (x.p_pct / beta).powf(gamma); // (53)

    af + gamma_d * theta1 + a_p // (46) with (50)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> DuctingInputs {
        DuctingInputs {
            f_ghz: 0.868,
            d_km: 60.0,
            dlt_km: 10.0,
            dlr_km: 12.0,
            theta_t_mrad: 3.0,
            theta_r_mrad: 2.0,
            hts_masl: 40.0,
            hrs_masl: 35.0,
            hte_m: 30.0,
            hre_m: 25.0,
            hm_m: 15.0,
            ae_km: 8930.6,
            beta0_pct: 1.1,
            d_lm_km: 60.0,
            omega: 0.0,
            dct_km: 60.0,
            dcr_km: 60.0,
            p_pct: 50.0,
        }
    }

    #[test]
    fn ducting_loss_is_substantial_over_land() {
        let l = lba_db(&base());
        assert!(l > 120.0, "{l}");
    }

    #[test]
    fn small_time_percentages_lose_less() {
        // Ducting is the anomalous mechanism: it strengthens (loss drops) at
        // small p.
        let mut x = base();
        x.p_pct = 1.0;
        let l1 = lba_db(&x);
        let l50 = lba_db(&base());
        assert!(l1 < l50, "{l1} vs {l50}");
    }

    #[test]
    fn site_shielding_kicks_in_above_the_floor() {
        // With d_lt = 10, both the (48a) shielding floor and the (52a) θ′ cap
        // sit at 0.1·d_lt = 1.0 mrad. Raising θ_t from 1.0 → 2.0 leaves θ′
        // unchanged (capped), so the entire difference is the A_st shielding
        // term — about 6.5 dB at 868 MHz.
        let at_floor = lba_db(&DuctingInputs { theta_t_mrad: 1.0, ..base() });
        let above = lba_db(&DuctingInputs { theta_t_mrad: 2.0, ..base() });
        assert!(above - at_floor > 3.0, "{above} vs {at_floor}");
        // Below the floor, only the γ_d·θ′ term moves — a fraction of a dB
        // per 0.1 mrad, not the shielding jump.
        let below = lba_db(&DuctingInputs { theta_t_mrad: 0.9, ..base() });
        assert!((at_floor - below).abs() < 0.2, "{at_floor} vs {below}");
    }

    #[test]
    fn rough_terrain_raises_loss() {
        let smooth = lba_db(&DuctingInputs { hm_m: 5.0, ..base() });
        let rough = lba_db(&DuctingInputs { hm_m: 80.0, ..base() });
        assert!(rough > smooth, "{rough} vs {smooth}");
    }
}
