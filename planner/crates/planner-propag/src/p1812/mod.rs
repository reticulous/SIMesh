//! ITU-R P.1812-8 (09/2025) — the v1 propagation model (decided 2026-08-30,
//! review §8). Implemented from the Recommendation text (local reference:
//! planner/refs/P1812-8.pdf, gitignored). Validation: black-box against
//! eeveetza/Py1812 — see tests/oracle/README.md. Equation numbers in comments
//! refer to the Rec.
//!
//! Module ← Rec map:
//! - `path`        ← §3.5–3.8 + Attachment 1 (eqs. 2–7, 71–93)
//! - `los`         ← §4.2 (eqs. 8–11)
//! - `diffraction` ← §4.3 (eqs. 12–43) delta-Bullington
//! - `tropo`       ← §4.4 (eqs. 44–45), `ducting` ← §4.5 (eqs. 46–56a)
//! - `combine`     ← §4.6 (eqs. 57–63)
//! - `location`    ← §4.7–4.9 (eqs. 64–69)
//! - `math`        ← Attachment 2 (eqs. 94–95) + J(ν) (eq. 12)
//!
//! Profile-height convention (§3.2): d_i (km), h_i terrain amsl, g_i = h_i +
//! representative clutter height except g_1 = h_1, g_n = h_n (eq. 1d).
//! `planner_core::Profile` carries terrain and clutter separately; this
//! adapter builds the h and g views. Attachment-1 analysis runs on terrain
//! heights per the Rec text — flagged for the oracle in `path.rs`.
//!
//! §3.2 also attaches a MINIMUM PROFILE SPACING to each of the two ways of
//! building g, and that floor is load bearing rather than advisory — see
//! `SurfaceMethod` and the spacing check in `lb_from_arrays`.

pub mod combine;
pub mod diffraction;
pub mod ducting;
pub mod location;
pub mod los;
pub mod math;
pub mod path;

use planner_core::model::{LinkParams, Loss, ModelError, PathLossModel};
use planner_core::profile::Profile;

pub struct P1812;

/// Which §3.2 method built the surface-height array `g_masl`.
///
/// This is not bookkeeping. Each method carries its own minimum profile-point
/// spacing, and that spacing is the only thing bounding how much influence a
/// single profile point can have on the answer: eqs. (13) and (17) divide by
/// d_i and (d − d_i), so a point sampled close to a terminal has an unbounded
/// derivative with respect to that terminal's antenna height. The floor is
/// enforced in `lb_from_arrays`; the comment there records what it costs when
/// it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceMethod {
    /// §3.2.1 (eq. 1d): terrain heights plus a representative clutter height
    /// per clutter class. The Rec: "When using this method the profile
    /// spacing should be no less than the order of 30 m."
    RepresentativeClutter,
    /// §3.2.2 (eq. 1e): surface heights taken directly from surface-height
    /// data. The Rec warns that spacing below the order of 10 m starts to
    /// describe individual obstacles and over-estimates the loss (and that
    /// going above the order of 50 m buys nothing over §3.2.1).
    SurfaceHeights,
}

impl SurfaceMethod {
    /// Minimum profile-point spacing (m) §3.2 attaches to this method.
    pub const fn min_spacing_m(self) -> f64 {
        match self {
            SurfaceMethod::RepresentativeClutter => 30.0,
            SurfaceMethod::SurfaceHeights => 10.0,
        }
    }

    /// Whole-sample stride that lifts a path sampled every `sample_step_m`
    /// onto this method's floor.
    ///
    /// Callers that must walk a raster at its own cell size — the coverage
    /// radial sweep, whose step size also sets the output resolution — keep
    /// every sample for the raster and decimate by this stride for the model.
    /// Callers free to choose their own step (`link.json`, the backbone pass)
    /// just take `sample_step_m.max(method.min_spacing_m())`.
    pub fn stride_for(self, sample_step_m: f64) -> usize {
        if sample_step_m <= 0.0 {
            return 1;
        }
        ((self.min_spacing_m() / sample_step_m).ceil() as usize).max(1)
    }
}

/// Array-level inputs for one loss computation — the allocation-free entry
/// point the coverage radial sweep uses with PREFIX SLICES of per-radial
/// arrays.
///
/// The terminal entries of `g_masl` are never read: §4.3.1's eqs. (13), (15)
/// and (17) all range over the intermediate points 2..n−1 only, and
/// Attachment 1 runs on `h_masl`. That makes eq. (1d)'s g_1 = h_1, g_n = h_n
/// inert, and callers that add clutter at the terminals get the same answer.
/// It is NOT licence to skip the rest of the §3.2 profile contract: the
/// spacing floor below is what actually decides whether the profile means
/// anything, and reading the inertness of eq. (1d) as "§3.2 is handled
/// downstream" is how the 5 m-spaced profile in `link.json` got shipped.
pub struct ArrayInputs<'a> {
    pub d_km: &'a [f64],
    pub h_masl: &'a [f64],
    pub g_masl: &'a [f64],
    /// How `g_masl` was built (§3.2.1 or §3.2.2) — sets the spacing floor.
    pub surface_method: SurfaceMethod,
    /// Zone-derived scalars (Table 5 / §3.4–3.6). For an all-inland path:
    /// omega 0, dct = dcr = d, d_tm = d_lm = d.
    pub omega: f64,
    pub dct_km: f64,
    pub dcr_km: f64,
    pub d_tm_km: f64,
    pub d_lm_km: f64,
    /// Representative clutter height at the receiver location (u(h), eq. 65).
    pub r_rx_m: f64,
}

/// Full P.1812-8 basic transmission loss from arrays (§4.2–4.9).
pub fn lb_from_arrays(x: &ArrayInputs, params: &LinkParams) -> Result<Loss, ModelError> {
    let n = x.d_km.len();
    if n < 3 {
        return Err(ModelError::BadProfile(
            "P.1812 needs at least 3 profile points (§3.2)".into(),
        ));
    }
    if n != x.h_masl.len() || n != x.g_masl.len() {
        return Err(ModelError::BadProfile("d/h/g array lengths differ".into()));
    }
    let f_ghz = params.freq_mhz / 1000.0;
    if !(0.03..=6.0).contains(&f_ghz) {
        return Err(ModelError::OutOfRange(format!(
            "frequency {} MHz outside P.1812's 30 MHz–6 GHz",
            params.freq_mhz
        )));
    }
    if !(1.0..=50.0).contains(&params.time_pct) {
        return Err(ModelError::OutOfRange("time_pct must be in [1, 50]".into()));
    }
    if !(1.0..=99.0).contains(&params.loc_pct) {
        return Err(ModelError::OutOfRange("loc_pct must be in [1, 99]".into()));
    }
    if !(1.0..=3000.0).contains(&params.tx_h_agl_m)
        || !(1.0..=3000.0).contains(&params.rx_h_agl_m)
    {
        return Err(ModelError::OutOfRange(
            "antenna heights must be in [1, 3000] m (Table 1)".into(),
        ));
    }
    let d_total = x.d_km[n - 1] - x.d_km[0];
    if !(0.25..=3000.0).contains(&d_total) {
        return Err(ModelError::OutOfRange(format!(
            "path length {d_total:.3} km outside P.1812's 0.25–3000 km"
        )));
    }
    // §3.2 profile-spacing floor. This is the input contract, not a style
    // preference, and it is checked here because no caller can be trusted to
    // remember it — one of them did not, and the result was a propagation
    // model that reported 59 dB of extra loss per metre of mast.
    //
    // Eq. (13) maximises s_i = (g_i + 500·C_e·d_i·(d − d_i) − h_tc)/d_i over
    // the INTERMEDIATE points, and eq. (17) mirrors it with 1/(d − d_i) at the
    // receiver. So ∂s_i/∂h_tc = −1/d_i: a profile point's grip on the answer
    // grows without bound as it approaches a terminal. Sampling a 10 m raster
    // at half a cell puts the first intermediate point 5 m from the mast —
    // where it is still a bilinear re-read of the transmitter's OWN clutter
    // cell — and gives that one sample 200 (m/km) of slope per metre of
    // antenna height, sixty times what a genuine obstacle 300 m down the path
    // gets. It wins eq. (13) until the antenna climbs past its own roof, drags
    // S_tim through d_bp (18) and ν_b (19) into L_bulla (21), and because
    // L_bulls and L_dsph are both zero on a short urban path, eq. (39) hands
    // the whole swing straight to L_b. Measured on a 1.155 km Berlin link at
    // 5 m spacing: −9 dB/m of mast at 14.6 m, steepening to −98 dB/m at
    // 15.00 m, then flat 2 cm later — a cliff keyed on the antenna height
    // alone, at every azimuth, because every radial leaves the site through
    // the same clutter cell.
    //
    // The Recommendation bounds 1/d_i by bounding the spacing (§3.2.1: no less
    // than the order of 30 m for the representative-clutter method; §3.2.2:
    // below the order of 10 m over-estimates the loss for surface data).
    // Refuse rather than quietly resample: the caller owns the profile, and a
    // resample hidden in here would be invisible in a number an operator acts
    // on. The 1 µm slack is float noise from d_m/1000 round-trips only.
    let mut min_gap_m = f64::INFINITY;
    for i in 1..n {
        min_gap_m = min_gap_m.min((x.d_km[i] - x.d_km[i - 1]) * 1000.0);
    }
    let floor_m = x.surface_method.min_spacing_m();
    if min_gap_m + 1e-6 < floor_m {
        return Err(ModelError::BadProfile(format!(
            "profile spacing {min_gap_m:.2} m is below the {floor_m:.0} m floor \
             §3.2 sets for this surface-profile method"
        )));
    }

    let (d_km, h, g) = (x.d_km, x.h_masl, x.g_masl);
    let ae = path::median_effective_earth_radius_km(params.delta_n);
    let abeta = path::beta_effective_earth_radius_km();
    let (d_tm, d_lm) = (x.d_tm_km, x.d_lm_km);
    let omega = x.omega;
    let (dct, dcr) = (x.dct_km, x.dcr_km);
    let beta0 = path::beta0(params.path_center_lat_deg, d_tm, d_lm);

    // Attachment-1 path analysis (terrain heights).
    let pa = path::analyze(&path::PathInputs {
        d_km,
        h_masl: h,
        htg_m: params.tx_h_agl_m,
        hrg_m: params.rx_h_agl_m,
        ae_km: ae,
        f_ghz,
    });

    // §4.2 notional LoS losses.
    let lbfs = los::lbfs_db(f_ghz, d_total, pa.hts_masl, pa.hrs_masl);
    let lb0p = lbfs + los::esp_db(pa.dlt_km, pa.dlr_km, params.time_pct); // (10)
    let lb0beta = lbfs + los::esp_db(pa.dlt_km, pa.dlr_km, beta0); // (11)

    // §4.3 diffraction on surface heights g.
    let dpath = diffraction::DiffractionPath {
        d_km,
        g_masl: g,
        htc: pa.hts_masl,
        hrc: pa.hrs_masl,
        hstd: pa.hstd_masl,
        hsrd: pa.hsrd_masl,
        f_ghz,
        omega,
        pol: params.polarization,
    };
    let (ld50, ldp) = diffraction::ldp_db(&dpath, ae, abeta, params.time_pct, beta0);
    let fi = diffraction::fi_factor(params.time_pct, beta0);

    // §4.4 troposcatter, §4.5 ducting.
    let lbs = tropo::lbs_db(f_ghz, d_total, pa.theta_mrad, params.n0, params.time_pct);
    let lba = ducting::lba_db(&ducting::DuctingInputs {
        f_ghz,
        d_km: d_total,
        dlt_km: pa.dlt_km,
        dlr_km: pa.dlr_km,
        theta_t_mrad: pa.theta_t_mrad,
        theta_r_mrad: pa.theta_r_mrad,
        hts_masl: pa.hts_masl,
        hrs_masl: pa.hrs_masl,
        hte_m: pa.hte_m,
        hre_m: pa.hre_m,
        hm_m: pa.hm_m,
        ae_km: ae,
        beta0_pct: beta0,
        d_lm_km: d_lm,
        omega,
        dct_km: dct,
        dcr_km: dcr,
        p_pct: params.time_pct,
    });

    // §4.6 combination → L_bc (p% time, 50% locations).
    let lbc = combine::lbc_db(&combine::CombineInputs {
        d_km: d_total,
        theta_mrad: pa.theta_mrad,
        omega,
        lb0p_db: lb0p,
        lb0beta_db: lb0beta,
        ldp_db: ldp,
        ld50_db: ld50,
        lbfs_db: lbfs,
        fi,
        lbs_db: lbs,
        lba_db: lba,
        p_pct: params.time_pct,
        beta0_pct: beta0,
    });

    // §4.7–4.9 location variability → L_b(p, pL).
    let sigma_l = location::sigma_l_db(f_ghz, params.location_resolution_m);
    let (l_loc, sigma_loc) = match params.entry_loss {
        None => (0.0, location::u_h(params.rx_h_agl_m, x.r_rx_m) * sigma_l), // (67a/68a)
        Some(e) => (e.median_db, location::sigma_indoor_db(sigma_l, e.sigma_db)), // (67b/68b)
    };
    let lb = location::lb_db(lb0p, lbc, l_loc, sigma_loc, params.loc_pct); // (69)

    Ok(Loss { lb_db: lb })
}

impl PathLossModel for P1812 {
    fn id(&self) -> &'static str {
        "p1812-8"
    }

    fn basic_transmission_loss(
        &self,
        profile: &Profile,
        params: &LinkParams,
    ) -> Result<Loss, ModelError> {
        let pts = &profile.points;
        let n = pts.len();
        if n < 3 {
            return Err(ModelError::BadProfile(
                "P.1812 needs at least 3 profile points (§3.2)".into(),
            ));
        }
        // Profile arrays (1a)–(1d); distances normalized to start at zero.
        let d0 = pts[0].d_m;
        let d_km: Vec<f64> = pts.iter().map(|p| (p.d_m - d0) / 1000.0).collect();
        let h: Vec<f64> = pts.iter().map(|p| p.h_terrain_m as f64).collect();
        let mut g = h.clone();
        for i in 1..n - 1 {
            g[i] += pts[i].h_clutter_m as f64; // (1d): terminals excluded
        }
        let (d_tm, d_lm) = path::land_sections_km(profile);
        let (dct, dcr) = path::coast_distances_km(profile);
        let x = ArrayInputs {
            d_km: &d_km,
            h_masl: &h,
            g_masl: &g,
            // A `Profile` carries terrain and a representative clutter height
            // per point, so g was built by eq. (1d) — §3.2.1's 30 m floor.
            surface_method: SurfaceMethod::RepresentativeClutter,
            omega: path::sea_fraction(profile),
            dct_km: dct,
            dcr_km: dcr,
            d_tm_km: d_tm,
            d_lm_km: d_lm,
            // R at the receiver: representative clutter height at the last
            // interior profile point (the RX's local environment).
            r_rx_m: pts[n - 2].h_clutter_m as f64,
        };
        lb_from_arrays(&x, params)
    }
}

pub mod tropo;

#[cfg(test)]
mod tests {
    use super::*;
    use planner_core::model::EntryLoss;
    use planner_core::profile::{ClutterClass, ProfilePoint, Zone};

    fn urban_profile(d_km: f64, n: usize, terrain: f32, clutter: f32) -> Profile {
        let points = (0..n)
            .map(|i| {
                let d_m = d_km * 1000.0 * i as f64 / (n - 1) as f64;
                ProfilePoint {
                    d_m,
                    h_terrain_m: terrain,
                    h_clutter_m: clutter,
                    clutter: if clutter > 0.0 { ClutterClass::Urban } else { ClutterClass::Open },
                    zone: Zone::Inland,
                }
            })
            .collect();
        Profile { points }
    }

    fn params() -> LinkParams {
        let mut p = LinkParams::eu868_defaults();
        p.freq_mhz = 868.0;
        p.tx_h_agl_m = 10.0;
        p.rx_h_agl_m = 2.0;
        p.loc_pct = 50.0;
        p
    }

    /// The geometry the transmitter-height cliff was reported on: 1.155 km
    /// across a Berlin block, terrain falling 62.42 → 57.27 m, receiver 12 m
    /// up. The transmitter stands in a 15 m urban cell; the path itself runs
    /// over a low 5 m canopy of gardens and single storeys, so nothing between
    /// the terminals obstructs a mast at ~15 m — Bullington reports the path
    /// clear, and the test below asserts that before it asserts anything else.
    ///
    /// `step_m` is the profile spacing. At the §3.2.1 floor the transmitter's
    /// own cell is sampled once, AT the transmitter, where eq. (1d) drops it.
    /// Below the floor it reappears as an intermediate point a few metres down
    /// the path — an obstacle the transmitter is then made to diffract over,
    /// with a 1/d_i grip on its own antenna height.
    fn near_mast_block_profile(step_m: f64) -> Profile {
        const D_M: f64 = 1155.2;
        const TX_CELL_M: f64 = 15.0; // the mast's own 10 m raster cell
        let n = (D_M / step_m).floor() as usize;
        let points = (0..=n)
            .map(|i| {
                let d_m = D_M * i as f64 / n as f64;
                ProfilePoint {
                    d_m,
                    h_terrain_m: (62.42 + (57.27 - 62.42) * (d_m / D_M)) as f32,
                    h_clutter_m: if d_m < TX_CELL_M { 15.0 } else { 5.0 },
                    clutter: if d_m < TX_CELL_M {
                        ClutterClass::Urban
                    } else {
                        ClutterClass::LowVegetation
                    },
                    zone: Zone::Inland,
                }
            })
            .collect();
        Profile { points }
    }

    fn berlin_params() -> LinkParams {
        let mut p = LinkParams::eu868_defaults();
        p.rx_h_agl_m = 12.0;
        p.loc_pct = 50.0;
        p.delta_n = 37.393; // Berlin pack manifest → a_e ≈ 8363 km, k = 1.313
        p.n0 = 319.87;
        p
    }

    /// L_b over a tx_h sweep, plus the worst secant slope between samples.
    fn sweep_tx_h(profile: &Profile, base: &LinkParams, lo: f64, hi: f64, step: f64) -> (Vec<f64>, f64) {
        let mut out = Vec::new();
        let mut worst = 0.0f64;
        let mut th = lo;
        while th <= hi + 1e-9 {
            let mut p = base.clone();
            p.tx_h_agl_m = th;
            let lb = P1812.basic_transmission_loss(profile, &p).unwrap().lb_db;
            if let Some(prev) = out.last() {
                worst = worst.max(((lb - prev) / step).abs());
            }
            out.push(lb);
            th += step;
        }
        (out, worst)
    }

    #[test]
    fn clear_short_path_reduces_to_free_space() {
        // 2 km, open, both terminals 30 m up: L_b ≈ L_bfs at p=50, pL=50.
        let profile = urban_profile(2.0, 41, 40.0, 0.0);
        let mut p = params();
        p.tx_h_agl_m = 30.0;
        p.rx_h_agl_m = 30.0;
        let lb = P1812.basic_transmission_loss(&profile, &p).unwrap().lb_db;
        let lbfs = los::lbfs_db(0.868, 2.0, 70.0, 70.0);
        assert!((lb - lbfs).abs() < 0.5, "lb={lb} lbfs={lbfs}");
    }

    #[test]
    fn urban_clutter_costs_tens_of_db() {
        let open = P1812
            .basic_transmission_loss(&urban_profile(3.0, 61, 40.0, 0.0), &params())
            .unwrap()
            .lb_db;
        let urban = P1812
            .basic_transmission_loss(&urban_profile(3.0, 61, 40.0, 15.0), &params())
            .unwrap()
            .lb_db;
        assert!(urban > open + 15.0, "urban={urban} open={open}");
        assert!(urban < 220.0, "sanity ceiling: {urban}");
    }

    #[test]
    fn location_percentage_orders_losses() {
        let profile = urban_profile(3.0, 61, 40.0, 15.0);
        let mut p50 = params();
        p50.loc_pct = 50.0;
        let mut p90 = params();
        p90.loc_pct = 90.0;
        let l50 = P1812.basic_transmission_loss(&profile, &p50).unwrap().lb_db;
        let l90 = P1812.basic_transmission_loss(&profile, &p90).unwrap().lb_db;
        assert!(l90 > l50, "{l90} vs {l50}");
    }

    #[test]
    fn indoor_terminal_pays_entry_loss() {
        let profile = urban_profile(3.0, 61, 40.0, 15.0);
        let mut pin = params();
        pin.entry_loss = Some(EntryLoss { median_db: 12.0, sigma_db: 6.0 });
        let out = P1812.basic_transmission_loss(&profile, &params()).unwrap().lb_db;
        let ind = P1812.basic_transmission_loss(&profile, &pin).unwrap().lb_db;
        assert!(ind > out + 10.0, "{ind} vs {out}");
    }

    #[test]
    fn validity_ranges_are_enforced() {
        let profile = urban_profile(3.0, 61, 40.0, 0.0);
        let mut bad_f = params();
        bad_f.freq_mhz = 10_000.0;
        assert!(matches!(
            P1812.basic_transmission_loss(&profile, &bad_f),
            Err(ModelError::OutOfRange(_))
        ));
        let short = urban_profile(0.1, 5, 40.0, 0.0);
        assert!(matches!(
            P1812.basic_transmission_loss(&short, &params()),
            Err(ModelError::OutOfRange(_))
        ));
        let two = Profile { points: urban_profile(1.0, 2, 0.0, 0.0).points };
        assert!(matches!(
            P1812.basic_transmission_loss(&two, &params()),
            Err(ModelError::BadProfile(_))
        ));
    }

    #[test]
    fn a_profile_sampled_below_the_clutter_method_spacing_floor_is_refused() {
        // §3.2.1: "the profile spacing should be no less than the order of
        // 30 m". Half a 10 m raster cell is 5 m — six times finer — and that
        // is the profile the point-to-point endpoint used to hand over.
        let fine = near_mast_block_profile(5.0);
        let err = P1812.basic_transmission_loss(&fine, &berlin_params()).unwrap_err();
        assert!(
            matches!(err, ModelError::BadProfile(ref m) if m.contains("spacing")),
            "expected a spacing refusal, got {err}"
        );
        // The same path at the floor is accepted.
        let coarse = near_mast_block_profile(30.0);
        assert!(P1812.basic_transmission_loss(&coarse, &berlin_params()).is_ok());
        // §3.2.2's surface-height method has its own, lower floor: the same
        // arrays are fine at 10 m there and still refused at 5 m.
        let pts = &near_mast_block_profile(10.0).points;
        let d_km: Vec<f64> = pts.iter().map(|p| p.d_m / 1000.0).collect();
        let h: Vec<f64> = pts.iter().map(|p| p.h_terrain_m as f64).collect();
        let mk = |method| ArrayInputs {
            d_km: &d_km,
            h_masl: &h,
            g_masl: &h,
            surface_method: method,
            omega: 0.0,
            dct_km: 1.1552,
            dcr_km: 1.1552,
            d_tm_km: 1.1552,
            d_lm_km: 1.1552,
            r_rx_m: 0.0,
        };
        assert!(lb_from_arrays(&mk(SurfaceMethod::SurfaceHeights), &berlin_params()).is_ok());
        assert!(lb_from_arrays(&mk(SurfaceMethod::RepresentativeClutter), &berlin_params()).is_err());
    }

    #[test]
    fn transmitter_height_cannot_move_the_loss_by_tens_of_db_per_metre() {
        // The defect this pins: on a 1.155 km urban path whose intermediate
        // clutter clears the mast, L_b fell 59 dB per metre of transmitter
        // height between 14.80 m and 14.82 m and then went flat — because the
        // profile point 5 m from the mast still carried the mast's OWN 15 m
        // clutter cell and won eq. (13)'s slope maximisation, where the 1/d_i
        // divisor gave it 200 (m/km) of slope per metre of antenna height.
        // Monotonicity alone would not have caught it (the curve stayed
        // monotone); only a bound on the DERIVATIVE does.
        let profile = near_mast_block_profile(30.0);
        let base = berlin_params();

        // Precondition: Bullington reports this path clear across the sweep,
        // so any structure in L_b(tx_h) here is spurious by construction.
        for th in [14.5f64, 15.1] {
            let pts = &profile.points;
            let n = pts.len();
            let d_km: Vec<f64> = pts.iter().map(|p| p.d_m / 1000.0).collect();
            let h: Vec<f64> = pts.iter().map(|p| p.h_terrain_m as f64).collect();
            let mut g = h.clone();
            for i in 1..n - 1 {
                g[i] += pts[i].h_clutter_m as f64;
            }
            let ae = path::median_effective_earth_radius_km(base.delta_n);
            let lbulla = diffraction::bullington_db(
                &d_km,
                &g,
                h[0] + th,
                h[n - 1] + base.rx_h_agl_m,
                ae,
                base.freq_mhz / 1000.0,
            );
            assert_eq!(lbulla, 0.0, "fixture must be Bullington-clear at tx_h {th}");
        }

        let (lb, worst) = sweep_tx_h(&profile, &base, 14.5, 15.1, 0.02);
        // Non-increasing: raising the transmitter cannot cost loss on a clear
        // path (1 mdB of slack for float noise, not for real structure).
        for w in lb.windows(2) {
            assert!(w[1] <= w[0] + 1e-3, "L_b rose with tx_h: {:.4} -> {:.4}", w[0], w[1]);
        }
        // Measured on this fixture: 0.00 dB/m at the §3.2.1 floor (the path is
        // clear, so L_b is free space and barely moves), against 15.19 dB/m —
        // and 19 dB of pure level error, 111.55 → 104.05 — when the same path
        // is sampled every 5 m. The bound sits well above what a clear path
        // does and well below what the defect did, so it fails loudly on a
        // regression without becoming a tripwire for ordinary retuning.
        assert!(worst < 1.0, "|dL_b/dh_tx| reached {worst:.2} dB/m over 14.5–15.1 m");
    }

    #[test]
    fn a_clear_kilometre_over_a_low_canopy_costs_about_free_space() {
        // Short-path anchor for the sweep above: same 1.155 km geometry, mast
        // at 15 m over an 8 m canopy. With Bullington clear and the spherical
        // term zero, §4.6 leaves L_bfs plus the §4.7 location term, and L_b
        // must sit within a couple of dB of free space — NOT the 133–142 dB
        // the near-mast artefact used to report here.
        let profile = near_mast_block_profile(30.0);
        let mut p = berlin_params();
        p.tx_h_agl_m = 15.0;
        let lb = P1812.basic_transmission_loss(&profile, &p).unwrap().lb_db;
        let lbfs = los::lbfs_db(p.freq_mhz / 1000.0, 1.1552, 62.42 + 15.0, 57.27 + 12.0);
        assert!((92.0..93.0).contains(&lbfs), "free-space anchor moved: {lbfs}");
        assert!(
            (lb - lbfs).abs() < 0.5,
            "L_b {lb:.2} should sit on free space {lbfs:.2} for a clear path"
        );
        // Absolute pin, so a change that moved BOTH sides together still
        // shows up. Measured 92.44 dB at 869.525 MHz over 1.1552 km.
        assert!((92.0..93.0).contains(&lb), "L_b moved off its anchor: {lb:.2}");
    }

    #[test]
    fn terminal_surface_heights_are_never_read() {
        // Eq. (1d)'s g_1 = h_1, g_n = h_n is inert: §4.3.1 ranges over the
        // intermediate points only and Attachment 1 runs on h. Callers may
        // therefore leave clutter on the terminals — but only as long as that
        // stays true, so pin it rather than trusting a comment.
        let pts = &near_mast_block_profile(30.0).points;
        let n = pts.len();
        let d_km: Vec<f64> = pts.iter().map(|p| p.d_m / 1000.0).collect();
        let h: Vec<f64> = pts.iter().map(|p| p.h_terrain_m as f64).collect();
        let mut g = h.clone();
        for i in 1..n - 1 {
            g[i] += pts[i].h_clutter_m as f64;
        }
        let mut g_dirty = g.clone();
        g_dirty[0] += 15.0;
        g_dirty[n - 1] += 15.0;
        let mk = |gg: &[f64]| {
            lb_from_arrays(
                &ArrayInputs {
                    d_km: &d_km,
                    h_masl: &h,
                    g_masl: gg,
                    surface_method: SurfaceMethod::RepresentativeClutter,
                    omega: 0.0,
                    dct_km: 1.1552,
                    dcr_km: 1.1552,
                    d_tm_km: 1.1552,
                    d_lm_km: 1.1552,
                    r_rx_m: 5.0,
                },
                &berlin_params(),
            )
            .unwrap()
            .lb_db
        };
        assert_eq!(mk(&g), mk(&g_dirty));
    }

    #[test]
    fn longer_paths_lose_more() {
        let l3 = P1812
            .basic_transmission_loss(&urban_profile(3.0, 61, 40.0, 10.0), &params())
            .unwrap()
            .lb_db;
        let l10 = P1812
            .basic_transmission_loss(&urban_profile(10.0, 201, 40.0, 10.0), &params())
            .unwrap()
            .lb_db;
        let l30 = P1812
            .basic_transmission_loss(&urban_profile(30.0, 301, 40.0, 10.0), &params())
            .unwrap()
            .lb_db;
        assert!(l3 < l10 && l10 < l30, "{l3} {l10} {l30}");
    }
}
