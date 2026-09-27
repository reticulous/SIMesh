//! §4.3: Propagation by diffraction — the delta-Bullington model
//! (eqs. 13–43). Bullington runs on the SURFACE heights g_i (terrain +
//! representative clutter, eq. 1d); the smooth-Earth run uses zeroed heights
//! with antennas reduced by h_std/h_srd from Attachment 1 §5.6.2.

use super::math::{inv_ccdf, j_nu};
use planner_core::model::Polarization;

/// Wavelength in meters for f in GHz.
pub fn lambda_m(f_ghz: f64) -> f64 {
    0.299_792_458 / f_ghz
}

/// §4.3.1 — Bullington part. `g_masl` are profile heights (surface for the
/// actual run, all-zero for the smooth run); `htc`/`hrc` the (possibly
/// modified, eqs. 37a/b) antenna heights amsl; `ap_km` the effective Earth
/// radius in use (§4.3.5).
pub fn bullington_db(
    d_km: &[f64],
    g_masl: &[f64],
    htc: f64,
    hrc: f64,
    ap_km: f64,
    f_ghz: f64,
) -> f64 {
    let n = d_km.len();
    debug_assert!(n >= 3 && n == g_masl.len());
    let d = d_km[n - 1] - d_km[0];
    let ce = 1.0 / ap_km;
    let lam = lambda_m(f_ghz);

    // (13) highest slope TX → intermediate point.
    let mut stim = f64::NEG_INFINITY;
    for i in 1..n - 1 {
        let di = d_km[i];
        let s = (g_masl[i] + 500.0 * ce * di * (d - di) - htc) / di;
        stim = stim.max(s);
    }
    // (14) TX → RX slope.
    let str_ = (hrc - htc) / d;

    let luc = if stim < str_ {
        // Case 1: LoS. (15) highest diffraction parameter.
        let mut nu_max = f64::NEG_INFINITY;
        for i in 1..n - 1 {
            let di = d_km[i];
            let nu = (g_masl[i] + 500.0 * ce * di * (d - di)
                - (htc * (d - di) + hrc * di) / d)
                * (0.002 * d / (lam * di * (d - di))).sqrt();
            nu_max = nu_max.max(nu);
        }
        j_nu(nu_max) // (16)
    } else {
        // Case 2: trans-horizon. (17) highest slope RX → intermediate point.
        let mut srim = f64::NEG_INFINITY;
        for i in 1..n - 1 {
            let di = d_km[i];
            let s = (g_masl[i] + 500.0 * ce * di * (d - di) - hrc) / (d - di);
            srim = srim.max(s);
        }
        let dbp = (hrc - htc + srim * d) / (stim + srim); // (18)
        let nu_b = (htc + stim * dbp - (htc * (d - dbp) + hrc * dbp) / d)
            * (0.002 * d / (lam * dbp * (d - dbp))).sqrt(); // (19)
        j_nu(nu_b) // (20)
    };

    luc + (1.0 - (-luc / 6.0).exp()) * (10.0 + 0.02 * d) // (21)
}

/// Largest value of `f` over the INTERMEDIATE abscissae d_1 … d_{n−2}, found
/// from where f′ vanishes instead of by touching all n − 2 of them.
///
/// `stationary` lists the abscissae where f′ = 0; entries that are not finite
/// are skipped, which is how "this branch has no stationary point" is spelled.
/// Between two consecutive stationary points f is monotone, so over a finite
/// set of abscissae its maximum can only sit at an end of the set or at one of
/// the two points that bracket a stationary point — never strictly inside a
/// monotone run. Evaluating those 2 + 2·|stationary| points therefore returns
/// what the full scan returns bit for bit, as long as
///
/// * f is evaluated by the same expression the scan uses (the callers copy
///   eqs. (13), (15) and (17) verbatim with g_i written as the literal 0.0),
/// * `d_km[1..=n-2]` is sorted, which is what the bracketing search assumes.
///   `lb_from_arrays` enforces it: its §3.2 minimum-spacing check rejects any
///   profile with a gap under 30 m, and that includes every non-increasing one.
///
/// The scan keeps the maximum VALUE (`stim.max(s)`), not the index that
/// produced it, so which of several equal maxima is found is unobservable and
/// there is no tie rule left to reproduce.
fn max_over_intermediate(d_km: &[f64], stationary: &[f64], f: impl Fn(f64) -> f64) -> f64 {
    let hi = d_km.len() - 2;
    // NEG_INFINITY seed and `.max()` throughout, exactly as the scan does, so
    // NaN samples are swallowed the same way on both sides.
    let mut best = f64::NEG_INFINITY;
    best = best.max(f(d_km[1]));
    best = best.max(f(d_km[hi]));
    for &x in stationary {
        if !x.is_finite() {
            continue;
        }
        // First intermediate point at or after x, and the last one before it.
        let k = 1 + d_km[1..=hi].partition_point(|&v| v < x);
        if k <= hi {
            best = best.max(f(d_km[k]));
        }
        if k > 1 {
            best = best.max(f(d_km[k - 1]));
        }
    }
    best
}

/// Real roots of x³ + a2·x² + a1·x + a0 (Cardano, trigonometric form for the
/// three-real-root case). Used only to place bracketing grid points, so the
/// ~1e-12 relative accuracy of this form is three to four orders of magnitude better (measured: first divergence at 3e-6 m spacing on a 5 km path and 3e-3 m on 1000 km, against a 10 m floor)
/// than the ~1e-5 (half a 30 m cell on a 3000 km path) that would be needed to
/// pick the wrong pair.
fn cubic_real_roots(a2: f64, a1: f64, a0: f64) -> ([f64; 3], usize) {
    let s = a2 / 3.0;
    // Depressed cubic t³ + p·t + q for x = t − s.
    let p = a1 - 3.0 * s * s;
    let q = 2.0 * s * s * s - a1 * s + a0;
    let disc = 0.25 * q * q + p * p * p / 27.0;
    if disc > 0.0 {
        let r = disc.sqrt();
        let t = (-0.5 * q + r).cbrt() + (-0.5 * q - r).cbrt();
        ([t - s, 0.0, 0.0], 1)
    } else if p == 0.0 {
        // Triple root (p = q = 0 once disc ≤ 0 forces q² ≤ −4p³/27 = 0).
        ([-s, 0.0, 0.0], 1)
    } else {
        let m = 2.0 * (-p / 3.0).sqrt();
        let arg = (3.0 * q / (2.0 * p)) * (-3.0 / p).sqrt();
        let phi = arg.clamp(-1.0, 1.0).acos() / 3.0;
        let tau = std::f64::consts::TAU / 3.0;
        (
            [
                m * phi.cos() - s,
                m * (phi - tau).cos() - s,
                m * (phi - 2.0 * tau).cos() - s,
            ],
            3,
        )
    }
}

/// §4.3.1 for the smooth-Earth run of eqs. (37a/b), where g_i ≡ 0.
///
/// Same numbers as `bullington_db(d_km, &vec![0.0; n], …)` — every value kept
/// is produced by the identical expression with g_i written as the literal 0.0
/// — but it neither allocates the zero profile nor scans it twice. With g ≡ 0
/// each maximisation collapses to a smooth function of d_i alone whose
/// stationary points are known in closed form:
///
/// * eq. (13): s(x) = a·(d − x) − h_tc/x with a = 500/a_p, so s′ = −a + h_tc/x²
///   and the single stationary point is √(h_tc/a) — none at all, s falling
///   throughout, when h_tc ≤ 0;
/// * eq. (17): the mirror image, d − √(h_rc/a);
/// * eq. (15): ν(x) = C(x)·√(K/P(x)) with P(x) = x(d − x), K = 0.002·d/λ and
///   the clearance C(x) = a·P(x) − h_tc − m·x, m = (h_rc − h_tc)/d. Then
///   dν/dx = ½·√K·P^(−3/2)·φ(x) with the cubic
///   φ(x) = 2a·x³ − 3a·d·x² + (a·d² − h_rc − h_tc)·x + h_tc·d,
///   so ν turns at most three times.
///
/// Caller contract (checked in `delta_bullington_db`, which falls back to the
/// literal scan when it does not hold): d_km sorted, n ≥ 3, a_p > 0,
/// 0 < d_1 and d_{n−2} < d, so that P(d_i) > 0 on every intermediate point.
fn bullington_smooth_db(d_km: &[f64], htc: f64, hrc: f64, ap_km: f64, f_ghz: f64) -> f64 {
    let n = d_km.len();
    debug_assert!(n >= 3);
    debug_assert!(d_km.windows(2).all(|w| w[0] < w[1]), "profile must be sorted");
    let d = d_km[n - 1] - d_km[0];
    let ce = 1.0 / ap_km;
    let lam = lambda_m(f_ghz);
    let a = 500.0 * ce; // earth-bulge coefficient of eqs. (13)–(17)

    // Cheap exit, before eq. (13) is touched at all, for the ordinary case: a
    // ray that clears the smooth bulge everywhere.
    //
    // C is a downward parabola, so its largest value over the intermediate
    // span is C at the vertex clamped into that span — no scan, and clamping
    // rather than taking the free vertex matters, because asymmetric terminals
    // push the vertex off the path entirely (h_tc = 20 m, h_rc = 12 m over
    // 5 km puts it at 16.8 km, where the free bound reads −4.2 m against a
    // true −12.0 m and would give up on this exit). With C ≤ C_max and
    // P(x) ≤ d²/4, ν ≤ 2·C_max·√K/d whenever C_max < 0; squaring that (both
    // sides negative) turns it into C_max² > 0.78²/4 · d²/K = 76.05·λ·d, which
    // costs no square root. Two things then follow, and both are needed:
    //
    //  * C_max < 0 means C(d_i) < 0 at every intermediate point, and
    //    C(d_i) = d_i·(s_i − S_tr), so S_tim < S_tr: eq. (14) takes the LoS
    //    branch. Not an assumption — a consequence.
    //  * J(ν) is exactly 0 for ν ≤ −0.78, and eq. (21) with L_uc = 0 is
    //    exactly +0.0 (exp(−0.0) = 1 to the bit), so the answer is known
    //    without locating the maximum.
    //
    // The 1e-8 of relative margin is ~4e-9 in ν, seven orders above the ~1e-16
    // rounding in ν and in the eq. (14) comparison, so this branch cannot
    // disagree with the scan about either. It is the branch a deployment
    // profile takes — 5 km with 20 m/12 m terminals clears by 12.0 m against
    // the 11.5 m this demands — and only a grazing geometry falls through to
    // the algebra below.
    let str_ = (hrc - htc) / d; // (14) TX → RX slope
    // max/min, not clamp(): f64::clamp panics when min > max, and an unsorted
    // profile is exactly the input the fallback exists for. The guard now
    // rejects those too, but a panic behind a guard is still a panic.
    let xv = ((a * d - str_) / (2.0 * a)).max(d_km[1]).min(d_km[n - 2]);
    let c_max = a * xv * (d - xv) - htc - str_ * xv;
    if c_max < 0.0 && c_max * c_max > 76.05 * lam * d * (1.0 + 1e-8) {
        return 0.0;
    }

    // (13) highest slope TX → intermediate point.
    let stim = max_over_intermediate(
        d_km,
        &[if htc > 0.0 { (htc / a).sqrt() } else { f64::NAN }],
        |di| (0.0 + 500.0 * ce * di * (d - di) - htc) / di,
    );

    let luc = if stim < str_ {
        // Case 1: LoS. (15) highest diffraction parameter.
        let (roots, k) = cubic_real_roots(
            -1.5 * d,
            (a * d * d - hrc - htc) / (2.0 * a),
            htc * d / (2.0 * a),
        );
        let nu_max = max_over_intermediate(d_km, &roots[..k], |di| {
            (0.0 + 500.0 * ce * di * (d - di) - (htc * (d - di) + hrc * di) / d)
                * (0.002 * d / (lam * di * (d - di))).sqrt()
        });
        j_nu(nu_max) // (16)
    } else {
        // Case 2: trans-horizon. (17) highest slope RX → intermediate point.
        let srim = max_over_intermediate(
            d_km,
            &[if hrc > 0.0 { d - (hrc / a).sqrt() } else { f64::NAN }],
            |di| (0.0 + 500.0 * ce * di * (d - di) - hrc) / (d - di),
        );
        let dbp = (hrc - htc + srim * d) / (stim + srim); // (18)
        let nu_b = (htc + stim * dbp - (htc * (d - dbp) + hrc * dbp) / d)
            * (0.002 * d / (lam * dbp * (d - dbp))).sqrt(); // (19)
        j_nu(nu_b) // (20)
    };

    luc + (1.0 - (-luc / 6.0).exp()) * (10.0 + 0.02 * d) // (21)
}

/// §4.3.3 — first-term spherical-Earth diffraction loss L_dft for a given
/// effective Earth radius (eqs. 28–36). ω = sea fraction of the path.
fn first_term_db(
    adft_km: f64,
    f_ghz: f64,
    htesph: f64,
    hresph: f64,
    d_km: f64,
    omega: f64,
    pol: Polarization,
) -> f64 {
    let land = first_term_single(adft_km, f_ghz, htesph, hresph, d_km, 22.0, 0.003, pol);
    let sea = first_term_single(adft_km, f_ghz, htesph, hresph, d_km, 80.0, 5.0, pol);
    omega * sea + (1.0 - omega) * land // (28)
}

fn first_term_single(
    adft: f64,
    f: f64,
    htesph: f64,
    hresph: f64,
    d: f64,
    eps_r: f64,
    sigma: f64,
    pol: Polarization,
) -> f64 {
    // (29a) normalized surface admittance, horizontal.
    let kh = 0.036 * (adft * f).powf(-1.0 / 3.0)
        * ((eps_r - 1.0).powi(2) + (18.0 * sigma / f).powi(2)).powf(-0.25);
    let k = match pol {
        Polarization::Horizontal => kh,
        // (29b)
        Polarization::Vertical => kh * (eps_r.powi(2) + (18.0 * sigma / f).powi(2)).sqrt(),
    };
    let k2 = k * k;
    let k4 = k2 * k2;
    let beta_dft = (1.0 + 1.6 * k2 + 0.67 * k4) / (1.0 + 4.5 * k2 + 1.53 * k4); // (30)
    let x = 21.88 * beta_dft * (f / (adft * adft)).powf(1.0 / 3.0) * d; // (31)
    let yt = 0.9575 * beta_dft * (f * f / adft).powf(1.0 / 3.0) * htesph; // (32a)
    let yr = 0.9575 * beta_dft * (f * f / adft).powf(1.0 / 3.0) * hresph; // (32b)
    let fx = if x >= 1.6 {
        11.0 + 10.0 * x.log10() - 17.6 * x
    } else {
        -20.0 * x.log10() - 5.6488 * x.powf(1.425)
    }; // (33)
    let g = |y: f64| -> f64 {
        let b = beta_dft * y; // (35)
        let gy = if b > 2.0 {
            17.6 * (b - 1.1).sqrt() - 5.0 * (b - 1.1).log10() - 8.0
        } else {
            20.0 * (b + 0.1 * b.powi(3)).log10()
        }; // (34)
        gy.max(2.0 + 20.0 * k.log10()) // limit under (35)
    };
    -fx - g(yt) - g(yr) // (36)
}

/// §4.3.2 — spherical-Earth diffraction loss L_dsph (eqs. 22–27).
fn spherical_db(
    ap_km: f64,
    f_ghz: f64,
    htesph: f64,
    hresph: f64,
    d_km: f64,
    omega: f64,
    pol: Polarization,
) -> f64 {
    let lam = lambda_m(f_ghz);
    // (22) marginal LoS distance for a smooth path.
    let dlos = (2.0 * ap_km).sqrt() * ((0.001 * htesph).sqrt() + (0.001 * hresph).sqrt());
    if d_km >= dlos {
        return first_term_db(ap_km, f_ghz, htesph, hresph, d_km, omega, pol);
    }
    // (24e), (24d), (24c), (24a), (24b)
    let mc = 250.0 * d_km * d_km / (ap_km * (htesph + hresph));
    let c = (htesph - hresph) / (htesph + hresph);
    let arg = (3.0 * c / 2.0) * (3.0 * mc / (mc + 1.0).powi(3)).sqrt();
    let b = 2.0 * ((mc + 1.0) / (3.0 * mc)).sqrt()
        * (std::f64::consts::PI / 3.0 + arg.clamp(-1.0, 1.0).acos() / 3.0).cos();
    let dse1 = d_km / 2.0 * (1.0 + b);
    let dse2 = d_km - dse1;
    // (23) smallest clearance height.
    let hse = ((htesph - 500.0 * dse1 * dse1 / ap_km) * dse2
        + (hresph - 500.0 * dse2 * dse2 / ap_km) * dse1)
        / d_km;
    // (25) required clearance for zero diffraction loss.
    let hreq = 17.456 * (dse1 * dse2 * lam / d_km).sqrt();
    if hse > hreq {
        return 0.0;
    }
    // (26) modified effective Earth radius giving marginal LoS at d.
    let aem = 500.0 * (d_km / (htesph.sqrt() + hresph.sqrt())).powi(2);
    let ldft = first_term_db(aem, f_ghz, htesph, hresph, d_km, omega, pol);
    if ldft < 0.0 {
        return 0.0;
    }
    (1.0 - hse / hreq) * ldft // (27)
}

/// Inputs to the complete §4.3.4 model that don't depend on a_p.
pub struct DiffractionPath<'a> {
    pub d_km: &'a [f64],
    /// Surface heights g_i (eq. 1d): terrain + representative clutter,
    /// terminals excluded.
    pub g_masl: &'a [f64],
    /// Antenna heights amsl (Table 5: h_tc = h_ts, h_rc = h_rs).
    pub htc: f64,
    pub hrc: f64,
    /// Smooth-surface heights for the diffraction model (eqs. 89a–d).
    pub hstd: f64,
    pub hsrd: f64,
    pub f_ghz: f64,
    /// Fraction of the path over sea (Table 5: ω).
    pub omega: f64,
    pub pol: Polarization,
}

/// §4.3.4 — complete delta-Bullington diffraction loss L_d for one a_p
/// (eqs. 37–39).
pub fn delta_bullington_db(p: &DiffractionPath, ap_km: f64) -> f64 {
    let n = p.d_km.len();
    let d_total = p.d_km[n - 1] - p.d_km[0];
    let lbulla = bullington_db(p.d_km, p.g_masl, p.htc, p.hrc, ap_km, p.f_ghz);
    // (37a/b) smooth path: all heights zero, antennas reduced.
    let htc_s = p.htc - p.hstd;
    let hrc_s = p.hrc - p.hsrd;
    // The zero profile is never materialised: with g ≡ 0 eqs. (13)/(15)/(17)
    // have closed-form stationary points, so `bullington_smooth_db` reads a
    // handful of profile points instead of allocating n zeros and scanning
    // them twice — this runs once per a_p per pixel, and a 5 km radial carries
    // ~170 of them. Its algebra needs P(d_i) = d_i·(d − d_i) > 0 on every
    // intermediate point and a positive earth radius; a profile that does not
    // start at its own origin gets the literal eq. (13)–(21) scan rather than
    // an answer from algebra that does not describe it.
    // Strictly increasing distances are the algorithm's own precondition (the
    // bracketing around each stationary point assumes monotone runs), and the
    // callers two layers up do enforce it -- but the fallback exists precisely
    // for contract violations, and an unsorted profile that passed the old
    // guard diverged silently by up to 17.6 dB (review finding). One pass.
    let sorted = p.d_km.windows(2).all(|w| w[1] > w[0]);
    let smooth_ok = sorted
        && ap_km > 0.0 && d_total > 0.0 && p.d_km[1] > 0.0 && p.d_km[n - 2] < d_total;
    let lbulls = if smooth_ok {
        bullington_smooth_db(p.d_km, htc_s, hrc_s, ap_km, p.f_ghz)
    } else {
        bullington_db(p.d_km, &vec![0.0; n], htc_s, hrc_s, ap_km, p.f_ghz)
    };
    // (38a/b) spherical-Earth with the same modified heights.
    let ldsph = spherical_db(ap_km, p.f_ghz, htc_s, hrc_s, d_total, p.omega, p.pol);
    lbulla + (ldsph - lbulls).max(0.0) // (39)
}

/// §4.3.5 — diffraction loss not exceeded for p% time: (L_d50, L_dp)
/// (eqs. 40–41). `ae`/`abeta` per eqs. (7a)/(7b); β0 per eq. (5).
pub fn ldp_db(
    path: &DiffractionPath,
    ae_km: f64,
    abeta_km: f64,
    p_pct: f64,
    beta0_pct: f64,
) -> (f64, f64) {
    let ld50 = delta_bullington_db(path, ae_km);
    if p_pct >= 50.0 {
        return (ld50, ld50);
    }
    let ldbeta = delta_bullington_db(path, abeta_km);
    let fi = fi_factor(p_pct, beta0_pct);
    (ld50, ld50 + (ldbeta - ld50) * fi) // (41)
}

/// F_i — diffraction interpolation factor (eq. 40), also consumed by the
/// §4.6 combination (eq. 59).
pub fn fi_factor(p_pct: f64, beta0_pct: f64) -> f64 {
    if p_pct >= 50.0 {
        0.0
    } else if p_pct > beta0_pct {
        inv_ccdf(p_pct / 100.0) / inv_ccdf(beta0_pct / 100.0)
    } else {
        1.0 // 1% ≤ p ≤ β0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spaced(d: f64, n: usize) -> Vec<f64> {
        (0..n).map(|i| d * i as f64 / (n - 1) as f64).collect()
    }

    const AE: f64 = 8930.6; // ΔN = 45

    /// The pre-existing eq. (13)–(21) scan over an explicit all-zero profile:
    /// what `bullington_smooth_db` has to reproduce bit for bit. Kept here
    /// rather than referenced through `bullington_db` so the comparison stays
    /// pinned to the code as it was even if the general path is retuned.
    fn smooth_by_scan(d_km: &[f64], htc: f64, hrc: f64, ap_km: f64, f_ghz: f64) -> f64 {
        let n = d_km.len();
        let d = d_km[n - 1] - d_km[0];
        let ce = 1.0 / ap_km;
        let lam = lambda_m(f_ghz);
        let g_masl = vec![0.0; n];

        let mut stim = f64::NEG_INFINITY;
        for i in 1..n - 1 {
            let di = d_km[i];
            let s = (g_masl[i] + 500.0 * ce * di * (d - di) - htc) / di;
            stim = stim.max(s);
        }
        let str_ = (hrc - htc) / d;

        let luc = if stim < str_ {
            let mut nu_max = f64::NEG_INFINITY;
            for i in 1..n - 1 {
                let di = d_km[i];
                let nu = (g_masl[i] + 500.0 * ce * di * (d - di)
                    - (htc * (d - di) + hrc * di) / d)
                    * (0.002 * d / (lam * di * (d - di))).sqrt();
                nu_max = nu_max.max(nu);
            }
            j_nu(nu_max)
        } else {
            let mut srim = f64::NEG_INFINITY;
            for i in 1..n - 1 {
                let di = d_km[i];
                let s = (g_masl[i] + 500.0 * ce * di * (d - di) - hrc) / (d - di);
                srim = srim.max(s);
            }
            let dbp = (hrc - htc + srim * d) / (stim + srim);
            let nu_b = (htc + stim * dbp - (htc * (d - dbp) + hrc * dbp) / d)
                * (0.002 * d / (lam * dbp * (d - dbp))).sqrt();
            j_nu(nu_b)
        };
        luc + (1.0 - (-luc / 6.0).exp()) * (10.0 + 0.02 * d)
    }

    /// Which arm of `bullington_smooth_db` a geometry lands in, recomputed
    /// independently so the property test can prove it reached all three.
    #[derive(PartialEq, Eq, Debug, Clone, Copy)]
    enum Arm {
        /// Case 1 answered from the ν ≤ −0.78 bound, no maximum located.
        LosBounded,
        /// Case 1 with the cubic's stationary points bracketed.
        LosCubic,
        /// Case 2, trans-horizon.
        TransHorizon,
    }

    /// Which branch eqs. (13)/(14) send the SCAN down — the ground truth the
    /// closed form's cheap exit claims to predict without scanning.
    fn scan_takes_los_branch(d_km: &[f64], htc: f64, hrc: f64, ap_km: f64) -> bool {
        let n = d_km.len();
        let d = d_km[n - 1] - d_km[0];
        let a = 500.0 / ap_km;
        let mut stim = f64::NEG_INFINITY;
        for &di in &d_km[1..n - 1] {
            stim = stim.max((a * di * (d - di) - htc) / di);
        }
        stim < (hrc - htc) / d
    }

    fn arm_of(d_km: &[f64], htc: f64, hrc: f64, ap_km: f64, f_ghz: f64) -> Arm {
        let n = d_km.len();
        let d = d_km[n - 1] - d_km[0];
        let a = 500.0 / ap_km;
        let str_ = (hrc - htc) / d;
        let xv = ((a * d - str_) / (2.0 * a)).max(d_km[1]).min(d_km[n - 2]);
        let c_max = a * xv * (d - xv) - htc - str_ * xv;
        if c_max < 0.0 && c_max * c_max > 76.05 * lambda_m(f_ghz) * d * (1.0 + 1e-8) {
            Arm::LosBounded
        } else if scan_takes_los_branch(d_km, htc, hrc, ap_km) {
            Arm::LosCubic
        } else {
            Arm::TransHorizon
        }
    }

    /// xorshift64*, so the sweep below is a fixed set of geometries and a
    /// failure is reproducible from the seed alone.
    struct Rng(u64);

    impl Rng {
        fn bits(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn unit(&mut self) -> f64 {
            (self.bits() >> 11) as f64 / (1u64 << 53) as f64
        }
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.unit()
        }
    }

    /// Profile shapes the model actually sees. Shape 1 is the one
    /// `planner-coverage` builds: a uniform stride with a short final step to
    /// the pixel. Shape 3 (dense near the transmitter) is where the closed
    /// form's bracketing has the least room, since d* then falls between
    /// points a few metres apart.
    fn profile(shape: usize, d: f64, n: usize, rng: &mut Rng) -> Vec<f64> {
        let last = (n - 1) as f64;
        match shape {
            0 => (0..n).map(|i| d * i as f64 / last).collect(),
            1 => {
                let h = d / (last + rng.range(0.05, 0.95));
                let mut v: Vec<f64> = (0..n - 1).map(|i| h * i as f64).collect();
                v.push(d);
                v
            }
            2 => {
                let mut v: Vec<f64> = (0..n)
                    .map(|i| d * (i as f64 + rng.range(-0.4, 0.4)) / last)
                    .collect();
                v[0] = 0.0;
                v[n - 1] = d;
                v
            }
            _ => (0..n).map(|i| d * (i as f64 / last).powf(1.7)).collect(),
        }
    }

    #[test]
    fn smooth_bullington_is_bit_identical_to_the_zero_profile_scan() {
        // The whole point of the closed form is that it changes nothing: the
        // maximum it evaluates is the same grid point the scan found, so the
        // dB it returns must match to the last bit, not to a tolerance.
        let mut rng = Rng(0x2026_0902_1812);
        let mut arms = [0usize; 3];
        let mut nonzero = 0usize;
        let mut cases = 0usize;
        for shape in 0..4 {
            for &n in &[3usize, 4, 5, 8, 17, 41, 171, 400] {
                for _ in 0..800 {
                    let d = (10f64).powf(rng.range(-0.6, 2.7)); // 0.25 … 500 km
                    let ap = rng.range(1000.0, 30000.0);
                    let f = (10f64).powf(rng.range(-1.52, 0.778)); // 0.03 … 6 GHz
                    let (htc, hrc) = if rng.unit() < 0.5 {
                        // Negative terminal heights cannot come out of eq. (89)
                        // (h_std ≤ h[0] ≤ h_tc) but `bullington_db` accepts
                        // them, and they are exactly the case where eq. (13)
                        // has no stationary point at all.
                        (rng.range(-20.0, 3000.0), rng.range(-20.0, 3000.0))
                    } else {
                        // Terminals within a stone's throw of the smooth bulge
                        // a·d²/4. Half the draws are aimed at this band on
                        // purpose: it is where the ν bound gives up and the
                        // cubic actually has to find the maximum, and a sweep
                        // of unaimed geometries barely reaches it.
                        let bulge = 500.0 / ap * d * d / 4.0;
                        (bulge * rng.range(0.5, 1.7), bulge * rng.range(0.5, 1.7))
                    };
                    let d_km = profile(shape, d, n, &mut rng);
                    assert!(d_km.windows(2).all(|w| w[0] < w[1]), "generator broke: {d_km:?}");
                    let want = smooth_by_scan(&d_km, htc, hrc, ap, f);
                    let got = bullington_smooth_db(&d_km, htc, hrc, ap, f);
                    assert_eq!(
                        want.to_bits(),
                        got.to_bits(),
                        "shape {shape} n {n} d {d} ap {ap} f {f} htc {htc} hrc {hrc}: \
                         {want:?} vs {got:?}"
                    );
                    let arm = arm_of(&d_km, htc, hrc, ap, f);
                    if arm == Arm::LosBounded {
                        // The exit skips eq. (13) on the argument that C_max<0
                        // forces the LoS branch. If that ever failed, the exit
                        // would be answering a trans-horizon path with 0 dB.
                        assert!(
                            scan_takes_los_branch(&d_km, htc, hrc, ap),
                            "ν-bound exit taken on a path eq. (14) sends \
                             trans-horizon: d {d} ap {ap} htc {htc} hrc {hrc}"
                        );
                    }
                    arms[match arm {
                        Arm::LosBounded => 0,
                        Arm::LosCubic => 1,
                        Arm::TransHorizon => 2,
                    }] += 1;
                    if got != 0.0 {
                        nonzero += 1;
                    }
                    cases += 1;
                }
            }
        }
        // A sweep that only ever took the cheap ν-bound exit would prove
        // nothing about the cubic, and one that never produced loss would
        // prove nothing at all. Measured on this seed: 12610 bounded / 7699
        // cubic / 5291 trans-horizon over 25600 geometries, 12849 of which had
        // non-zero loss.
        println!("arms {arms:?} nonzero {nonzero}/{cases}");
        assert!(arms.iter().all(|&c| c > 2000), "arm coverage {arms:?}");
        assert!(nonzero > cases / 4, "only {nonzero}/{cases} geometries had any loss");
    }

    #[test]
    fn smooth_bullington_matches_where_the_stationary_point_lands_on_a_node() {
        // d* = √(h_tc·a_p/500) is the eq. (13) maximiser. The discrete maximum
        // is NOT that point, so the three placements that can go wrong are
        // swept explicitly: d* outside the intermediate range on either side,
        // exactly on a node, and each way between two nodes.
        let n = 33;
        let d = 20.0;
        let d_km = spaced(d, n);
        let a = 500.0 / AE;
        let step = d / (n - 1) as f64; // 0.625 km
        let mut checked = 0;
        for target in [
            -1.0,             // below d_1 (h_tc ≤ 0: no stationary point at all)
            0.0,              // at the profile origin, outside the range
            step * 0.5,       // between d_0 and d_1, i.e. left of the range
            step,             // exactly on d_1, the first intermediate point
            step * 7.0,       // exactly on a node in the middle
            step * 7.25,      // just past a node
            step * 7.5,       // midway between two nodes
            step * 7.75,      // just before the next node
            step * (n as f64 - 2.0), // exactly on d_{n−2}, the last one
            d * 1.5,          // above the range: eq. (13) still rising at d_{n−2}
            d * 40.0,         // far above it
        ] {
            let htc = a * target * target * target.signum();
            for hrc in [1.0, 30.0, 300.0] {
                for f in [0.0868, 0.868, 5.8] {
                    let want = smooth_by_scan(&d_km, htc, hrc, AE, f);
                    let got = bullington_smooth_db(&d_km, htc, hrc, AE, f);
                    assert_eq!(
                        want.to_bits(),
                        got.to_bits(),
                        "d* target {target} (h_tc {htc}) h_rc {hrc} f {f}: {want:?} vs {got:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 11 * 3 * 3);
    }

    #[test]
    fn candidate_set_clamps_stationary_points_to_the_intermediate_range() {
        // The defect this pins is the off-by-one at the ends: a stationary
        // point below d_1 or above d_{n−2} must still leave the maximum at the
        // nearest INTERMEDIATE point — the terminals (i = 0, i = n−1) are not
        // eligible, and neither is an index past the end of the slice.
        let d_km = spaced(10.0, 21);
        let brute = |x0: f64| {
            let mut m = f64::NEG_INFINITY;
            for &di in &d_km[1..d_km.len() - 1] {
                m = m.max(-(di - x0) * (di - x0));
            }
            m
        };
        for x0 in [-5.0, 0.0, 0.5, 1.0, 4.75, 5.0, 9.5, 10.0, 25.0] {
            let f = |di: f64| -(di - x0) * (di - x0);
            assert_eq!(
                max_over_intermediate(&d_km, &[x0], f).to_bits(),
                brute(x0).to_bits(),
                "stationary point at {x0}"
            );
            // A non-finite entry means "no stationary point" and must be
            // skipped, not bracketed.
            assert_eq!(
                max_over_intermediate(&d_km, &[f64::NAN, x0, f64::INFINITY], f).to_bits(),
                brute(x0).to_bits(),
                "non-finite entries changed the answer at {x0}"
            );
        }
    }

    #[test]
    fn cubic_roots_solve_the_cubic() {
        // Both branches of the solver, checked by substitution rather than
        // against expected roots: three real roots (2, −1, −3) and one
        // (2 plus a complex pair).
        for (a2, a1, a0, want) in [
            (2.0f64, -5.0f64, -6.0f64, 3usize),
            (-1.0, 2.0, -8.0, 1),
            (0.0, 0.0, 0.0, 1),
        ] {
            let (roots, k) = cubic_real_roots(a2, a1, a0);
            assert_eq!(k, want, "root count for x³+{a2}x²+{a1}x+{a0}");
            for &x in &roots[..k] {
                let r = x * x * x + a2 * x * x + a1 * x + a0;
                assert!(r.abs() < 1e-9, "root {x} leaves residual {r}");
            }
        }
    }

    /// Not a test — the measurement behind the change. Run with
    /// `cargo test -p planner-propag --release -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_smooth_bullington() {
        use std::hint::black_box;
        use std::time::Instant;
        // Minimum over rounds, not the mean: this box compiles other crates
        // while the bench runs, and interference can only ever ADD time.
        let round = |f: &dyn Fn(&[f64], f64) -> f64, d_km: &[f64], reps: u32| -> f64 {
            let mut best = f64::INFINITY;
            for _ in 0..7 {
                let t = Instant::now();
                for i in 0..reps {
                    // Sweep the terminal height so nothing hoists out of the loop.
                    black_box(f(black_box(d_km), 20.0 + (i % 97) as f64 * 0.1));
                }
                best = best.min(t.elapsed().as_secs_f64() / reps as f64 * 1e9);
            }
            best
        };
        // Cleared: a 5 km radial with 20 m/12 m terminals — the ν bound holds
        // and the answer is 0 dB. Grazing: 42 km with terminals just above the
        // 24.7 m bulge, so the bound fails and the cubic runs. The arm each
        // one lands in is printed, not assumed.
        let scan = |d: &[f64], h: f64| smooth_by_scan(d, h, 12.0, AE, 0.868);
        let closed = |d: &[f64], h: f64| bullington_smooth_db(d, h, 12.0, AE, 0.868);
        let scan_g = |d: &[f64], h: f64| smooth_by_scan(d, h * 1.6, 40.0, AE, 0.868);
        let closed_g = |d: &[f64], h: f64| bullington_smooth_db(d, h * 1.6, 40.0, AE, 0.868);
        // A 5 km radial sampled at 30 m is the geometry `planner-coverage`
        // walks; the other lengths bracket it.
        for &n in &[6usize, 21, 84, 167, 400] {
            let d_km = spaced(5.0, n);
            let (old, new) = (round(&scan, &d_km, 200_000), round(&closed, &d_km, 200_000));
            let g_km = spaced(42.0, n);
            let (old_g, new_g) =
                (round(&scan_g, &g_km, 200_000), round(&closed_g, &g_km, 200_000));
            let arm = arm_of(&d_km, 24.0, 12.0, AE, 0.868);
            let arm_g = arm_of(&g_km, 38.4, 40.0, AE, 0.868);
            println!(
                "n={n:4}  {arm:?}: scan {old:7.1} closed {new:7.1} ns ({:4.1}x)   \
                 {arm_g:?}: scan {old_g:7.1} closed {new_g:7.1} ns ({:4.1}x)",
                old / new,
                old_g / new_g
            );
        }
    }

    #[test]
    fn clear_los_path_has_zero_bullington_loss() {
        // Flat zero terrain, antennas 30 m: huge negative ν → J = 0 → L_bull = 0.
        let d = spaced(5.0, 11);
        let g = vec![0.0; 11];
        let l = bullington_db(&d, &g, 30.0, 30.0, AE, 0.868);
        assert_eq!(l, 0.0);
    }

    /// The early exit's margin is load-bearing, and nothing pinned it.
    ///
    /// The exit fires on a BOUND: C_max² > 76.05·λ·d·(1+1e-8), derived from
    /// P(x) ≤ d²/4. That bound is tight only when the parabola's vertex sits at
    /// mid-path, i.e. for SYMMETRIC terminals -- anywhere else it is slack and
    /// a loosened margin fires harmlessly early. So the geometry here is
    /// symmetric (h_tc = h_rc), where the exit's threshold coincides with the
    /// point at which J(ν) itself reaches exactly 0, and the transmitter
    /// height is walked in ulps across that point demanding bit equality with
    /// the literal scan at every step. Measured by an independent review with
    /// the margin as a knob: at exactly 1.0, three of 2.24 M banded cases
    /// exited early (worst 0.024 dB); the shipped 1+1e-8 gave none. Loosened
    /// to 1−1e-4 this test fails at the first step past the transition.
    #[test]
    fn the_early_exit_margin_is_pinned_at_the_transition() {
        let ap = 8500.0;
        let mut checked = 0usize;
        for &(d_total, n, f_ghz) in &[(5.0f64, 21usize, 0.868f64), (12.0, 61, 0.868), (42.0, 85, 2.4), (0.9, 7, 6.0)] {
            let d: Vec<f64> = (0..n).map(|i| d_total * i as f64 / (n - 1) as f64).collect();
            let zeros = vec![0.0; n];
            let new = |h: f64| bullington_smooth_db(&d, h, h, ap, f_ghz);
            let old = |h: f64| bullington_db(&d, &zeros, h, h, ap, f_ghz);
            let (mut lo, mut hi) = (-400.0f64, 400.0f64);
            assert!(new(lo) > 0.0 && new(hi) == 0.0, "bracket assumption failed for d={d_total}");
            for _ in 0..200 {
                let mid = 0.5 * (lo + hi);
                if new(mid) == 0.0 { hi = mid } else { lo = mid }
            }
            let mut h = lo;
            for _ in 0..400 { h = f64::from_bits(h.to_bits() - 1); }
            for _ in 0..800 {
                assert_eq!(new(h).to_bits(), old(h).to_bits(), "d={d_total} km f={f_ghz} h={h:e}: new {} old {}", new(h), old(h));
                h = f64::from_bits(h.to_bits() + 1);
                checked += 1;
            }
        }
        assert_eq!(checked, 3200);
    }

    /// An unsorted profile that the old guard admitted must neither panic nor
    /// diverge from the scan: it is routed to the scan.
    ///
    /// Review finding: `[0, 3, 1, 5]` passed the guard (d_1 > 0, d_{n-2} < d)
    /// and hit `f64::clamp` with min > max -- a panic where the old code
    /// returned a number -- and `[0, 3, 1, 4, 5]` with h_tc = −5, h_rc = 12 at
    /// 6 GHz returned 0.0 where the scan gives 17.573 dB.
    #[test]
    fn an_unsorted_profile_is_routed_to_the_scan_not_the_algebra() {
        let cases: [(&[f64], f64, f64, f64); 3] = [
            (&[0.0, 3.0, 1.0, 5.0], -5.0, 12.0, 6.0),
            (&[0.0, 3.0, 1.0, 4.0, 5.0], -5.0, 12.0, 6.0),
            (&[0.0, 4.0, 4.5, 1.0, 2.0, 5.0], 10.0, 10.0, 0.868),
        ];
        for (d, htc, hrc, f) in cases {
            let n = d.len();
            let g = vec![0.0; n];
            let p = DiffractionPath { d_km: d, g_masl: &g, htc, hrc, hstd: 0.0, hsrd: 0.0, f_ghz: f, omega: 0.0, pol: Polarization::Vertical };
            let got = delta_bullington_db(&p, 8500.0);
            let lbulla = bullington_db(d, &g, htc, hrc, 8500.0, f);
            let lbulls = bullington_db(d, &g, htc, hrc, 8500.0, f);
            let ldsph = spherical_db(8500.0, f, htc, hrc, d[n - 1] - d[0], 0.0, Polarization::Vertical);
            let want = lbulla + (ldsph - lbulls).max(0.0);
            assert_eq!(got.to_bits(), want.to_bits(), "profile {d:?}");
        }
    }

    #[test]
    fn knife_edge_costs_and_grows_with_height() {
        let d = spaced(10.0, 21);
        let mut g = vec![0.0; 21];
        g[10] = 40.0;
        let l40 = bullington_db(&d, &g, 10.0, 10.0, AE, 0.868);
        g[10] = 80.0;
        let l80 = bullington_db(&d, &g, 10.0, 10.0, AE, 0.868);
        assert!(l40 > 6.0, "obstructed path must lose > J(0)=6.9-ish, got {l40}");
        assert!(l80 > l40, "higher edge must cost more: {l80} vs {l40}");
    }

    #[test]
    fn bullington_is_reciprocal() {
        // Swapping TX/RX (reversing the profile) must not change the loss.
        let d = spaced(12.0, 25);
        let mut g = vec![0.0; 25];
        g[7] = 35.0;
        g[16] = 55.0;
        let fwd = bullington_db(&d, &g, 20.0, 40.0, AE, 0.868);
        let g_rev: Vec<f64> = g.iter().rev().copied().collect();
        let rev = bullington_db(&d, &g_rev, 40.0, 20.0, AE, 0.868);
        assert!((fwd - rev).abs() < 1e-9, "{fwd} vs {rev}");
    }

    #[test]
    fn smooth_path_reduces_to_spherical_model() {
        // g ≡ 0 and h_std = h_srd = 0 → L_bulla = L_bulls → L_d = L_dsph
        // ("for a perfectly smooth path the final diffraction loss will be the
        // output of the spherical-Earth model", §4.3).
        let d = spaced(60.0, 61);
        let g = vec![0.0; 61];
        let path = DiffractionPath {
            d_km: &d,
            g_masl: &g,
            htc: 20.0,
            hrc: 20.0,
            hstd: 0.0,
            hsrd: 0.0,
            f_ghz: 0.868,
            omega: 0.0,
            pol: Polarization::Vertical,
        };
        let ld = delta_bullington_db(&path, AE);
        let lsph = spherical_db(AE, 0.868, 20.0, 20.0, 60.0, 0.0, Polarization::Vertical);
        let lbulls = bullington_db(&d, &g, 20.0, 20.0, AE, 0.868);
        assert!((ld - (lbulls + (lsph - lbulls).max(0.0))).abs() < 1e-9);
        // 60 km with 20 m antennas is far beyond d_los → real loss.
        assert!(lsph > 20.0, "expected substantial spherical loss, got {lsph}");
    }

    #[test]
    fn short_high_clearance_path_has_zero_spherical_loss() {
        // 5 km, 30 m antennas: h_se ≫ h_req → L_dsph = 0 (eq. 25 branch).
        let l = spherical_db(AE, 0.868, 30.0, 30.0, 5.0, 0.0, Polarization::Vertical);
        assert_eq!(l, 0.0);
    }

    #[test]
    fn spherical_loss_grows_with_distance_beyond_horizon() {
        let l1 = spherical_db(AE, 0.868, 10.0, 10.0, 40.0, 0.0, Polarization::Vertical);
        let l2 = spherical_db(AE, 0.868, 10.0, 10.0, 80.0, 0.0, Polarization::Vertical);
        assert!(l2 > l1, "{l2} vs {l1}");
        assert!(l1 > 0.0);
    }

    #[test]
    fn ldp_interpolation_brackets() {
        let d = spaced(30.0, 31);
        let mut g = vec![0.0; 31];
        g[15] = 60.0;
        let path = DiffractionPath {
            d_km: &d,
            g_masl: &g,
            htc: 15.0,
            hrc: 15.0,
            hstd: 0.0,
            hsrd: 0.0,
            f_ghz: 0.868,
            omega: 0.0,
            pol: Polarization::Vertical,
        };
        let abeta = 3.0 * 6371.0;
        let beta0 = 1.1;
        let (ld50, ldp50) = ldp_db(&path, AE, abeta, 50.0, beta0);
        assert_eq!(ld50, ldp50);
        // p = β0 → F_i = 1 → L_dp = L_dβ exactly.
        let (ld50b, ldp_beta) = ldp_db(&path, AE, abeta, beta0, beta0);
        let ldbeta = delta_bullington_db(&path, abeta);
        assert!((ldp_beta - ldbeta).abs() < 1e-9);
        assert_eq!(ld50, ld50b);
        // β0 < p < 50 → strictly between the two anchors (larger radius ⇒
        // less curvature bulge ⇒ less loss, so L_dβ < L_d50 here).
        let (_, ldp10) = ldp_db(&path, AE, abeta, 10.0, beta0);
        let (lo, hi) = if ldbeta < ld50 { (ldbeta, ld50) } else { (ld50, ldbeta) };
        assert!(ldp10 >= lo - 1e-9 && ldp10 <= hi + 1e-9, "{lo} {ldp10} {hi}");
    }
}
