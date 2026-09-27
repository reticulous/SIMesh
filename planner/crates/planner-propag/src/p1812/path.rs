//! §3.5–3.8 + Attachment 1: radio-meteorological parameters and path-profile
//! analysis (eqs. 2–7 and 71–93).

use planner_core::profile::{Profile, Zone};

/// Average physical Earth radius a (Table 7).
pub const EARTH_RADIUS_KM: f64 = 6371.0;

/// Median effective Earth radius a_e = k50·a, k50 = 157/(157−ΔN) (eqs. 6, 7a).
pub fn median_effective_earth_radius_km(delta_n: f64) -> f64 {
    157.0 / (157.0 - delta_n) * EARTH_RADIUS_KM
}

/// Effective Earth radius exceeded for β0 time, a_β = k_β·a with k_β = 3.0
/// (eq. 7b).
pub fn beta_effective_earth_radius_km() -> f64 {
    3.0 * EARTH_RADIUS_KM
}

/// Incidence of ducting β0 (%) from path-centre latitude φ (deg) and the
/// longest continuous land / inland sections d_tm, d_lm (km) (eqs. 2–5).
pub fn beta0(phi_deg: f64, d_tm_km: f64, d_lm_km: f64) -> f64 {
    let tau = 1.0 - (-0.000412 * d_lm_km.powf(2.41)).exp(); // (3)
    let mu1 = (10f64.powf(-d_tm_km / (16.0 - 6.6 * tau))
        + 10f64.powf(-5.0 * (0.496 + 0.354 * tau)))
    .powf(0.2)
    .min(1.0); // (2)
    let abs_phi = phi_deg.abs();
    let mu4 = if abs_phi <= 70.0 {
        mu1.powf(-0.935 + 0.0176 * abs_phi) // (4)
    } else {
        mu1.powf(0.3)
    };
    if abs_phi <= 70.0 {
        10f64.powf(-0.015 * abs_phi + 1.67) * mu1 * mu4 // (5)
    } else {
        4.17 * mu1 * mu4
    }
}

/// Longest continuous land (inland + coastal) and inland-only sections of the
/// path, (d_tm, d_lm) in km (§3.6). Zone changes are taken to occur midway
/// between points with different zone codes (§3.3).
fn seg_bounds(profile: &Profile, i: usize) -> (f64, f64) {
    let pts = &profile.points;
    let start = if i == 0 { pts[0].d_m } else { (pts[i - 1].d_m + pts[i].d_m) / 2.0 };
    let end = if i + 1 == pts.len() {
        pts[pts.len() - 1].d_m
    } else {
        (pts[i].d_m + pts[i + 1].d_m) / 2.0
    };
    (start, end)
}

/// Fraction of the path over sea, ω (Table 5), by zone-segment length.
pub fn sea_fraction(profile: &Profile) -> f64 {
    let pts = &profile.points;
    let total = profile.length_m();
    if total <= 0.0 {
        return 0.0;
    }
    let sea: f64 = (0..pts.len())
        .filter(|&i| pts[i].zone == Zone::Sea)
        .map(|i| {
            let (s, e) = seg_bounds(profile, i);
            e - s
        })
        .sum();
    sea / total
}

/// Terminal-to-coast distances (d_ct, d_cr), §3.4: distance along the path
/// from each terminal to the first sea segment (path length when there is no
/// sea — the terms gated on these are then inactive anyway).
pub fn coast_distances_km(profile: &Profile) -> (f64, f64) {
    let pts = &profile.points;
    let d_total = profile.length_m();
    let mut dct = d_total;
    let mut dcr = d_total;
    for i in 0..pts.len() {
        if pts[i].zone == Zone::Sea {
            dct = seg_bounds(profile, i).0 - pts[0].d_m;
            break;
        }
    }
    for i in (0..pts.len()).rev() {
        if pts[i].zone == Zone::Sea {
            dcr = d_total - (seg_bounds(profile, i).1 - pts[0].d_m);
            break;
        }
    }
    (dct / 1000.0, dcr / 1000.0)
}

pub fn land_sections_km(profile: &Profile) -> (f64, f64) {
    let pts = &profile.points;
    if pts.is_empty() {
        return (0.0, 0.0);
    }
    let seg = |i: usize| seg_bounds(profile, i);
    let mut best_land = 0.0f64;
    let mut best_inland = 0.0f64;
    let mut run_land = 0.0f64;
    let mut run_inland = 0.0f64;
    for i in 0..pts.len() {
        let (s, e) = seg(i);
        let len = e - s;
        match pts[i].zone {
            Zone::Inland => {
                run_land += len;
                run_inland += len;
            }
            Zone::Coastal => {
                run_land += len;
                best_inland = best_inland.max(run_inland);
                run_inland = 0.0;
            }
            Zone::Sea => {
                best_land = best_land.max(run_land);
                best_inland = best_inland.max(run_inland);
                run_land = 0.0;
                run_inland = 0.0;
            }
        }
    }
    best_land = best_land.max(run_land);
    best_inland = best_inland.max(run_inland);
    (best_land / 1000.0, best_inland / 1000.0)
}

/// Inputs to the Attachment-1 analysis. `d_km`/`h_masl` are the profile
/// arrays (1a)/(1b); the Rec's Attachment 1 runs on TERRAIN heights h_i
/// (its §1: "a path profile of terrain heights ... is required").
/// NOTE for the oracle phase: some reference implementations are said to run
/// parts of this analysis on the clutter-modified g_i instead — the function
/// is array-agnostic on purpose; if oracle vectors disagree, switch the
/// caller's array, not this code.
pub struct PathInputs<'a> {
    pub d_km: &'a [f64],
    pub h_masl: &'a [f64],
    /// Antenna heights above ground (Table 1: h_tg, h_rg).
    pub htg_m: f64,
    pub hrg_m: f64,
    /// Median effective Earth radius (7a).
    pub ae_km: f64,
    /// Frequency (GHz) — used by the LoS horizon-point criterion (78a).
    pub f_ghz: f64,
}

/// Everything Table 5 / Table 7 requires from the path-profile analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct PathAnalysis {
    pub d_km: f64,
    /// Antenna centre heights amsl: h_ts = h_1 + h_tg, h_rs = h_n + h_rg.
    pub hts_masl: f64,
    pub hrs_masl: f64,
    /// Trans-horizon flag (eq. 73: θ_max > θ_td).
    pub trans_horizon: bool,
    /// Horizon elevation angles (mrad) and distances (km) (§5.1–5.4).
    pub theta_t_mrad: f64,
    pub theta_r_mrad: f64,
    pub dlt_km: f64,
    pub dlr_km: f64,
    /// Profile indices of the TX-side / RX-side horizon points.
    pub ilt: usize,
    pub ilr: usize,
    /// Path angular distance θ (mrad) (eq. 82).
    pub theta_mrad: f64,
    /// Smooth-Earth surface heights amsl at the terminals (eqs. 85–86).
    pub hst_masl: f64,
    pub hsr_masl: f64,
    /// Smooth-surface heights for the diffraction model (eqs. 88–89).
    pub hstd_masl: f64,
    pub hsrd_masl: f64,
    /// Effective antenna heights for the ducting model (eqs. 92a/92b) and the
    /// terrain roughness parameter (eq. 93).
    pub hte_m: f64,
    pub hre_m: f64,
    pub hm_m: f64,
}

/// Attachment-1 path-profile analysis (§§3–5, eqs. 71–93).
///
/// Panics if fewer than 3 points or arrays of unequal length (§3.2: n ≥ 3).
pub fn analyze(inp: &PathInputs) -> PathAnalysis {
    let d = inp.d_km;
    let h = inp.h_masl;
    let n = d.len();
    assert!(n >= 3, "P.1812 requires n >= 3 profile points");
    assert_eq!(n, h.len(), "d/h arrays must have equal length");

    let dtot = d[n - 1] - d[0]; // (71)
    let ae = inp.ae_km;
    let hts = h[0] + inp.htg_m;
    let hrs = h[n - 1] + inp.hrg_m;

    // --- Path classification (§4, eqs. 73–76) ---
    // The argument of (75), before the atan. Splitting it out is what lets the
    // argmax loop below skip the atan on almost every point: atan is monotone
    // non-decreasing, so `arg > arg_max` is a NECESSARY condition for
    // `θ_i > θ_max`, and the atan only has to be evaluated where the argument
    // reaches at least the INCUMBENT WINNER's — a handful per radial instead
    // of all n. (`arg_max` below is the argument of the point that currently
    // holds θ_max, not a running maximum of the argument: after an atan tie
    // the two differ, and the guard is then merely more permissive, never
    // less. Turning it into a true running max would reintroduce the defect
    // `first_max_survives_an_atan_tie` pins.)
    //
    // Why bother: an earlier profile of lb_from_arrays on real Berlin radials
    // (an independent review of the sweep; not re-measured in this change) put
    // path::analyze at 55–60% of it, with these two horizon-angle loops about
    // half of that again. Measured here by `atan_hoisting_timing` below (500
    // mixed profiles × 200 points, release build, best of seven on a loaded
    // workstation): 2.50 → 1.98 µs per analyze call, −21% (five repeats spread
    // −20.6% to −25.7%; the absolute figures move with the machine's load, the
    // ratio does not). The win is shape-dependent and the sweep deliberately
    // includes its own worst case: on a monotonically rising profile every
    // point is a new maximum and every atan still runs.
    //
    // The atan is NOT hoisted out of the comparison, only out of the points
    // that cannot win. Hoisting it out entirely — argmax of the bare argument,
    // one atan afterwards — is *not* bit-identical: two arguments differing by
    // a few ulp can round to the SAME f64 atan (already at |arg| ≈ 10, where
    // one ulp of the argument moves the atan by ~1/12 ulp of the result), and
    // there the strict '>' below must still keep the FIRST of them, because
    // that is the one that gives the minimum d_lt (§5.2).
    // `first_max_survives_an_atan_tie` pins exactly that case.
    let theta_arg = |i: usize| -> f64 { (h[i] - hts) / (1000.0 * d[i]) - d[i] / (2.0 * ae) };
    let mut arg_max = f64::NEG_INFINITY;
    let mut theta_max = f64::NEG_INFINITY;
    let mut i_theta_max = 1usize;
    for i in 1..n - 1 {
        let arg = theta_arg(i);
        // '>=', not '>': the sentinel is −∞ and an argument may legitimately
        // BE −∞ (d_i = 0), which the old loop still admitted because
        // atan(−∞) = −π/2 > −∞. A tie merely costs one extra atan.
        if arg >= arg_max {
            let th = 1000.0 * arg.atan(); // (75)
            if th > theta_max {
                theta_max = th; // (74); strict '>' keeps the FIRST max → minimum d_lt (§5.2)
                arg_max = arg;
                i_theta_max = i;
            }
        }
    }
    let theta_td = 1000.0 * ((hrs - hts) / (1000.0 * dtot) - dtot / (2.0 * ae)).atan(); // (76)
    let trans_horizon = theta_max > theta_td; // (73)

    // --- §5.1/5.2: θ_t, d_lt, i_lt ---
    let (theta_t, dlt, ilt) = if trans_horizon {
        (theta_max, d[i_theta_max], i_theta_max) // (77), (78)
    } else {
        // LoS: horizon point = max diffraction parameter ν (78a).
        let ce = 1.0 / ae;
        let lambda_m = 0.299_792_458 / inp.f_ghz;
        let mut nu_max = f64::NEG_INFINITY;
        let mut i_nu = 1usize;
        for i in 1..n - 1 {
            let di = d[i];
            let bulge = h[i] + 500.0 * ce * di * (dtot - di) - (hts * (dtot - di) + hrs * di) / dtot;
            let nu = bulge * (0.002 * dtot / (lambda_m * di * (dtot - di))).sqrt();
            if nu > nu_max {
                nu_max = nu;
                i_nu = i;
            }
        }
        (theta_td.max(theta_max), d[i_nu], i_nu) // θ_t = max(θ_max, θ_td) (77)
    };

    // --- §5.3/5.4: θ_r, d_lr, i_lr ---
    let (theta_r, dlr, ilr) = if trans_horizon {
        // Same hoist as (74) above, mirrored. The old loop ran forward with
        // '>=' to keep the LAST max (→ minimum distance from the receiver,
        // 81); walking the points in REVERSE with a strict '>' picks exactly
        // the same element, and only in that direction is `arg > arg_max` a
        // necessary condition for the update — forward, an equal atan reached
        // from a *smaller* argument still has to displace the incumbent.
        // `last_max_survives_an_atan_tie` pins that.
        let theta_arg_r = |j: usize| -> f64 {
            (h[j] - hrs) / (1000.0 * (dtot - d[j])) - (dtot - d[j]) / (2.0 * ae)
        };
        let mut arg_max_r = f64::NEG_INFINITY;
        let mut th_max = f64::NEG_INFINITY;
        let mut j_max = 1usize;
        for j in (1..n - 1).rev() {
            let arg = theta_arg_r(j);
            if arg >= arg_max_r {
                let th = 1000.0 * arg.atan(); // (80a)
                if th > th_max {
                    th_max = th;
                    arg_max_r = arg;
                    j_max = j;
                }
            }
        }
        (th_max, dtot - d[j_max], j_max) // (80), (81)
    } else {
        let th = 1000.0 * ((hts - hrs) / (1000.0 * dtot) - dtot / (2.0 * ae)).atan(); // (79)
        (th, dtot - dlt, ilt) // (81a)
    };

    let theta = 1000.0 * dtot / ae + theta_t + theta_r; // (82)

    // --- §5.6.1: smooth-Earth surface (least-squares, eqs. 83–86) ---
    let mut v1 = 0.0;
    let mut v2 = 0.0;
    for i in 1..n {
        let dd = d[i] - d[i - 1];
        v1 += dd * (h[i] + h[i - 1]); // (83)
        v2 += dd * (h[i] * (2.0 * d[i] + d[i - 1]) + h[i - 1] * (d[i] + 2.0 * d[i - 1])); // (84)
    }
    let hst = (2.0 * v1 * dtot - v2) / dtot.powi(2); // (85)
    let hsr = (v2 - v1 * dtot) / dtot.powi(2); // (86)

    // --- §5.6.2: smooth-surface heights for the diffraction model ---
    let (htc, hrc) = (hts, hrs); // Table 5: h_tc, h_rc are h_ts, h_rs
    let mut hobs = f64::NEG_INFINITY;
    let mut alpha_obt = f64::NEG_INFINITY;
    let mut alpha_obr = f64::NEG_INFINITY;
    for i in 1..n - 1 {
        let hi = h[i] - (htc * (dtot - d[i]) + hrc * d[i]) / dtot; // (87d)
        hobs = hobs.max(hi); // (87a)
        alpha_obt = alpha_obt.max(hi / d[i]); // (87b)
        alpha_obr = alpha_obr.max(hi / (dtot - d[i])); // (87c)
    }
    let (hstp, hsrp) = if hobs <= 0.0 {
        (hst, hsr) // (88a,b)
    } else {
        let gt = alpha_obt / (alpha_obt + alpha_obr); // (88e)
        let gr = alpha_obr / (alpha_obt + alpha_obr); // (88f)
        (hst - hobs * gt, hsr - hobs * gr) // (88c,d)
    };
    let hstd = if hstp > h[0] { h[0] } else { hstp }; // (89a,b)
    let hsrd = if hsrp > h[n - 1] { h[n - 1] } else { hsrp }; // (89c,d)

    // --- §5.6.3: ducting/layer-reflection parameters ---
    let hst_c = hst.min(h[0]); // (90a)
    let hsr_c = hsr.min(h[n - 1]); // (90b)
    let m = (hsr_c - hst_c) / dtot; // (91)
    let hte = inp.htg_m + h[0] - hst_c; // (92a)
    let hre = inp.hrg_m + h[n - 1] - hsr_c; // (92b)
    let (lo, hi) = if ilt <= ilr { (ilt, ilr) } else { (ilr, ilt) };
    let mut hm = f64::NEG_INFINITY;
    for i in lo..=hi {
        hm = hm.max(h[i] - (hst_c + m * d[i])); // (93)
    }

    PathAnalysis {
        d_km: dtot,
        hts_masl: hts,
        hrs_masl: hrs,
        trans_horizon,
        theta_t_mrad: theta_t,
        theta_r_mrad: theta_r,
        dlt_km: dlt,
        dlr_km: dlr,
        ilt,
        ilr,
        theta_mrad: theta,
        hst_masl: hst,
        hsr_masl: hsr,
        hstd_masl: hstd,
        hsrd_masl: hsrd,
        hte_m: hte,
        hre_m: hre,
        hm_m: hm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use planner_core::profile::{ClutterClass, ProfilePoint};

    fn equally_spaced(d_km: f64, h: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let n = h.len();
        let d: Vec<f64> = (0..n).map(|i| d_km * i as f64 / (n - 1) as f64).collect();
        (d, h.to_vec())
    }

    #[test]
    fn effective_earth_radius_median() {
        // ΔN = 45 → k50 = 157/112 ≈ 1.4018 → a_e ≈ 8930.6 km.
        let ae = median_effective_earth_radius_km(45.0);
        assert!((ae - 8930.6).abs() < 0.5, "got {ae}");
    }

    #[test]
    fn flat_path_is_los_and_symmetric() {
        let (d, h) = equally_spaced(10.0, &[0.0; 21]);
        let a = analyze(&PathInputs {
            d_km: &d,
            h_masl: &h,
            htg_m: 10.0,
            hrg_m: 10.0,
            ae_km: median_effective_earth_radius_km(45.0),
            f_ghz: 0.868,
        });
        assert!(!a.trans_horizon);
        // θ_td = 1000·atan(0 − 10/(2·8930.6)) ≈ −0.5599 mrad; θ_t = θ_r by symmetry.
        assert!((a.theta_t_mrad + 0.5599).abs() < 0.01, "{}", a.theta_t_mrad);
        assert!((a.theta_r_mrad - a.theta_t_mrad).abs() < 1e-9);
        // Smooth Earth through zero terrain.
        assert!(a.hst_masl.abs() < 1e-9 && a.hsr_masl.abs() < 1e-9);
        assert!((a.hte_m - 10.0).abs() < 1e-9 && (a.hre_m - 10.0).abs() < 1e-9);
        assert!(a.hm_m.abs() < 1e-9);
        // Horizon distances partition the path (81a).
        assert!((a.dlt_km + a.dlr_km - 10.0).abs() < 1e-9);
        assert!(a.dlt_km > 0.0 && a.dlr_km > 0.0);
    }

    #[test]
    fn knife_edge_is_trans_horizon() {
        let mut h = vec![0.0; 21];
        h[10] = 50.0; // spike at 5 km of a 10 km path
        let (d, h) = equally_spaced(10.0, &h);
        let ae = median_effective_earth_radius_km(45.0);
        let a = analyze(&PathInputs {
            d_km: &d,
            h_masl: &h,
            htg_m: 10.0,
            hrg_m: 10.0,
            ae_km: ae,
            f_ghz: 0.868,
        });
        assert!(a.trans_horizon);
        // θ_t = 1000·atan((50−10)/(1000·5) − 5/(2·a_e)) ≈ 7.719 mrad.
        let expect = 1000.0 * ((40.0 / 5000.0) - 5.0 / (2.0 * ae)).atan();
        assert!((a.theta_t_mrad - expect).abs() < 1e-6, "{}", a.theta_t_mrad);
        assert!((a.theta_r_mrad - expect).abs() < 1e-6);
        assert!((a.dlt_km - 5.0).abs() < 1e-9 && (a.dlr_km - 5.0).abs() < 1e-9);
        assert_eq!((a.ilt, a.ilr), (10, 10));
        // θ = 1000·d/a_e + θ_t + θ_r (82).
        let theta = 1000.0 * 10.0 / ae + 2.0 * expect;
        assert!((a.theta_mrad - theta).abs() < 1e-6);
        // h_m: eqs. (90a/90b) clamp the smooth surface to the terminal terrain
        // heights (0 here), so the roughness equals the full spike height.
        assert!((a.hm_m - 50.0).abs() < 1e-6, "{}", a.hm_m);
    }

    #[test]
    fn smooth_earth_fit_reproduces_a_ramp() {
        // h(d) linear 0 → 100: least-squares line equals the data.
        let n = 51;
        let h: Vec<f64> = (0..n).map(|i| 100.0 * i as f64 / (n - 1) as f64).collect();
        let (d, h) = equally_spaced(10.0, &h);
        let a = analyze(&PathInputs {
            d_km: &d,
            h_masl: &h,
            htg_m: 10.0,
            hrg_m: 10.0,
            ae_km: median_effective_earth_radius_km(45.0),
            f_ghz: 0.868,
        });
        assert!(a.hst_masl.abs() < 1e-6, "{}", a.hst_masl);
        assert!((a.hsr_masl - 100.0).abs() < 1e-6, "{}", a.hsr_masl);
        // Ducting effective heights collapse to the antenna heights on a ramp.
        assert!((a.hte_m - 10.0).abs() < 1e-6 && (a.hre_m - 10.0).abs() < 1e-6);
    }

    #[test]
    fn beta0_berlin_all_inland() {
        // Hand-computed for φ=52.5°, d_tm=d_lm=50 km: β0 ≈ 1.1% (see review
        // notes); pin the hand-calculation band.
        let b = beta0(52.5, 50.0, 50.0);
        assert!(b > 1.0 && b < 1.3, "{b}");
    }

    #[test]
    fn land_sections_with_sea_gap() {
        let mk = |d_m: f64, zone| ProfilePoint {
            d_m,
            h_terrain_m: 0.0,
            h_clutter_m: 0.0,
            clutter: ClutterClass::Open,
            zone,
        };
        // 0–4 km inland, 4–6 km sea, 6–10 km coastal (boundaries at segment
        // midpoints per §3.3).
        let pts: Vec<_> = (0..=10)
            .map(|i| {
                let d = i as f64 * 1000.0;
                let zone = match i {
                    0..=3 => Zone::Inland,
                    4..=5 => Zone::Sea,
                    _ => Zone::Coastal,
                };
                mk(d, zone)
            })
            .collect();
        let profile = Profile { points: pts };
        let (d_tm, d_lm) = land_sections_km(&profile);
        // Longest land run: coastal tail 5.5–10 km = 4.5 km; inland head = 3.5 km.
        assert!((d_tm - 4.5).abs() < 1e-9, "{d_tm}");
        assert!((d_lm - 3.5).abs() < 1e-9, "{d_lm}");
    }

    // ---- eqs. (74)/(75) and (80a): the hoisted atan must change nothing ----

    /// `analyze` as it was before the atan hoist: one `atan` per intermediate
    /// point, forward, `>` for (74) and `>=` for (80a). Everything else is
    /// character-for-character the current function, so a field-by-field
    /// comparison isolates the hoist and nothing else.
    fn analyze_atan_per_point(inp: &PathInputs) -> PathAnalysis {
        let d = inp.d_km;
        let h = inp.h_masl;
        let n = d.len();
        assert!(n >= 3, "P.1812 requires n >= 3 profile points");
        assert_eq!(n, h.len(), "d/h arrays must have equal length");

        let dtot = d[n - 1] - d[0]; // (71)
        let ae = inp.ae_km;
        let hts = h[0] + inp.htg_m;
        let hrs = h[n - 1] + inp.hrg_m;

        let theta_i = |i: usize| -> f64 {
            1000.0 * ((h[i] - hts) / (1000.0 * d[i]) - d[i] / (2.0 * ae)).atan() // (75)
        };
        let mut theta_max = f64::NEG_INFINITY;
        let mut i_theta_max = 1usize;
        for i in 1..n - 1 {
            let th = theta_i(i);
            if th > theta_max {
                theta_max = th; // (74); strict '>' keeps the FIRST max
                i_theta_max = i;
            }
        }
        let theta_td = 1000.0 * ((hrs - hts) / (1000.0 * dtot) - dtot / (2.0 * ae)).atan(); // (76)
        let trans_horizon = theta_max > theta_td; // (73)

        let (theta_t, dlt, ilt) = if trans_horizon {
            (theta_max, d[i_theta_max], i_theta_max) // (77), (78)
        } else {
            let ce = 1.0 / ae;
            let lambda_m = 0.299_792_458 / inp.f_ghz;
            let mut nu_max = f64::NEG_INFINITY;
            let mut i_nu = 1usize;
            for i in 1..n - 1 {
                let di = d[i];
                let bulge =
                    h[i] + 500.0 * ce * di * (dtot - di) - (hts * (dtot - di) + hrs * di) / dtot;
                let nu = bulge * (0.002 * dtot / (lambda_m * di * (dtot - di))).sqrt();
                if nu > nu_max {
                    nu_max = nu;
                    i_nu = i;
                }
            }
            (theta_td.max(theta_max), d[i_nu], i_nu) // (77)
        };

        let (theta_r, dlr, ilr) = if trans_horizon {
            let theta_j = |j: usize| -> f64 {
                1000.0
                    * ((h[j] - hrs) / (1000.0 * (dtot - d[j])) - (dtot - d[j]) / (2.0 * ae)).atan()
                // (80a)
            };
            let mut th_max = f64::NEG_INFINITY;
            let mut j_max = 1usize;
            for j in 1..n - 1 {
                let th = theta_j(j);
                // '>=' keeps the LAST max → minimum distance from the receiver (81).
                if th >= th_max {
                    th_max = th;
                    j_max = j;
                }
            }
            (th_max, dtot - d[j_max], j_max) // (80), (81)
        } else {
            let th = 1000.0 * ((hts - hrs) / (1000.0 * dtot) - dtot / (2.0 * ae)).atan(); // (79)
            (th, dtot - dlt, ilt) // (81a)
        };

        let theta = 1000.0 * dtot / ae + theta_t + theta_r; // (82)

        let mut v1 = 0.0;
        let mut v2 = 0.0;
        for i in 1..n {
            let dd = d[i] - d[i - 1];
            v1 += dd * (h[i] + h[i - 1]); // (83)
            v2 += dd * (h[i] * (2.0 * d[i] + d[i - 1]) + h[i - 1] * (d[i] + 2.0 * d[i - 1])); // (84)
        }
        let hst = (2.0 * v1 * dtot - v2) / dtot.powi(2); // (85)
        let hsr = (v2 - v1 * dtot) / dtot.powi(2); // (86)

        let (htc, hrc) = (hts, hrs);
        let mut hobs = f64::NEG_INFINITY;
        let mut alpha_obt = f64::NEG_INFINITY;
        let mut alpha_obr = f64::NEG_INFINITY;
        for i in 1..n - 1 {
            let hi = h[i] - (htc * (dtot - d[i]) + hrc * d[i]) / dtot; // (87d)
            hobs = hobs.max(hi); // (87a)
            alpha_obt = alpha_obt.max(hi / d[i]); // (87b)
            alpha_obr = alpha_obr.max(hi / (dtot - d[i])); // (87c)
        }
        let (hstp, hsrp) = if hobs <= 0.0 {
            (hst, hsr) // (88a,b)
        } else {
            let gt = alpha_obt / (alpha_obt + alpha_obr); // (88e)
            let gr = alpha_obr / (alpha_obt + alpha_obr); // (88f)
            (hst - hobs * gt, hsr - hobs * gr) // (88c,d)
        };
        let hstd = if hstp > h[0] { h[0] } else { hstp }; // (89a,b)
        let hsrd = if hsrp > h[n - 1] { h[n - 1] } else { hsrp }; // (89c,d)

        let hst_c = hst.min(h[0]); // (90a)
        let hsr_c = hsr.min(h[n - 1]); // (90b)
        let m = (hsr_c - hst_c) / dtot; // (91)
        let hte = inp.htg_m + h[0] - hst_c; // (92a)
        let hre = inp.hrg_m + h[n - 1] - hsr_c; // (92b)
        let (lo, hi) = if ilt <= ilr { (ilt, ilr) } else { (ilr, ilt) };
        let mut hm = f64::NEG_INFINITY;
        for i in lo..=hi {
            hm = hm.max(h[i] - (hst_c + m * d[i])); // (93)
        }

        PathAnalysis {
            d_km: dtot,
            hts_masl: hts,
            hrs_masl: hrs,
            trans_horizon,
            theta_t_mrad: theta_t,
            theta_r_mrad: theta_r,
            dlt_km: dlt,
            dlr_km: dlr,
            ilt,
            ilr,
            theta_mrad: theta,
            hst_masl: hst,
            hsr_masl: hsr,
            hstd_masl: hstd,
            hsrd_masl: hsrd,
            hte_m: hte,
            hre_m: hre,
            hm_m: hm,
        }
    }

    /// Deterministic LCG. The property test wants many path *shapes*, not good
    /// randomness, and a fixed stream keeps any failure reproducible.
    struct Lcg(u64);

    impl Lcg {
        fn unit(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Shapes that stress the horizon-angle argmax differently: the number of
    /// running maxima — i.e. the number of atans the hoisted loop still has to
    /// evaluate — is 1 for a descending ramp and up to n for an ascending one,
    /// so both the best and the worst case for the hoist are in the sweep.
    fn synth_profile(rng: &mut Lcg, kind: usize, n: usize, len_km: f64) -> (Vec<f64>, Vec<f64>) {
        let last = (n - 1) as f64;
        let d: Vec<f64> = if kind % 3 == 1 {
            // Jittered spacing: a radial crossing a raster at an angle.
            let mut acc = 0.0;
            (0..n)
                .map(|_| {
                    let cur = acc;
                    acc += len_km / last * (0.5 + rng.unit());
                    cur
                })
                .collect()
        } else {
            (0..n).map(|i| len_km * i as f64 / last).collect()
        };
        let h: Vec<f64> = (0..n)
            .map(|i| {
                let x = i as f64 / last;
                match kind % 8 {
                    0 => 35.0,                                             // flat
                    1 => 300.0 * x,                                        // ascending ramp
                    2 => 300.0 * (1.0 - x),                                // descending ramp
                    3 => if i == n / 2 { 120.0 } else { 5.0 },             // single knife edge
                    4 => if i % 2 == 0 { 0.0 } else { 25.0 },              // urban sawtooth
                    5 => if (0.3..0.7).contains(&x) { 60.0 } else { 10.0 }, // plateau: equal heights
                    6 => 40.0 + 30.0 * (7.0 * x).sin() + 12.0 * (23.0 * x).cos(), // rolling hills
                    _ => 20.0 + 40.0 * rng.unit(),                         // noise
                }
            })
            .collect();
        (d, h)
    }

    /// Exact, field-by-field. Not an epsilon: the hoisted loops evaluate the
    /// *same* f64 expression at the *same* index, so anything other than bit
    /// equality is a bug, and an epsilon would hide the one failure mode that
    /// matters (a different horizon point, which moves d_lt by kilometres).
    fn assert_bit_identical(new: &PathAnalysis, old: &PathAnalysis, what: &str) {
        assert_eq!(new.ilt, old.ilt, "{what}: i_lt");
        assert_eq!(new.ilr, old.ilr, "{what}: i_lr");
        for (label, a, b) in [
            ("theta_t", new.theta_t_mrad, old.theta_t_mrad),
            ("theta_r", new.theta_r_mrad, old.theta_r_mrad),
            ("dlt", new.dlt_km, old.dlt_km),
            ("dlr", new.dlr_km, old.dlr_km),
            ("theta", new.theta_mrad, old.theta_mrad),
            ("hm", new.hm_m, old.hm_m),
        ] {
            assert_eq!(a.to_bits(), b.to_bits(), "{what}: {label} {a} vs {b}");
        }
        // Every remaining field by name -- never a PartialEq of the whole
        // struct, which a NaN-bearing analysis would fail even when the two
        // are bit-identical.
        assert_eq!(new.d_km.to_bits(), old.d_km.to_bits(), "{what}: d_km");
        assert_eq!(new.hts_masl.to_bits(), old.hts_masl.to_bits(), "{what}: hts_masl");
        assert_eq!(new.hrs_masl.to_bits(), old.hrs_masl.to_bits(), "{what}: hrs_masl");
        assert_eq!(new.trans_horizon, old.trans_horizon, "{what}: trans_horizon");
        assert_eq!(new.hst_masl.to_bits(), old.hst_masl.to_bits(), "{what}: hst_masl");
        assert_eq!(new.hsr_masl.to_bits(), old.hsr_masl.to_bits(), "{what}: hsr_masl");
        assert_eq!(new.hstd_masl.to_bits(), old.hstd_masl.to_bits(), "{what}: hstd_masl");
        assert_eq!(new.hsrd_masl.to_bits(), old.hsrd_masl.to_bits(), "{what}: hsrd_masl");
        assert_eq!(new.hte_m.to_bits(), old.hte_m.to_bits(), "{what}: hte_m");
        assert_eq!(new.hre_m.to_bits(), old.hre_m.to_bits(), "{what}: hre_m");
    }

    #[test]
    fn atan_hoisting_is_bit_identical_on_synthetic_profiles() {
        let mut rng = Lcg(0x1812_0008);
        let ae = median_effective_earth_radius_km(45.0);
        let mut cases = 0usize;
        let mut trans = 0usize;
        for kind in 0..24 {
            for &n in &[3usize, 4, 7, 33, 129, 200] {
                for &len in &[0.3f64, 2.0, 17.0, 140.0] {
                    for &(htg, hrg) in &[(2.0, 2.0), (30.0, 1.5), (1.5, 300.0)] {
                        let (d, h) = synth_profile(&mut rng, kind, n, len);
                        let inp = PathInputs {
                            d_km: &d,
                            h_masl: &h,
                            htg_m: htg,
                            hrg_m: hrg,
                            ae_km: ae,
                            f_ghz: 0.868,
                        };
                        let new = analyze(&inp);
                        let old = analyze_atan_per_point(&inp);
                        assert_bit_identical(&new, &old, &format!("kind {kind} n {n} len {len}"));
                        assert!(new.theta_mrad.is_finite(), "degenerate profile in the sweep");
                        cases += 1;
                        trans += usize::from(new.trans_horizon);
                    }
                }
            }
        }
        assert_eq!(cases, 1728);
        // Both branches of (73) must actually be walked — the (80a) loop only
        // runs on trans-horizon paths, so an all-LoS sweep would prove nothing
        // about it.
        assert!(trans > 100 && trans < cases - 100, "{trans}/{cases} trans-horizon");
    }

    /// An intermediate point AT the transmitter (d_i = 0) makes the bare
    /// argument −∞. The old loop still admitted it, because atan(−∞) = −π/2 is
    /// greater than the −∞ sentinel; the guard's '>=' is what keeps that true
    /// here. With '>' the point would be skipped and, on a profile where it is
    /// the only intermediate point, the horizon index would differ.
    #[test]
    fn a_zero_distance_intermediate_point_is_still_admitted() {
        let ae = median_effective_earth_radius_km(45.0);
        let d = [0.0, 0.0, 0.5, 1.0];
        let h = [30.0, 45.0, 31.0, 30.0];
        let inp = PathInputs { d_km: &d, h_masl: &h, htg_m: 10.0, hrg_m: 2.0, ae_km: ae, f_ghz: 0.868 };
        assert_bit_identical(&analyze(&inp), &analyze_atan_per_point(&inp), "d_1 = 0");
        let d = [0.0, 0.0, 1.0];
        let h = [30.0, 45.0, 30.0];
        let inp = PathInputs { d_km: &d, h_masl: &h, htg_m: 10.0, hrg_m: 2.0, ae_km: ae, f_ghz: 0.868 };
        assert_bit_identical(&analyze(&inp), &analyze_atan_per_point(&inp), "d_1 = 0, n = 3");
    }

    /// The defect a *fully* hoisted atan would introduce at (74): argmax over
    /// the bare argument picks the later point here, because its argument is a
    /// few ulp larger — even though both arguments round to the same f64
    /// angle, where the strict '>' must keep the FIRST (minimum d_lt, §5.2).
    #[test]
    fn first_max_survives_an_atan_tie() {
        let ae = median_effective_earth_radius_km(45.0);
        let n = 6;
        // 1 m spacing at the transmitter puts (75)'s argument near 100, where
        // d(atan)/dx ≈ 1e-4 and one ulp of the argument is far below one ulp
        // of its arctangent — so distinct arguments collide after the atan.
        let d: Vec<f64> = (0..n).map(|i| i as f64 * 1e-3).collect();
        let mut h = vec![0.0f64; n];
        let htg = 10.0;
        let hts = h[0] + htg;
        let arg = |i: usize, hi: f64| (hi - hts) / (1000.0 * d[i]) - d[i] / (2.0 * ae);
        h[1] = hts + 100.0;
        let a1 = arg(1, h[1]);
        // Walk point 2's argument up one ulp at a time until it is strictly
        // larger than point 1's yet bit-identical after the atan.
        let mut found = false;
        for k in 1..4096u64 {
            let target = f64::from_bits(a1.to_bits() + k);
            let h2 = hts + (target + d[2] / (2.0 * ae)) * (1000.0 * d[2]);
            let a2 = arg(2, h2);
            if a2 > a1
                && (1000.0 * a2.atan()).to_bits() == (1000.0 * a1.atan()).to_bits()
            {
                h[2] = h2;
                found = true;
                break;
            }
        }
        assert!(found, "could not build an atan tie near |arg| = 100");
        let a2 = arg(2, h[2]);
        assert!(a2 > a1, "precondition: a bare-argument argmax would pick point 2");

        let inp = PathInputs {
            d_km: &d,
            h_masl: &h,
            htg_m: htg,
            hrg_m: 10.0,
            ae_km: ae,
            f_ghz: 0.868,
        };
        let a = analyze(&inp);
        assert!(a.trans_horizon, "the tie only reaches (78) on a trans-horizon path");
        assert_eq!(a.ilt, 1, "the FIRST of two equal angles must win");
        assert_eq!(a.dlt_km.to_bits(), d[1].to_bits());
        assert_eq!(a.theta_t_mrad.to_bits(), (1000.0 * a1.atan()).to_bits());
        assert_bit_identical(&a, &analyze_atan_per_point(&inp), "atan tie at (74)");
    }

    /// The mirror defect at (80a): the later point's argument is *smaller*,
    /// yet its angle is bit-identical, and eq. (81)'s `>=` must still move the
    /// horizon to it (minimum distance from the receiver). A forward argmax
    /// over the bare argument keeps the earlier point instead; the production
    /// loop walks the points in reverse precisely so that it does not.
    #[test]
    fn last_max_survives_an_atan_tie() {
        let ae = median_effective_earth_radius_km(45.0);
        let n = 6;
        let d: Vec<f64> = (0..n).map(|i| i as f64 * 1e-3).collect();
        let dtot = d[n - 1] - d[0];
        let mut h = vec![0.0f64; n];
        let hrg = 10.0;
        let hrs = h[n - 1] + hrg;
        let arg = |j: usize, hj: f64| {
            (hj - hrs) / (1000.0 * (dtot - d[j])) - (dtot - d[j]) / (2.0 * ae)
        };
        let (q, p) = (n - 3, n - 2); // q is nearer the transmitter, p nearer the receiver
        h[q] = hrs + 100.0 * (1000.0 * (dtot - d[q]));
        let aq = arg(q, h[q]);
        let mut found = false;
        for k in 1..4096u64 {
            let target = f64::from_bits(aq.to_bits() - k);
            let hp = hrs + (target + (dtot - d[p]) / (2.0 * ae)) * (1000.0 * (dtot - d[p]));
            let ap = arg(p, hp);
            if ap < aq
                && (1000.0 * ap.atan()).to_bits() == (1000.0 * aq.atan()).to_bits()
            {
                h[p] = hp;
                found = true;
                break;
            }
        }
        assert!(found, "could not build an atan tie near |arg| = 100");
        assert!(arg(p, h[p]) < aq, "precondition: a bare-argument argmax would pick point {q}");

        let inp = PathInputs {
            d_km: &d,
            h_masl: &h,
            htg_m: 10.0,
            hrg_m: hrg,
            ae_km: ae,
            f_ghz: 0.868,
        };
        let a = analyze(&inp);
        assert!(a.trans_horizon, "(80a) only runs on a trans-horizon path");
        assert_eq!(a.ilr, p, "the LAST of two equal angles must win");
        assert_eq!(a.dlr_km.to_bits(), (dtot - d[p]).to_bits());
        assert_eq!(a.theta_r_mrad.to_bits(), (1000.0 * aq.atan()).to_bits());
        assert_bit_identical(&a, &analyze_atan_per_point(&inp), "atan tie at (80a)");
    }

    /// The same equivalence over the real Py1812 validation profiles. The
    /// vectors derive from the non-redistributable ITU maps and are
    /// gitignored, so this skips with a notice when they are absent, exactly
    /// as `tests/oracle.rs` does — a green run without them has checked the
    /// synthetic profiles only.
    #[test]
    fn atan_hoisting_is_bit_identical_on_oracle_vectors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/oracle/vectors/vectors.jsonl");
        let Ok(data) = std::fs::read_to_string(&path) else {
            eprintln!("SKIP: no oracle vectors at {} — synthetic profiles only", path.display());
            return;
        };
        let mut checked = 0usize;
        let mut trans = 0usize;
        for line in data.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line).expect("vector json");
            let num = |k: &str| v[k].as_f64().unwrap_or_else(|| panic!("field {k}"));
            let arr = |k: &str| -> Vec<f64> {
                v[k].as_array()
                    .unwrap_or_else(|| panic!("field {k}"))
                    .iter()
                    .map(|x| x.as_f64().expect("f64 element"))
                    .collect()
            };
            let d = arr("d_km");
            let h = arr("h");
            if d.len() < 3 {
                continue;
            }
            let inp = PathInputs {
                d_km: &d,
                h_masl: &h,
                htg_m: num("htg"),
                hrg_m: num("hrg"),
                ae_km: median_effective_earth_radius_km(num("dn")),
                f_ghz: num("f_ghz"),
            };
            let new = analyze(&inp);
            assert_bit_identical(&new, &analyze_atan_per_point(&inp), &v["name"].to_string());
            checked += 1;
            trans += usize::from(new.trans_horizon);
        }
        eprintln!("atan hoist: {checked} oracle profiles bit-identical ({trans} trans-horizon)");
        assert!(checked > 0, "vectors file present but held no usable profile");
    }

    /// Not a correctness test — this is where the figure quoted in `analyze`'s
    /// comment comes from. Run it with:
    ///   cargo test -p planner-propag --release -- --ignored --nocapture
    #[test]
    #[ignore = "timing only; run with --release --ignored --nocapture"]
    fn atan_hoisting_timing() {
        use std::hint::black_box;
        use std::time::Instant;

        let ae = median_effective_earth_radius_km(45.0);
        let mut rng = Lcg(0x00B3_1812);
        // 200 points over 5 km is the shape of a Berlin radial at 25 m spacing.
        let profiles: Vec<(Vec<f64>, Vec<f64>)> =
            (0..500).map(|k| synth_profile(&mut rng, k, 200, 5.0)).collect();
        let time = |f: &dyn Fn(&PathInputs) -> PathAnalysis| -> (f64, f64) {
            let t0 = Instant::now();
            let mut acc = 0.0;
            for _ in 0..40 {
                for (d, h) in &profiles {
                    let a = f(black_box(&PathInputs {
                        d_km: d,
                        h_masl: h,
                        htg_m: 30.0,
                        hrg_m: 1.5,
                        ae_km: ae,
                        f_ghz: 0.868,
                    }));
                    acc += a.theta_mrad + a.ilt as f64 + a.ilr as f64;
                }
            }
            (t0.elapsed().as_secs_f64() * 1e6 / (40 * profiles.len()) as f64, acc)
        };
        let new = &|i: &PathInputs| analyze(i);
        let old = &|i: &PathInputs| analyze_atan_per_point(i);
        time(new); // warm-up: first pass pays page faults and branch-predictor training
        time(old);
        // Best of seven each, alternating, so drift and other load on the
        // machine hit both implementations equally.
        let mut best_new = f64::INFINITY;
        let mut best_old = f64::INFINITY;
        let mut checksum = (0.0, 0.0);
        for _ in 0..7 {
            let (t, a) = time(old);
            best_old = best_old.min(t);
            checksum.0 = a;
            let (t, a) = time(new);
            best_new = best_new.min(t);
            checksum.1 = a;
        }
        assert_eq!(checksum.0.to_bits(), checksum.1.to_bits(), "the two loops disagree");
        eprintln!(
            "analyze: atan per point {best_old:.3} µs/call → hoisted {best_new:.3} µs/call \
             ({:+.1}%)",
            100.0 * (best_new - best_old) / best_old
        );
    }
}
