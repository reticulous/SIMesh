//! Attachment 2 to Annex 1: approximation to the inverse complementary
//! cumulative normal distribution function (eqs. 94–95), plus the single
//! knife-edge loss J(ν) (eq. 12) shared by several sections.

/// Inverse complementary cumulative normal, I(x) (eqs. 94a/94b).
///
/// Valid for 1e-6 ≤ x ≤ 0.999999 (max error 0.00054); inputs outside are
/// clamped per the Attachment. Callers for eq. (69) must additionally clamp
/// x to [0.01, 0.99] themselves.
pub fn inv_ccdf(x: f64) -> f64 {
    let x = x.clamp(1e-6, 0.999_999);
    if x <= 0.5 {
        t(x) - xi(t(x)) // (94a)
    } else {
        -(t(1.0 - x) - xi(t(1.0 - x))) // (94b)
    }
}

fn t(x: f64) -> f64 {
    (-2.0 * x.ln()).sqrt() // (95a)
}

fn xi(t: f64) -> f64 {
    // (95b) with constants (95c)–(95h)
    const C0: f64 = 2.515516698;
    const C1: f64 = 0.802853;
    const C2: f64 = 0.010328;
    const D1: f64 = 1.432788;
    const D2: f64 = 0.189269;
    const D3: f64 = 0.001308;
    (((C2 * t + C1) * t) + C0) / ((((D3 * t + D2) * t) + D1) * t + 1.0)
}

/// Single knife-edge diffraction loss approximation J(ν) (eq. 12);
/// J(ν) = 0 for ν ≤ −0.78.
pub fn j_nu(nu: f64) -> f64 {
    if nu <= -0.78 {
        0.0
    } else {
        6.9 + 20.0 * (((nu - 0.1).powi(2) + 1.0).sqrt() + nu - 0.1).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inv_ccdf_median_is_zero() {
        assert!(inv_ccdf(0.5).abs() < 1e-3);
    }

    #[test]
    fn inv_ccdf_matches_normal_quantiles() {
        // Standard normal: Q(x)=0.1 → 1.28155, Q(x)=0.01 → 2.32635.
        assert!((inv_ccdf(0.1) - 1.28155).abs() < 6e-4);
        assert!((inv_ccdf(0.01) - 2.32635).abs() < 6e-4);
        // Symmetry (94b).
        assert!((inv_ccdf(0.9) + inv_ccdf(0.1)).abs() < 2e-3);
    }

    #[test]
    fn j_nu_anchor_points() {
        assert_eq!(j_nu(-1.0), 0.0);
        // J(−0.78) ≈ 0 by construction (the Rec notes J(−0.78) ≈ 0).
        assert!(j_nu(-0.78 + 1e-9).abs() < 0.05);
        // Large ν grows ~ 13 + 20 log ν.
        assert!((j_nu(10.0) - (13.0 + 20.0)).abs() < 1.0);
    }
}
