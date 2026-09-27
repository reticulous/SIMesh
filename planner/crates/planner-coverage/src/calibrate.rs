//! Fit per-clutter-class offsets from measured links.
//!
//! Lives here rather than in `planner-pack` because fitting needs the
//! propagation model and the pack's grids at the same time, which is exactly
//! this crate's job. The profile *format* is in `planner_core::calibration`
//! so that consumers can read a profile without pulling in the model.
//!
//! Method, deliberately blunt: for every sample, predict L_b along the
//! terrain+clutter profile, take `residual = measured − predicted`, group by
//! the clutter class **at the receiver**, and use the median residual as
//! that class's offset. Median rather than mean because community RSSI data
//! contains transcription errors, GPS drift and indoor receivers, and one
//! bad row must not move a class.
//!
//! Why the receiver's class: a household node sits *inside* its own clutter,
//! and the terminal clutter term is where P.1812's urban behaviour is least
//! constrained by the profile. A path-dominant class would be defensible
//! too; if the residuals stay large this is the first thing to revisit.

use crate::CoverageError;
use planner_core::calibration::{
    mad, median, CalibrationProfile, ClassOffset, FitAssumptions, LinkSample,
};
use planner_core::geo::Xy;
use planner_core::model::LinkParams;
use planner_core::profile::ClutterClass;
use planner_propag::p1812::{lb_from_arrays, ArrayInputs};
use planner_terrain::Grid;
use std::collections::HashMap;

/// Profile sampling step for the fit (m). Matches the coverage sweep's own
/// resolution philosophy: fine enough not to skip a ridge, coarse enough
/// that a few thousand samples are cheap.
const STEP_M: f64 = 30.0;

pub struct FitParams {
    pub name: String,
    pub created_utc: String,
    pub link: LinkParams,
    pub assumptions: FitAssumptions,
}

/// Predict L_b for one sample over the pack's grids.
fn predict(
    terrain: &Grid,
    clutter: Option<&Grid>,
    s: &LinkSample,
    link: &LinkParams,
) -> Option<f64> {
    let prof = planner_terrain::extract_profile(terrain, clutter, s.tx, s.rx, STEP_M).ok()?;
    let n = prof.points.len();
    if n < 3 {
        return None;
    }
    let mut d_km = Vec::with_capacity(n);
    let mut h_masl = Vec::with_capacity(n);
    let mut g_masl = Vec::with_capacity(n);
    for p in &prof.points {
        d_km.push(p.d_m / 1000.0);
        h_masl.push(p.h_terrain_m as f64);
        g_masl.push((p.h_terrain_m + p.h_clutter_m) as f64);
    }
    let mut link = link.clone();
    if let Some(h) = s.tx_h_agl_m {
        link.tx_h_agl_m = h;
    }
    if let Some(h) = s.rx_h_agl_m {
        link.rx_h_agl_m = h;
    }
    // All-inland scalars, matching the sweep's own convention (Table 5).
    let total_km = *d_km.last().unwrap_or(&0.0);
    let x = ArrayInputs {
        d_km: &d_km,
        h_masl: &h_masl,
        g_masl: &g_masl,
        omega: 0.0,
        dct_km: total_km,
        dcr_km: total_km,
        d_tm_km: total_km,
        d_lm_km: total_km,
        // Clutter at the receiver is already in `g_masl`; the terminal term
        // is left at 0 so the fit measures the model as the sweep uses it.
        r_rx_m: 0.0,
    };
    lb_from_arrays(&x, &link).ok().map(|l| l.lb_db)
}

/// Clutter class at a point, from the pack's class raster.
fn class_at(class_grid: Option<&Grid>, p: Xy) -> ClutterClass {
    class_grid
        .and_then(|g| g.sample_bilinear(p))
        .and_then(|v| ClutterClass::from_code(v.round() as u8))
        .unwrap_or(ClutterClass::Open)
}

/// Fit a calibration profile. Returns an error only when nothing could be
/// predicted at all; a fit that finds no structure comes back as a profile
/// whose `is_useful()` is false, because that is a result worth recording.
pub fn fit(
    terrain: &Grid,
    clutter: Option<&Grid>,
    class_grid: Option<&Grid>,
    samples: &[LinkSample],
    p: &FitParams,
) -> Result<CalibrationProfile, CoverageError> {
    let mut by_class: HashMap<ClutterClass, Vec<f64>> = HashMap::new();
    let mut all_residuals: Vec<f64> = Vec::new();
    let mut used = 0usize;
    for s in samples {
        let Some(pred) = predict(terrain, clutter, s, &p.link) else { continue };
        let resid = s.measured_loss_db - pred;
        if !resid.is_finite() {
            continue;
        }
        used += 1;
        all_residuals.push(resid);
        by_class.entry(class_at(class_grid, s.rx)).or_default().push(resid);
    }
    if used == 0 {
        return Err(CoverageError::BadRadius); // nothing predictable
    }

    let mut per_class: Vec<ClassOffset> = by_class
        .into_iter()
        .map(|(class, mut rs)| {
            let m = median(&mut rs);
            ClassOffset { class, offset_db: m, n: rs.len(), mad_db: mad(&rs, m) }
        })
        .collect();
    // Stable order so two fits of the same data produce identical JSON.
    per_class.sort_by_key(|c| c.class.code());

    let before = {
        let mut a: Vec<f64> = all_residuals.iter().map(|r| r.abs()).collect();
        median(&mut a)
    };
    // "After" must be judged with the SAME gate that predictions will use,
    // or a profile can look good on classes it will refuse to apply.
    let lookup: HashMap<ClutterClass, f64> = per_class
        .iter()
        .filter(|c| c.n >= planner_core::calibration::MIN_SAMPLES_PER_CLASS)
        .map(|c| (c.class, c.offset_db))
        .collect();
    let mut after_vals: Vec<f64> = Vec::with_capacity(samples.len());
    for s in samples {
        let Some(pred) = predict(terrain, clutter, s, &p.link) else { continue };
        let off = lookup.get(&class_at(class_grid, s.rx)).copied().unwrap_or(0.0);
        let r = s.measured_loss_db - (pred + off);
        if r.is_finite() {
            after_vals.push(r.abs());
        }
    }
    let after = median(&mut after_vals);

    Ok(CalibrationProfile {
        name: p.name.clone(),
        created_utc: p.created_utc.clone(),
        freq_mhz: p.link.freq_mhz,
        assumptions: p.assumptions.clone(),
        n_observations: used,
        per_class,
        median_abs_residual_before_db: before,
        median_abs_residual_after_db: after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_world(n: usize, res: f64) -> (Grid, Grid) {
        let terrain =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, vec![20.0f32; n * n]).unwrap();
        // Left half urban (code 5), right half open (code 0).
        let mut codes = vec![0f32; n * n];
        for r in 0..n {
            for c in 0..n / 2 {
                codes[r * n + c] = ClutterClass::Urban.code() as f32;
            }
        }
        let class = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, codes).unwrap();
        (terrain, class)
    }

    fn params() -> FitParams {
        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 868.0;
        link.tx_h_agl_m = 10.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        FitParams {
            name: "test".into(),
            created_utc: "2026-08-31T00:00:00Z".into(),
            link,
            assumptions: FitAssumptions {
                tx_power_dbm: 22.0,
                antenna_gain_dbi: 2.0,
                assumed_noise_floor_dbm: None,
                notes: "synthetic".into(),
            },
        }
    }

    /// Inject a known per-class bias and check the fit recovers it. This is
    /// the only honest test available until real measurements arrive: it
    /// proves the machinery, not the physics.
    #[test]
    fn recovers_an_injected_per_class_bias() {
        let (terrain, class) = flat_world(200, 30.0);
        let p = params();
        let tx = Xy { x: 3000.0, y: -3000.0 };
        // Receivers spread along a line, half in each class region.
        let mut samples = Vec::new();
        let mut truth = Vec::new();
        for i in 0..60 {
            let x = 300.0 + i as f64 * 90.0;
            let rx = Xy { x, y: -3000.0 };
            if rx.dist_m(&tx) < 300.0 {
                continue;
            }
            let Some(pred) = predict(&terrain, None, &LinkSample {
                tx, rx, measured_loss_db: 0.0, tx_h_agl_m: None, rx_h_agl_m: None,
            }, &p.link) else { continue };
            let cls = class_at(Some(&class), rx);
            let bias = if cls == ClutterClass::Urban { 9.0 } else { -3.0 };
            samples.push(LinkSample {
                tx,
                rx,
                measured_loss_db: pred + bias,
                tx_h_agl_m: None,
                rx_h_agl_m: None,
            });
            truth.push((cls, bias));
        }
        assert!(samples.len() > 30, "fixture should produce plenty of samples");

        let prof = fit(&terrain, None, Some(&class), &samples, &p).unwrap();
        assert_eq!(prof.n_observations, samples.len());
        for c in &prof.per_class {
            let want = if c.class == ClutterClass::Urban { 9.0 } else { -3.0 };
            assert!(
                (c.offset_db - want).abs() < 1e-6,
                "{:?}: fitted {} want {want}",
                c.class,
                c.offset_db
            );
            assert!(c.mad_db < 1e-6, "a noiseless fixture must have ~zero spread");
        }
        // The fit removed the bias it was given.
        assert!(prof.median_abs_residual_before_db > 2.0);
        assert!(prof.median_abs_residual_after_db < 1e-6);
        assert!(prof.is_useful());
    }

    /// One absurd row must not drag a class offset around.
    #[test]
    fn median_fit_is_robust_to_an_outlier() {
        let (terrain, class) = flat_world(200, 30.0);
        let p = params();
        let tx = Xy { x: 3000.0, y: -3000.0 };
        let mut samples = Vec::new();
        for i in 0..40 {
            let rx = Xy { x: 300.0 + i as f64 * 60.0, y: -3000.0 };
            if rx.dist_m(&tx) < 300.0 {
                continue;
            }
            let Some(pred) = predict(&terrain, None, &LinkSample {
                tx, rx, measured_loss_db: 0.0, tx_h_agl_m: None, rx_h_agl_m: None,
            }, &p.link) else { continue };
            // One transcription-error row at +200 dB.
            let bias = if i == 7 { 200.0 } else { 5.0 };
            samples.push(LinkSample {
                tx, rx, measured_loss_db: pred + bias, tx_h_agl_m: None, rx_h_agl_m: None,
            });
        }
        let prof = fit(&terrain, None, Some(&class), &samples, &p).unwrap();
        for c in &prof.per_class {
            assert!(
                (c.offset_db - 5.0).abs() < 0.5,
                "{:?} offset {} was dragged by the outlier",
                c.class,
                c.offset_db
            );
        }
    }

    /// Structureless noise must NOT produce a profile that claims to help.
    #[test]
    fn noise_does_not_look_like_a_fit() {
        let (terrain, class) = flat_world(200, 30.0);
        let p = params();
        let tx = Xy { x: 3000.0, y: -3000.0 };
        let mut samples = Vec::new();
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) - 0.5
        };
        for i in 0..60 {
            let rx = Xy { x: 300.0 + i as f64 * 90.0, y: -3000.0 };
            if rx.dist_m(&tx) < 300.0 {
                continue;
            }
            let Some(pred) = predict(&terrain, None, &LinkSample {
                tx, rx, measured_loss_db: 0.0, tx_h_agl_m: None, rx_h_agl_m: None,
            }, &p.link) else { continue };
            samples.push(LinkSample {
                tx, rx, measured_loss_db: pred + rnd() * 40.0,
                tx_h_agl_m: None, rx_h_agl_m: None,
            });
        }
        let prof = fit(&terrain, None, Some(&class), &samples, &p).unwrap();
        // Zero-mean noise leaves a near-zero offset and a large spread; the
        // "improvement" must be marginal at best.
        for c in &prof.per_class {
            assert!(c.mad_db > 3.0, "noise should show a wide MAD, got {}", c.mad_db);
        }
        let gain = prof.median_abs_residual_before_db - prof.median_abs_residual_after_db;
        assert!(gain < 2.0, "noise must not appear to buy much: {gain} dB");
    }
}
