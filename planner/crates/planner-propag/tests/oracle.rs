//! Black-box validation of our P.1812-8 against eeveetza/Py1812.
//!
//! Vectors are generated locally by `planner/tools/gen_oracle.py` (they derive
//! from the non-redistributable ITU maps, so they are gitignored). When the
//! vectors file is absent the test passes with a skip notice — CI without the
//! maps stays green; the oracle gate runs wherever the vectors exist.
//! Acceptance: |ΔLb| ≤ 0.1 dB on every vector.

use planner_core::model::{LinkParams, PathLossModel, Polarization};
use planner_core::profile::{ClutterClass, Profile, ProfilePoint, Zone};
use planner_propag::P1812;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Vector {
    name: String,
    f_ghz: f64,
    p: f64,
    pl: f64,
    pol: u8,
    htg: f64,
    hrg: f64,
    phi: f64,
    dn: f64,
    n0: f64,
    wa: f64,
    d_km: Vec<f64>,
    h: Vec<f64>,
    r: Vec<f64>,
    zone: Vec<u8>,
    expected_lb: f64,
}

fn to_profile(v: &Vector) -> Profile {
    let points = v
        .d_km
        .iter()
        .zip(&v.h)
        .zip(&v.r)
        .zip(&v.zone)
        .map(|(((d, h), r), z)| ProfilePoint {
            d_m: d * 1000.0,
            h_terrain_m: *h as f32,
            h_clutter_m: *r as f32,
            clutter: if *r > 0.0 { ClutterClass::Urban } else { ClutterClass::Open },
            zone: match z {
                1 => Zone::Sea,
                3 => Zone::Coastal,
                _ => Zone::Inland,
            },
        })
        .collect();
    Profile { points }
}

struct OracleRun {
    n: usize,
    failures: Vec<String>,
    worst: (f64, String),
}

fn run_oracle() -> Option<OracleRun> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle/vectors/vectors.jsonl");
    let Ok(data) = std::fs::read_to_string(&path) else {
        eprintln!(
            "SKIP: no oracle vectors at {} — run planner/tools/gen_oracle.py (needs Py1812 + ITU maps)",
            path.display()
        );
        return None;
    };

    let mut failures = Vec::new();
    let mut worst: (f64, String) = (0.0, String::new());
    let mut n = 0usize;
    for line in data.lines().filter(|l| !l.trim().is_empty()) {
        let v: Vector = serde_json::from_str(line).expect("vector json");
        n += 1;
        let profile = to_profile(&v);
        let params = LinkParams {
            freq_mhz: v.f_ghz * 1000.0,
            tx_h_agl_m: v.htg,
            rx_h_agl_m: v.hrg,
            time_pct: v.p,
            loc_pct: v.pl,
            polarization: if v.pol == 1 { Polarization::Horizontal } else { Polarization::Vertical },
            delta_n: v.dn,
            n0: v.n0,
            path_center_lat_deg: v.phi,
            location_resolution_m: v.wa,
            entry_loss: None,
        };
        match P1812.basic_transmission_loss(&profile, &params) {
            Ok(loss) => {
                let delta = loss.lb_db - v.expected_lb;
                if delta.abs() > worst.0.abs() {
                    worst = (delta, v.name.clone());
                }
                if delta.abs() > 0.1 {
                    failures.push(format!(
                        "{}: ours {:.4} vs py1812 {:.4} (Δ {:+.4} dB)",
                        v.name, loss.lb_db, v.expected_lb, delta
                    ));
                }
            }
            Err(e) => failures.push(format!("{}: model error: {e}", v.name)),
        }
    }
    eprintln!("oracle: {n} vectors, worst Δ {:+.4} dB ({})", worst.0, worst.1);
    Some(OracleRun { n, failures, worst })
}

/// Always-on guard: locks in the currently-achieved accuracy so regressions
/// fail immediately (≥95% of vectors within 0.1 dB, worst within 0.5 dB).
#[test]
fn oracle_regression_guard() {
    let Some(run) = run_oracle() else { return };
    assert!(
        run.worst.0.abs() <= 0.5,
        "worst deviation {:+.4} dB ({}) exceeds 0.5 dB guard",
        run.worst.0,
        run.worst.1
    );
    let within = run.n - run.failures.len();
    assert!(
        within * 100 >= run.n * 95,
        "only {within}/{} vectors within 0.1 dB:\n{}",
        run.n,
        run.failures.join("\n")
    );
}

/// The P0 acceptance gate: EVERY vector within 0.1 dB of Py1812.
/// (Passed 2026-08-30 on 108 vectors after fixing a generator artifact —
/// degenerate identical endpoints skewed Py1812's path-centre latitude.)
#[test]
fn oracle_strict_gate() {
    let Some(run) = run_oracle() else { return };
    assert!(
        run.failures.is_empty(),
        "{}/{} vectors outside 0.1 dB:\n{}",
        run.failures.len(),
        run.n,
        run.failures.join("\n")
    );
}
