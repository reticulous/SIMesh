//! The DISC-table sweep, kept as a reference, and the tests that hold the
//! sectored sweep in `lib.rs` to it bit for bit.
//!
//! `coverage()` used to allocate one polar loss table for the whole disc,
//! `vec![f32::NAN; n_az * m]`, and read it in exactly one place: the
//! four-corner bilinear gather in the rasteriser, which for each output cell
//! touches azimuth rows `ia0` and `ia0 + 1` and range columns `ik0`, `ik1`.
//! The table was therefore never needed in full — only two ADJACENT rows are
//! live for any one cell. Splitting the azimuths into S consecutive sectors,
//! and giving each sector ONE extra row past its end (wrapping for the last),
//! keeps every gather intact while holding `(n_az/S + 1) * m * 4` bytes
//! instead of `n_az * m * 4`.
//!
//! Sizes that motivated the split, at the 32768-azimuth ceiling the web layer
//! sets and a 5 m pack: 20 km ⇒ n_az = 25133, m = 4000 ⇒ 402 MB; 30 km ⇒ n_az
//! clamps to 32768, m = 6000 ⇒ 786 MB.
//!
//! The output raster must be BIT-IDENTICAL to the disc version — this is a
//! memory rewrite, not a numerical one. Two properties buy that:
//!
//!  1. a radial's row is a pure function of `(az, n_az, grid, params)`, with
//!     no coupling between radials, so computing it inside a sector produces
//!     the same f64/f32 bits as computing it in the disc sweep. Since the
//!     kernel (`crate::fill_radial`) is now shared by both, that is true by
//!     construction rather than by transcription — this file used to carry a
//!     second copy of it, and a review that mutated the terminal terms in one
//!     copy showed how quietly two copies diverge;
//!  2. every output cell is rasterised by exactly ONE sector, chosen by the
//!     same `ia0` the gather itself uses, and that sector holds rows `ia0` and
//!     `ia0 + 1`. The wedge bounding box is a visit filter only: it may be
//!     loose, and correctness never depends on where its edges land.
//!
//! What the split costs, measured by `bench_disc_against_sectored` (601×601
//! grid at 10 m, radius 2.5 km, n_az 1571, m 250 — the sweep's natural
//! n_az ≈ 2π·m shape; min of 9 INTERLEAVED repetitions on a 10-core/12-thread
//! workstation, quietest of four runs, disc = 117.4 ms of which 114.8 ms was
//! the radial sweep and 1.7 ms the rasteriser):
//!
//! ```text
//!   S     time      table     wedge visits   extra radials
//!   1   +0.1 %    1.57 MB        1.00x           +0.06 %
//!   2   +0.6 %    0.79 MB        1.01x           +0.13 %
//!   4   +1.6 %    0.39 MB        1.02x           +0.25 %
//!   8   +6.2 %    0.20 MB        1.47x           +0.51 %
//!  16  +11.7 %    0.10 MB        2.18x           +1.02 %
//!  32  +19.2 %    0.05 MB        3.55x           +2.04 %
//!  64  +33.9 %    0.03 MB        6.29x           +4.07 %
//! 128  +66.2 %    0.01 MB       11.77x           +8.15 %
//! ```
//!
//! Two things to read off it. Up to S ≈ 8 the split is nearly free — an order
//! of magnitude off the table for a few per cent of the sweep — and past that
//! the wedge windows and the S parallel barriers cost more than the memory is
//! worth, which is why `sectors_for_budget` picks the FEWEST sectors that fit
//! rather than a fixed count. And the overhead is NOT mostly the rasteriser:
//! at S = 128 it grew 25 ms of the 78 ms added, the rest being the extra
//! radials and the tail of each sector's parallel region.
//!
//! Timing on a loaded machine measures the load. The same interleaved harness
//! reported S = 1 at +0.1 %, +5.0 %, +14.2 % and +0.1 % across four
//! consecutive runs while another build shared the cores — and S = 1 is the
//! disc's own shape, so anything above noise there is the machine talking.
//! Quote the quietest run, and quote S = 1 alongside it as the noise floor.
//! On an independent re-run the floor itself sat at +2.7 % to +9.3 %, so the
//! sub-10 % rows above carry one significant figure of meaning at most; the
//! shape (free to S ≈ 8, then rising) and the deterministic columns (wedge
//! visits, extra radials) are what reproduce to the digit.
//!
//! Nothing outside the tests calls the reference. `lib.rs` declares this module
//! `#[cfg(test)]`, which is the one configuration where it has to exist.

use crate::{
    fill_radial, sector_range, wedge_window, CoverageError, CoverageParams, CoverageResult,
    RadialScratch,
};
use planner_core::geo::Xy;
use planner_terrain::Grid;

/// The sweep as it was before the sector split: ONE polar table for the whole
/// disc, one pass of the rasteriser over the whole radius window.
///
/// Kept verbatim apart from calling the shared radial kernel instead of an
/// inline copy of it, so that "the sectored sweep changed no number" is a test
/// rather than a claim. If `coverage()` ever gains a genuinely new behaviour,
/// this reference has to gain it too or the bit-identity test will say so.
fn coverage_disc(
    terrain: &Grid,
    clutter: Option<&Grid>,
    p: &CoverageParams,
) -> Result<CoverageResult, CoverageError> {
    coverage_disc_timed(terrain, clutter, p, &mut crate::Timings::default())
}

/// `coverage_disc` with the sweep/raster split reported, so a bench can put
/// the two designs' phases side by side. The four `Instant::now()` calls are
/// the only thing here that the code this replaced did not do; they touch no
/// float and the bit-identity test covers the rest.
fn coverage_disc_timed(
    terrain: &Grid,
    clutter: Option<&Grid>,
    p: &CoverageParams,
    timings: &mut crate::Timings,
) -> Result<CoverageResult, CoverageError> {
    *timings = crate::Timings::default();
    if p.radius_m <= 0.0 {
        return Err(CoverageError::BadRadius);
    }
    let res = terrain.dx_m.abs();
    if terrain.sample_bilinear(p.tx).is_none() {
        return Err(CoverageError::TxOutsideGrid);
    }
    crate::gaps::probe_link(&p.link)?;

    let m = (p.radius_m / res).floor() as usize; // steps per radial (excl. TX)
    let n_az = p.max_azimuths.unwrap_or_else(|| {
        ((2.0 * std::f64::consts::PI * p.radius_m / res).ceil() as usize).clamp(360, 5760)
    });

    // Per-radial loss tables, indexed [az][step-1] — the allocation the split
    // exists to avoid.
    let mut table = vec![f32::NAN; n_az * m];
    let d_km: Vec<f64> = (0..=m).map(|k| k as f64 * res / 1000.0).collect();

    use rayon::prelude::*;
    let t_sweep = std::time::Instant::now();
    table.par_chunks_mut(m).enumerate().for_each_init(
        || RadialScratch::new(m, p.max_profile_points),
        |scratch, (az, row)| {
            if let Some(c) = &p.cancel {
                if c.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
            }
            fill_radial(terrain, clutter, p, res, m, n_az, &d_km, az, row, scratch);
        },
    );
    timings.sweep_ms = t_sweep.elapsed().as_secs_f64() * 1e3;

    let rad_px = (p.radius_m / res).ceil() as isize + 1;
    let col_c = ((p.tx.x - terrain.origin.x) / terrain.dx_m).round() as isize;
    let row_c = ((p.tx.y - terrain.origin.y) / terrain.dy_m).round() as isize;
    let c0 = (col_c - rad_px).max(0) as usize;
    let r0 = (row_c - rad_px).max(0) as usize;
    let c1 = ((col_c + rad_px).max(0) as usize).min(terrain.width - 1);
    let r1 = ((row_c + rad_px).max(0) as usize).min(terrain.height - 1);
    let (ow, oh) = (c1.saturating_sub(c0) + 1, r1.saturating_sub(r0) + 1);
    let mut out = Grid {
        origin: Xy {
            x: terrain.origin.x + c0 as f64 * terrain.dx_m,
            y: terrain.origin.y + r0 as f64 * terrain.dy_m,
        },
        dx_m: terrain.dx_m,
        dy_m: terrain.dy_m,
        width: ow,
        height: oh,
        data: vec![f32::NAN; ow * oh],
    };
    let two_pi = 2.0 * std::f64::consts::PI;
    let t_raster = std::time::Instant::now();
    let (ow, o_origin, o_dx, o_dy) = (out.width, out.origin, out.dx_m, out.dy_m);
    out.data.par_chunks_mut(ow).enumerate().for_each(|(row, orow)| {
        let y = o_origin.y + row as f64 * o_dy;
        for col in 0..ow {
            let x = o_origin.x + col as f64 * o_dx;
            let (ex, ey) = (x - p.tx.x, y - p.tx.y);
            let dist = (ex * ex + ey * ey).sqrt();
            if dist > p.radius_m {
                continue;
            }
            let kf = dist / res;
            if kf < 1.0 || kf > m as f64 {
                continue;
            }
            let mut ang = ex.atan2(ey); // 0 = north, clockwise positive
            if ang < 0.0 {
                ang += two_pi;
            }
            let azf = ang / two_pi * n_az as f64;
            let a0 = azf.floor();
            let (ia0, ia1) = (a0 as usize % n_az, (a0 as usize + 1) % n_az);
            let ta = azf - a0;
            let k0 = kf.floor().max(1.0);
            let (ik0, ik1) = (k0 as usize, (k0 as usize + 1).min(m));
            let tk = kf - k0;

            let mut acc = 0.0f64;
            let mut wsum = 0.0f64;
            for (ia, wa) in [(ia0, 1.0 - ta), (ia1, ta)] {
                for (ik, wk) in [(ik0, 1.0 - tk), (ik1, tk)] {
                    let v = table[ia * m + (ik - 1)];
                    if v.is_finite() {
                        let w = wa * wk;
                        acc += v as f64 * w;
                        wsum += w;
                    }
                }
            }
            if wsum > 0.5 {
                orow[col] = (acc / wsum) as f32;
            }
        }
    });
    timings.raster_ms = t_raster.elapsed().as_secs_f64() * 1e3;

    if let Some(c) = &p.cancel {
        if c.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(CoverageError::Cancelled);
        }
    }

    Ok(CoverageResult {
        loss: out,
        col_offset: c0,
        row_offset: r0,
        azimuths: n_az,
        steps_per_radial: m,
        sectors: 1,
    })
}

/// Total inclusive-window cells the sector rasteriser visits, against the one
/// window the disc rasteriser walks. Pure geometry — no sweep is run — so the
/// overhead can be reported without paying for the propagation.
fn wedge_visit_cells(
    out: &Grid,
    tx: Xy,
    radius_m: f64,
    n_az: usize,
    sectors: usize,
) -> (u64, u64) {
    let sectors = sectors.clamp(1, n_az.max(1));
    let mut sum = 0u64;
    for s in 0..sectors {
        let (a_lo, a_hi) = sector_range(n_az, sectors, s);
        if let Some((r0, r1, c0, c1)) = wedge_window(out, tx, radius_m, n_az, a_lo, a_hi) {
            sum += ((r1 - r0 + 1) as u64) * ((c1 - c0 + 1) as u64);
        }
    }
    (sum, (out.width as u64) * (out.height as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coverage, coverage_inner, peak_table_bytes, Timings, DEFAULT_STOP_AFTER_M,
        DEFAULT_TABLE_BUDGET_BYTES, MAX_PROFILE_POINTS,
    };
    use planner_core::model::LinkParams;

    /// The sectored sweep with the split PINNED, which `coverage()` does not
    /// expose: the budget picks S there, and these tests need to hold S itself
    /// still while everything else varies.
    fn coverage_sectored(
        terrain: &Grid,
        clutter: Option<&Grid>,
        p: &CoverageParams,
        sectors: usize,
    ) -> Result<CoverageResult, CoverageError> {
        coverage_inner(
            terrain,
            clutter,
            p,
            Some(sectors),
            true,
            &mut Timings::default(),
        )
    }

    /// Deterministic, dependency-free noise so the property test's terrains are
    /// reproducible on any machine (xorshift64*).
    fn rng(seed: &mut u64) -> f64 {
        let mut x = *seed;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *seed = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64) / ((1u64 << 53) as f64)
    }

    /// Relief at several scales — a broad swell, a knife edge, cell-scale
    /// roughness, and a seeded random field — so decimation, the horizon
    /// search and the budget cut-off all have something to bite on.
    fn rough_terrain(n: usize, res: f64, seed: u64) -> Grid {
        let mut s = seed | 1;
        let mut data = vec![0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let (x, y) = (col as f64 * res, row as f64 * res);
                let broad = 60.0 * ((x / 4000.0).sin() + (y / 5000.0).cos());
                let knife = if (col as i64 - (n as i64 * 2 / 3)).abs() < 2 { 70.0 } else { 0.0 };
                let rough = 5.0 * ((x / 61.0).sin() * (y / 47.0).cos());
                let noise = 12.0 * rng(&mut s);
                data[row * n + col] = (120.0 + broad + knife + rough + noise) as f32;
            }
        }
        Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap()
    }

    fn patchy_clutter(n: usize, res: f64, seed: u64) -> Grid {
        let mut s = seed | 1;
        let mut data = vec![0f32; n * n];
        for v in data.iter_mut() {
            let r = rng(&mut s);
            *v = if r < 0.35 { 0.0 } else { (4.0 + 22.0 * r) as f32 };
        }
        Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap()
    }

    fn link() -> LinkParams {
        let mut l = LinkParams::eu868_defaults();
        l.freq_mhz = 869.618;
        l.tx_h_agl_m = 18.0;
        l.rx_h_agl_m = 2.0;
        l.loc_pct = 50.0;
        l
    }

    fn params(tx: Xy, radius_m: f64, azimuths: usize, stop: Option<f32>) -> CoverageParams {
        CoverageParams {
            tx,
            radius_m,
            link: link(),
            max_azimuths: Some(azimuths),
            stop_above_db: stop,
            stop_after_m: DEFAULT_STOP_AFTER_M,
            tx_terminal_db: None,
            tx_model_h_m: None,
            rx_terminal: None,
            cancel: None,
            max_profile_points: MAX_PROFILE_POINTS,
            table_budget_bytes: DEFAULT_TABLE_BUDGET_BYTES,
        }
    }

    /// As `params`, with BOTH terminal corrections live. Without this the
    /// terminal terms are dead weight in every comparison -- an independent
    /// review mutated them and the suite did not notice. The bearing table is
    /// deliberately asymmetric, so a sector reading the wrong azimuth for it
    /// shows up as a different number.
    fn params_with_terminals(tx: Xy, radius_m: f64, azimuths: usize, stop: Option<f32>) -> CoverageParams {
        let mut p = params(tx, radius_m, azimuths, stop);
        p.tx_terminal_db = Some(std::sync::Arc::new(|ang: f64| {
            (6.0 + 5.0 * ang.sin() + 2.0 * (3.0 * ang).cos()) as f32
        }));
        p.rx_terminal = Some(crate::RxTerminal { freq_ghz: 0.868, h_agl_m: 2.0, ws_m: 27.0 });
        p
    }

    /// Bit-level comparison. NOT an epsilon: the sector split is a memory
    /// rewrite, and any numerical difference at all means a row was computed
    /// from different inputs or gathered from the wrong place. Both-NaN counts
    /// as equal only because a NaN's payload is not part of the answer.
    fn assert_bit_identical(a: &CoverageResult, b: &CoverageResult, what: &str) {
        assert_eq!(a.azimuths, b.azimuths, "{what}: azimuth count");
        assert_eq!(a.steps_per_radial, b.steps_per_radial, "{what}: steps");
        assert_eq!((a.col_offset, a.row_offset), (b.col_offset, b.row_offset), "{what}: offsets");
        assert_eq!((a.loss.width, a.loss.height), (b.loss.width, b.loss.height), "{what}: size");
        let mut finite = 0usize;
        for (i, (x, y)) in a.loss.data.iter().zip(&b.loss.data).enumerate() {
            if x.is_finite() {
                finite += 1;
            }
            let same = x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan());
            assert!(
                same,
                "{what}: cell {i} (row {}, col {}) disc {x} ({:#010x}) vs sectored {y} ({:#010x})",
                i / a.loss.width,
                i % a.loss.width,
                x.to_bits(),
                y.to_bits()
            );
        }
        assert!(finite > 500, "{what}: only {finite} evaluated cells — fixture proves nothing");
    }

    /// The load-bearing property: the sector-chunked sweep `coverage()` now
    /// runs produces the same raster BIT FOR BIT as the disc table it
    /// replaced, over shapes that exercise the awkward parts — a transmitter
    /// off the grid centre and off the cell lattice, azimuth counts that do
    /// not divide by the sector count, wedges that straddle north (the wrap
    /// seam), budget termination leaving ragged NaN, and clutter on and off.
    #[test]
    fn sector_chunking_reproduces_the_disc_sweep_bit_for_bit() {
        let res = 30.0;
        let n = 161usize;
        let cases: [(u64, Xy, f64, usize, Option<f32>, bool); 5] = [
            // seed, tx, radius, azimuths, stop_above_db, use clutter
            (0x1234_5678, Xy { x: 2400.0, y: -2400.0 }, 1800.0, 360, None, false),
            // tx off the cell lattice: forces the ex/ey rounding that can send
            // an almost-due-north cell to ia0 = 0 through the 2π wrap.
            (0x0BAD_C0DE, Xy { x: 2411.7, y: -2387.3 }, 1500.0, 361, None, true),
            // a prime azimuth count against composite sector counts
            (0xDEAD_BEEF, Xy { x: 1800.0, y: -1800.0 }, 1200.0, 257, Some(135.0), true),
            // small azimuth count: sectors become 1–2 rows wide
            (0x5EED_1111, Xy { x: 2100.0, y: -2100.0 }, 900.0, 37, None, false),
            // transmitter near the grid edge: radials die off-grid mid-sweep
            (0x00C0_FFEE, Xy { x: 300.0, y: -2400.0 }, 1500.0, 180, Some(150.0), true),
        ];
        for (seed, tx, radius, az, stop, clut) in cases {
            let terrain = rough_terrain(n, res, seed);
            let clutter = clut.then(|| patchy_clutter(n, res, seed ^ 0xFFFF));
            // Twice per case: bare, and with both terminal corrections live.
            // The second is what makes the P.2108 terms part of the comparison
            // at all -- without it a sector that read the wrong bearing for the
            // transmitter table would still pass.
            for (label, p) in [
                ("bare", params(tx, radius, az, stop)),
                ("terminals", params_with_terminals(tx, radius, az, stop)),
            ] {
                let disc = coverage_disc(&terrain, clutter.as_ref(), &p).unwrap();
                for sectors in [1usize, 2, 3, 4, 7, 8, 16, 64, 1024] {
                    let sec = coverage_sectored(&terrain, clutter.as_ref(), &p, sectors).unwrap();
                    assert_bit_identical(&disc, &sec, &format!("seed {seed:#x}, {label}, S={sectors}"));
                }
                // And the same again through the PUBLIC entry point, whose S
                // comes from the memory budget rather than from this test —
                // squeezed until it has to split, so the default-budget S = 1
                // is not the only path that ever runs here.
                let mut tight = params(tx, radius, az, stop);
                tight.tx_terminal_db = p.tx_terminal_db.clone();
                tight.rx_terminal = p.rx_terminal;
                // Six rows: five owned plus the overlap, so S = ceil(n_az/5)
                // whatever the case's azimuth count is. A byte figure would
                // leave the small fixtures undivided and quietly stop testing
                // the public path at all.
                tight.table_budget_bytes = 6 * (radius / res).floor() as usize * 4;
                let squeezed = coverage(&terrain, clutter.as_ref(), &tight).unwrap();
                assert!(squeezed.sectors > 1, "an 8 KiB budget must force a split");
                assert_bit_identical(&disc, &squeezed, &format!("seed {seed:#x}, {label}, budgeted"));
            }
        }
    }

    /// The DEFECT the one-row overlap exists to prevent.
    ///
    /// Drop it and every sector boundary becomes a seam: a cell whose bearing
    /// falls in the last azimuth bin of a sector wants rows `ia0` and
    /// `ia0 + 1`, and `ia0 + 1` belongs to the next sector. Clamping back onto
    /// the sector's own last row silently halves that cell's gather. It is a
    /// thin artefact — one bin in n_az per boundary — which is exactly why it
    /// needs a test rather than an eyeball.
    #[test]
    fn a_sector_without_its_overlapping_azimuth_seams_at_every_boundary() {
        let res = 30.0;
        let terrain = rough_terrain(161, res, 0x1234_5678);
        let p = params(Xy { x: 2400.0, y: -2400.0 }, 1800.0, 360, None);
        let disc = coverage_disc(&terrain, None, &p).unwrap();
        let good = coverage_sectored(&terrain, None, &p, 8).unwrap();
        assert_bit_identical(&disc, &good, "with overlap");

        let seamed =
            coverage_inner(&terrain, None, &p, Some(8), false, &mut Timings::default()).unwrap();
        let differing = disc
            .loss
            .data
            .iter()
            .zip(&seamed.loss.data)
            .filter(|(a, b)| a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()))
            .count();
        assert!(
            differing > 0,
            "dropping the overlap row must corrupt the seam — if it does not, \
             the gather no longer reads ia0+1 and this design is stale"
        );
        // Order-of-magnitude sanity: the damage is confined to the boundary
        // bins, not the whole raster. Measured on this fixture: 60 of 15129
        // cells (0.40 %) with 8 sectors over 360 azimuths — small enough that
        // no eyeball would find it on a coverage map, which is the point.
        let total = disc.loss.data.len();
        println!("seam damage without the overlap row: {differing}/{total} cells");
        assert!(
            differing * 20 < total,
            "seam damage {differing}/{total} is too broad to be a seam — \
             the sector partition itself is wrong"
        );
    }

    /// The memory claim, stated as arithmetic rather than prose.
    #[test]
    fn peak_table_memory_falls_as_one_over_the_sector_count() {
        // 30 km at 5 m with the 32768-azimuth ceiling: the case that motivated
        // this file.
        let (n_az, m) = (32768usize, 6000usize);
        let disc = peak_table_bytes(n_az, m, 1);
        assert_eq!(peak_table_bytes(n_az, m, 1), (n_az + 1) * m * 4);
        assert!(
            (786_000_000..800_000_000).contains(&disc),
            "disc table {disc} B moved off its 786 MB anchor"
        );
        // Computed, not estimated: (ceil(n_az/S) + 1)·m·4 bytes.
        for (s, want_mb) in [(8usize, 98.328f64), (32, 24.600), (128, 6.168), (512, 1.560)] {
            let b = peak_table_bytes(n_az, m, s) as f64 / 1e6;
            assert!(
                (b - want_mb).abs() < 0.01,
                "S={s}: {b:.3} MB, expected {want_mb} MB"
            );
        }
        // A sector is never smaller than the two rows a gather needs.
        assert_eq!(peak_table_bytes(360, 100, 100_000), 2 * 100 * 4);
    }

    /// What the wedge bounding boxes cost in cell visits.
    ///
    /// A wedge contains the apex, so its axis-aligned box is roughly
    /// R²·|sin 2φ|/2 for a thin wedge at bearing φ — mean 1/π·R² over φ —
    /// against the disc window's 4R². The total therefore grows about
    /// LINEARLY in S with a constant near 0.08, not quadratically.
    ///
    /// Measured here (161² grid at 30 m, R = 1800 m, n_az = 360): 1.00x at
    /// S = 1, then 1.04, 1.09, 1.63, 2.52, 4.21, 7.57x at S = 2, 4, 8, 16,
    /// 32, 64. A visit that misses is cheap — a subtraction, a squared
    /// distance, a compare — but not free, and cells near the apex fall
    /// inside almost every wedge's box, so they pay the atan2 S times over.
    /// The lever that would flatten this is a per-scanline column range
    /// clipped to the WEDGE rather than to its box; the ownership test on
    /// `ia0` would keep such a refinement honest, since it may be loose.
    #[test]
    fn wedge_bounding_boxes_cost_predictable_extra_cell_visits() {
        let res = 30.0;
        let terrain = rough_terrain(161, res, 1);
        let tx = Xy { x: 2400.0, y: -2400.0 };
        let p = params(tx, 1800.0, 360, None);
        let out = coverage(&terrain, None, &p).unwrap().loss;
        let mut ratios = Vec::new();
        for s in [1usize, 2, 4, 8, 16, 32, 64] {
            let (visits, disc) = wedge_visit_cells(&out, tx, p.radius_m, 360, s);
            ratios.push((s, visits as f64 / disc as f64));
        }
        println!("wedge-window visits / disc-window cells: {ratios:?}");
        // S = 1 is the disc window itself, give or take the 2-cell pad.
        assert!(ratios[0].1 < 1.1, "S=1 visited {:.2}x the disc window", ratios[0].1);
        // Growth is linear in S with a small constant, NOT quadratic: if this
        // ever fails, the window is being computed from the wedge's chord
        // rather than its arc.
        for (s, r) in &ratios {
            assert!(
                *r < 0.12 * *s as f64 + 1.2,
                "S={s}: {r:.2}x the disc window — bounding boxes are too loose"
            );
        }
    }

    /// Not an assertion — a measurement harness. Run it explicitly:
    ///   cargo test --release -p planner-coverage sector::tests::bench_disc -- \
    ///     --ignored --nocapture
    ///
    /// Variants are INTERLEAVED and reported as min-of-N, not measured one
    /// after another: on a shared workstation the same disc sweep measured
    /// 1145 ms and 1699 ms in two consecutive back-to-back runs, so a
    /// sequential harness compares the machine's mood, not the two designs.
    #[test]
    #[ignore = "measurement, not a gate"]
    fn bench_disc_against_sectored() {
        use std::time::Instant;
        // The sweep's NATURAL geometry, at a scale that fits a unit test:
        // n_az = ceil(2πR/res) ≈ 2π·m, which is the ratio that fixes how much
        // of the run is propagation and how much is rasterising. A 10 m pack
        // at 2.5 km gives m = 250, n_az = 1571 — the same shape as the 20 km
        // case (m = 4000 at 5 m) that cost 402 MB, at a sixty-fourth the
        // compute.
        const REPS: usize = 9;
        const SECTORS: [usize; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
        let res = 10.0;
        let n = 601usize;
        let terrain = rough_terrain(n, res, 0x1234_5678);
        let clutter = patchy_clutter(n, res, 0x4321);
        let tx = Xy { x: 3000.0, y: -3000.0 };
        let p = params(tx, 2500.0, 1571, None);

        let mut disc_split = Timings::default();
        let warm = coverage_disc(&terrain, Some(&clutter), &p).unwrap();
        let m = warm.steps_per_radial;
        let n_az = warm.azimuths;
        println!(
            "grid {n}x{n} @ {res} m, radius {} m, n_az {n_az}, m {m}, min of {REPS} interleaved",
            p.radius_m
        );

        let mut disc_ms = f64::INFINITY;
        let mut best: [(f64, Timings); SECTORS.len()] =
            [(f64::INFINITY, Timings::default()); SECTORS.len()];
        for _ in 0..REPS {
            let mut dtm = Timings::default();
            let t = Instant::now();
            let r = coverage_disc_timed(&terrain, Some(&clutter), &p, &mut dtm).unwrap();
            std::hint::black_box(&r);
            let el = t.elapsed().as_secs_f64() * 1e3;
            if el < disc_ms {
                disc_ms = el;
                disc_split = dtm;
            }
            for (slot, s) in best.iter_mut().zip(SECTORS) {
                let mut tm = Timings::default();
                let t0 = Instant::now();
                let r =
                    coverage_inner(&terrain, Some(&clutter), &p, Some(s), true, &mut tm).unwrap();
                std::hint::black_box(&r);
                let el = t0.elapsed().as_secs_f64() * 1e3;
                if el < slot.0 {
                    *slot = (el, tm);
                }
            }
        }
        println!(
            "disc         : {disc_ms:8.1} ms           sweep {:7.1} raster {:6.1}  \
             table {:7.2} MB",
            disc_split.sweep_ms,
            disc_split.raster_ms,
            peak_table_bytes(n_az, m, 1) as f64 / 1e6
        );
        for ((ms, tm), s) in best.iter().zip(SECTORS) {
            let (visits, disc_cells) = wedge_visit_cells(&warm.loss, tx, p.radius_m, n_az, s);
            println!(
                "sectored S={s:<4}: {ms:8.1} ms ({:+6.1}%)  sweep {:7.1} raster {:6.1}  \
                 table {:7.2} MB  visits {:5.2}x  extra radials {:+.2}%",
                (ms / disc_ms - 1.0) * 100.0,
                tm.sweep_ms,
                tm.raster_ms,
                peak_table_bytes(n_az, m, s) as f64 / 1e6,
                visits as f64 / disc_cells as f64,
                s as f64 / n_az as f64 * 100.0
            );
        }
    }

    /// The 20 km case at its REAL polar-table size: 5 m cells, radius 20 km,
    /// n_az 25133 ⇒ m = 4000, and therefore a 402 MB disc table against
    /// 67.0 MB (S = 6) under the default budget.
    ///
    /// The terrain grid is deliberately only 2 km across, so radials leave it
    /// after ~200–280 steps and the sweep costs a second rather than minutes.
    /// The TABLE does not shrink with them — it is sized from n_az and m, not
    /// from how far the rays get — so the ALLOCATION under test is the real
    /// one while the propagation bill is not.
    ///
    /// Read the RASTER half of the split with that in mind, and do not quote
    /// it as the cost of sectoring: with the window 2 km wide and the radius
    /// 20 km, every wedge's bounding box clamps to the whole grid, so the
    /// rasteriser walks all 401² cells S times over. On a pack that actually
    /// spans the sweep the boxes are wedge-shaped, and
    /// `wedge_bounding_boxes_cost_predictable_extra_cell_visits` measures the
    /// visit ratio there: 1.09x at S = 4, 1.63x at S = 8.
    fn twenty_km_fixture() -> (Grid, Grid, CoverageParams) {
        let res = 5.0;
        let n = 401usize;
        let terrain = rough_terrain(n, res, 0x2A2A_2A2A);
        let clutter = patchy_clutter(n, res, 0x1357);
        let mut p = params(Xy { x: 1000.0, y: -1000.0 }, 20_000.0, 25_133, None);
        p.max_profile_points = MAX_PROFILE_POINTS;
        (terrain, clutter, p)
    }

    fn twenty_km_report(label: &str, sectored: bool) {
        use std::time::Instant;
        const REPS: usize = 3;
        let (terrain, clutter, p) = twenty_km_fixture();
        let mut best = f64::INFINITY;
        let mut split = Timings::default();
        let mut shape = (0usize, 0usize, 0usize);
        for _ in 0..REPS {
            let mut tm = Timings::default();
            let t = Instant::now();
            let r = if sectored {
                crate::coverage_timed(&terrain, Some(&clutter), &p, &mut tm).unwrap()
            } else {
                coverage_disc_timed(&terrain, Some(&clutter), &p, &mut tm).unwrap()
            };
            let el = t.elapsed().as_secs_f64() * 1e3;
            if el < best {
                best = el;
                split = tm;
            }
            shape = (r.azimuths, r.steps_per_radial, r.sectors);
            std::hint::black_box(&r);
        }
        let (n_az, m, s) = shape;
        println!(
            "{label:9}: {best:9.1} ms (min of {REPS})  sweep {:8.1}  raster {:7.1}  \
             n_az {n_az} m {m} S {s}  peak table {:7.1} MB",
            split.sweep_ms,
            split.raster_ms,
            peak_table_bytes(n_az, m, s) as f64 / 1e6
        );
    }

    /// Old against new on the 20 km-equivalent sweep, INTERLEAVED — the only
    /// form of this measurement worth quoting (see `bench_twenty_km_disc` for
    /// why the two single-arm probes are not).
    ///
    /// Measured, min of 5 interleaved reps, quietest of five runs on a
    /// 10-core/12-thread workstation, disc = 1244.5 ms (sweep 1123.9, raster
    /// 3.3): S = 1 −0.2 %, S = 2 +1.0 %, S = 3 +0.4 %, S = 4 +1.0 %,
    /// S = 6 +0.5 %, S = 8 +2.5 %, S = 12 +1.1 %. S = 6 is what the default
    /// 64 MiB budget picks here, and S = 1 is the noise floor: on this fixture
    /// the split costs nothing measurable, because 4189 radials per sector
    /// still saturate the cores and the six recomputed seam radials are 0.02 %
    /// of the work. Loaded runs of the same harness put S = 1 at +34 % — read
    /// those as a busy machine, not a design.
    #[test]
    #[ignore = "measurement, not a gate"]
    fn bench_twenty_km_interleaved() {
        use std::time::Instant;
        const REPS: usize = 5;
        const SECTORS: [usize; 7] = [1, 2, 3, 4, 6, 8, 12];
        let (terrain, clutter, p) = twenty_km_fixture();
        let mut disc_ms = f64::INFINITY;
        let mut disc_split = Timings::default();
        let mut best: [(f64, Timings); SECTORS.len()] =
            [(f64::INFINITY, Timings::default()); SECTORS.len()];
        for _ in 0..REPS {
            let mut dtm = Timings::default();
            let t = Instant::now();
            let r = coverage_disc_timed(&terrain, Some(&clutter), &p, &mut dtm).unwrap();
            std::hint::black_box(&r);
            let el = t.elapsed().as_secs_f64() * 1e3;
            if el < disc_ms {
                disc_ms = el;
                disc_split = dtm;
            }
            for (slot, s) in best.iter_mut().zip(SECTORS) {
                let mut tm = Timings::default();
                let t0 = Instant::now();
                let r =
                    coverage_inner(&terrain, Some(&clutter), &p, Some(s), true, &mut tm).unwrap();
                std::hint::black_box(&r);
                let el = t0.elapsed().as_secs_f64() * 1e3;
                if el < slot.0 {
                    *slot = (el, tm);
                }
            }
        }
        println!(
            "disc      : {disc_ms:8.1} ms  sweep {:8.1} raster {:6.1}",
            disc_split.sweep_ms, disc_split.raster_ms
        );
        for ((ms, tm), s) in best.iter().zip(SECTORS) {
            println!(
                "S={s:<3}     : {ms:8.1} ms ({:+6.1}%)  sweep {:8.1} raster {:6.1}  table {:6.1} MB",
                (ms / disc_ms - 1.0) * 100.0,
                tm.sweep_ms,
                tm.raster_ms,
                peak_table_bytes(25_133, 4_000, s) as f64 / 1e6
            );
        }
    }

    /// Peak RSS is a PER-PROCESS figure, so the two arms are separate tests:
    /// run each on its own and sample the peak working set of the test binary
    /// while it lives (on Windows it reads back as 0 once the process has
    /// exited).
    ///   cargo test --release -p planner-coverage bench_twenty_km_disc \
    ///     -- --ignored --nocapture --exact sector::tests::bench_twenty_km_disc
    ///
    /// Measured that way: 391 MB peak working set for the disc against 72 MB
    /// for the sectored sweep — the 402 MB and 67.0 MB tables plus ~4 MB of
    /// grids either side. Do NOT quote these two tests' WALL CLOCK against each
    /// other: separate processes cannot be interleaved, and on a shared machine
    /// the pair measured anything from −6 % to +40 % for the same code.
    /// `bench_twenty_km_interleaved` is the timing measurement.
    #[test]
    #[ignore = "measurement, not a gate"]
    fn bench_twenty_km_disc() {
        twenty_km_report("disc", false);
    }

    #[test]
    #[ignore = "measurement, not a gate"]
    fn bench_twenty_km_sectored() {
        twenty_km_report("sectored", true);
    }
}
