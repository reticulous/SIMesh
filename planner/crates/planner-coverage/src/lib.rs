//! Point-to-area coverage: radial sweep over a terrain `Grid` evaluating
//! P.1812-8 at every step via the allocation-free array API (prefix slices
//! of per-radial arrays — review §8.3: no naive per-pixel path rebuilds).
//!
//! v0 scope, tracked in TODO.md: DSM-as-terrain (SurfaceDsm bootstrap packs;
//! clutter layer joins when split packs land), all-inland zones, and a
//! per-radial full evaluation per prefix (structured for a rayon
//! par-iter-over-radials if measurements demand it).
//!
//! The polar loss table is built one SECTOR of azimuths at a time rather than
//! for the whole disc. The rasteriser's four-corner gather only ever reads two
//! ADJACENT azimuth rows, so a sector plus one overlapping row past its end
//! serves every cell whose bearing lands inside it, and the table costs
//! `(ceil(n_az/S) + 1) * m * 4` bytes instead of `n_az * m * 4`. That is a
//! memory bound, not a numerical change: `sector.rs` keeps the disc-table
//! implementation as a reference and pins the two rasters bit for bit.
//!
//! What the disc table cost, at the 32768-azimuth ceiling the web layer sets
//! and a 5 m pack: 20 km ⇒ n_az = 25133, m = 4000 ⇒ 402 MB; 30 km ⇒ n_az
//! clamps to 32768, m = 6000 ⇒ 786 MB. Under the default 64 MiB budget those
//! become 67.0 MB (S = 6) and 65.6 MB (S = 12).

pub mod environment;
pub mod gaps;
pub mod siting;

/// The disc-table sweep this one replaced, kept as a REFERENCE implementation,
/// plus the tests that hold the sectored sweep to it bit for bit. It exists in
/// exactly one configuration — `cfg(test)` — because its whole purpose is to
/// be compared against.
#[cfg(test)]
mod sector;

use planner_core::model::LinkParams;
use planner_propag::p1812::{lb_from_arrays, ArrayInputs, SurfaceMethod};
use planner_terrain::Grid;
use planner_core::geo::Xy;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoverageError {
    #[error("transmitter position outside the terrain grid")]
    TxOutsideGrid,
    #[error("radius must be positive")]
    BadRadius,
    #[error("thread pool: {0}")]
    Threads(String),
    #[error("no viable candidate sites (none reach any demand) — check the pack's population layer, radius and link budget")]
    NoCandidates,
    #[error("the propagation model rejects these link parameters, so no path could be evaluated: {0}")]
    UnusableLink(String),
    #[error("superseded by a newer request")]
    Cancelled,
}

pub struct CoverageParams {
    /// TX position in the grid's (metric) CRS.
    pub tx: Xy,
    /// HARD CAP on sweep distance — a compute budget, not a physical range.
    /// Set it at or beyond the radio horizon (≈4.12(√h_t+√h_r) km) and let
    /// `stop_above_db` end each radial where the budget actually closes;
    /// capping by distance instead truncates rural line-of-sight sites and
    /// biases any coverage comparison toward dense urban ones.
    pub radius_m: f64,
    pub link: LinkParams,
    /// Cap on the number of azimuths (default: enough for ~1 px arc at the
    /// rim, clamped to [360, 5760]).
    pub max_azimuths: Option<usize>,
    /// Stop walking a radial once loss has exceeded this for a continuous
    /// stretch (dB). Usually the link budget: past it the ray contributes
    /// nothing, so the r² cost collapses to the useful area.
    pub stop_above_db: Option<f32>,
    /// How far the loss must stay over `stop_above_db` before the radial is
    /// abandoned (metres). Must exceed the depth of clutter a path can be
    /// buried in before reopening — a node inside a city is over budget for
    /// the first few hundred metres and still reaches a hilltop 12 km out.
    /// See `DEFAULT_STOP_AFTER_M`.
    pub stop_after_m: f64,
    /// A_h at the TRANSMITTER for a given azimuth (radians), dB.
    ///
    /// P.1812 puts both terminals on bare terrain by construction, so a node
    /// below its own roofline gets a free horizon from it — this is the
    /// P.2108-1 §3.1 correction the Recommendation leaves out. It is a
    /// function of AZIMUTH and not a constant because the roof edge that
    /// shadows a balcony is metres away on one bearing and tens of metres on
    /// another; a constant offset cannot express a directional shadow, and
    /// cannot change a ranking either, since it moves every prediction by the
    /// same amount.
    ///
    /// A closure rather than data because the geometry lives in the caller's
    /// building index, which this crate must not depend on.
    pub tx_terminal_db: Option<std::sync::Arc<dyn Fn(f64) -> f32 + Send + Sync>>,
    /// The transmitter height (m above ground) P.1812 is run at for a given
    /// azimuth, when `tx_terminal_db` is applied: the representative clutter
    /// height on that bearing when the antenna is below it (P.2108 §3.1,
    /// `p2108::model_height_m`). A_h corrects a loss computed TO the clutter
    /// height; run from the antenna itself, the model charges the same
    /// obstruction twice, which for an antenna under a tall roof buries every
    /// path. `None` runs at the antenna, as a sweep without the correction does.
    pub tx_model_h_m: Option<std::sync::Arc<dyn Fn(f64) -> f64 + Send + Sync>>,
    /// The receiver end of the same correction, applied per CELL.
    ///
    /// `None` leaves the sweep as pure P.1812.
    pub rx_terminal: Option<RxTerminal>,
    /// Set true to abandon the sweep.
    ///
    /// Without this a sweep can only be waited out. That is fine when one
    /// takes a second and unacceptable when it takes minutes: dragging a
    /// receiver-height control supersedes the running sweep on every move,
    /// and a superseding request that cannot stop its predecessor either
    /// fails outright or queues behind a band that may be 400 s long. Checked
    /// once per azimuth, which is the finest granularity that costs nothing —
    /// a relaxed atomic load against a whole radial of P.1812 evaluations.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Cap on profile points handed to P.1812 per path. See
    /// `MAX_PROFILE_POINTS` for why this exists; set it to `usize::MAX` to
    /// evaluate the profile at full raster resolution (what the accuracy test
    /// compares against).
    pub max_profile_points: usize,
    /// Ceiling on the polar loss table, in BYTES. See
    /// `DEFAULT_TABLE_BUDGET_BYTES`.
    ///
    /// The sweep splits its azimuths into as few sectors as this allows and
    /// holds one sector's table at a time, so the peak is
    /// `(ceil(n_az/S) + 1) * m * 4` rather than `n_az * m * 4`. A budget too
    /// small for two adjacent rows is not refused — it is honoured as far as
    /// the gather allows (see `sectors_for_budget`).
    pub table_budget_bytes: usize,
}

/// Cap on the number of profile points handed to P.1812 for one path.
///
/// The sweep walks a radial at the pack's own cell size and evaluates the
/// model at every step, over a profile that grows with the step index. That
/// makes a radial cost O(steps^2): raising a 10 m pack's sweep cap from 8 km
/// to 20 km took one transmitter from ~3 s to ~20 s, which is not a usable
/// interaction.
///
/// A 20 km path resolved at 10 m is carrying 2000 points to describe a handful
/// of ridgelines, so decimating to a fixed budget (keeping BOTH terminals,
/// which are load bearing) makes the cost O(steps * POINTS) instead. 384
/// points is ~52 m spacing on a 20 km path and ~21 m on 8 km.
///
/// This cap is a SPEED knob and nothing more. It used to be described here as
/// harmless because "the recommendation is insensitive to spacing well below
/// the scale of the obstacles that actually diffract" — that is the opposite
/// of what P.1812-8 §3.2.1 says, and believing it is what let the sweep hand
/// the model 10 m-spaced profiles on a 10 m pack. Spacing is now floored
/// separately, by `SurfaceMethod::stride_for`, and the two strides are
/// combined below; whichever is coarser wins.
///
/// `coverage_profile_max_points_for` exists so tests can pin the decimation.
pub const MAX_PROFILE_POINTS: usize = 384;

/// What the receiver end needs for the P.2108 §3.1 correction.
///
/// R comes from the sweep's own clutter grid, which already holds the
/// representative obstruction height at every cell — the same value fed to
/// `r_rx_m`. Only the antenna height, the frequency and the distance to the
/// clutter have to be supplied.
#[derive(Debug, Clone, Copy)]
pub struct RxTerminal {
    pub freq_ghz: f64,
    pub h_agl_m: f64,
    /// Distance from the receiver to the surrounding clutter.
    ///
    /// NOMINAL at the receiver, unlike the transmitter, and the asymmetry is
    /// deliberate: the transmitter is one point and can afford a ray walk per
    /// azimuth, while a receiver exists at every cell in the sweep and a walk
    /// each would cost more than the propagation. P.2108 offers 27 m as a
    /// typical street width and that is what this defaults to. It makes the
    /// receiver-side term non-directional, which is a real limitation and is
    /// stated rather than hidden.
    pub ws_m: f64,
}

/// How the sweep builds its surface profile, and therefore which §3.2 spacing
/// floor applies: `g = h + representative clutter height` (eq. 1d) whenever a
/// clutter grid is supplied. A clutter-free grid — bare earth, or a legacy
/// DSM-as-terrain pack — is the strictly easier case for the same floor, and
/// §3.2.2 itself says spacing beyond the order of 50 m buys nothing over
/// §3.2.1, so one floor for both is honest and one fewer knob to get wrong.
const SURFACE_METHOD: SurfaceMethod = SurfaceMethod::RepresentativeClutter;

/// Default over-budget stretch before a radial is abandoned.
///
/// 2 km is chosen to clear the depth of urban clutter a path can be buried in
/// while still cutting the sweep short once terrain genuinely blocks it. The
/// previous behaviour (8 cells) was resolution-dependent and far too eager.
pub const DEFAULT_STOP_AFTER_M: f64 = 2000.0;

/// Default ceiling on the polar loss table: 64 MiB.
///
/// Chosen against the deployment target rather than the workstation. The
/// service runs on a 2-core/2 GB multi-tenant VPS and holds a pack's terrain
/// and clutter grids, the output raster and a tile cache alongside the sweep;
/// the disc table it replaced was 402 MB for a 20 km sweep on a 5 m pack and
/// 786 MB at the 30 km cap, either of which is the whole machine. 64 MiB
/// puts the TABLE an order of magnitude under the smallest box it has to run
/// on -- the table only: the output raster is not under this budget and is
/// 256 MB for a 20 km window at 5 m, which is the next thing to bound. It
/// costs almost nothing at the S it implies: on the
/// 20 km-equivalent fixture in `sector.rs` (n_az 25133, m 4000, S = 6) the
/// sweep measured +0.5 % against the disc, min of 5 interleaved reps, on a run
/// whose S = 1 arm sat at −0.2 % — i.e. inside the noise. The peak working set
/// of that test process fell from 391 MB to 72 MB.
///
/// It is a knob, not a law: a workstation batch run can raise it and get the
/// disc back with `S = 1`.
pub const DEFAULT_TABLE_BUDGET_BYTES: usize = 64 << 20;

impl CoverageParams {
    /// Sensible defaults for the optional knobs.
    pub fn new(tx: Xy, radius_m: f64, link: LinkParams) -> Self {
        Self {
            tx,
            radius_m,
            link,
            max_azimuths: None,
            cancel: None,
            tx_terminal_db: None,
            tx_model_h_m: None,
            rx_terminal: None,
            stop_above_db: None,
            stop_after_m: DEFAULT_STOP_AFTER_M,
            max_profile_points: MAX_PROFILE_POINTS,
            table_budget_bytes: DEFAULT_TABLE_BUDGET_BYTES,
        }
    }
}

pub struct CoverageResult {
    /// Basic transmission loss L_b (dB) per pixel; NaN where not evaluated
    /// (outside the sweep radius, or off-grid). Inside P.1812's 0.25 km floor
    /// the near-field model answers instead, so those cells are NOT NaN.
    ///
    /// Covers ONLY the radius window around the transmitter, not the whole
    /// input grid — a region-sized output per candidate cost ~294 MB on the
    /// Berlin+Brandenburg pack and made batch siting infeasible. Use
    /// `col_offset`/`row_offset` to map back to parent-grid indices.
    pub loss: Grid,
    /// Position of the window's first column/row in the input grid.
    pub col_offset: usize,
    pub row_offset: usize,
    pub azimuths: usize,
    pub steps_per_radial: usize,
    /// Azimuth sectors the polar table was split into (1 = the whole disc).
    /// Reported so a caller can check what its memory budget bought without
    /// re-deriving it — the peak table was `peak_table_bytes(azimuths,
    /// steps_per_radial, sectors)`.
    pub sectors: usize,
}

impl CoverageResult {
    /// Loss at a pixel of the INPUT grid; NaN outside the computed window.
    pub fn loss_at(&self, row: usize, col: usize) -> f32 {
        if row < self.row_offset || col < self.col_offset {
            return f32::NAN;
        }
        let (lr, lc) = (row - self.row_offset, col - self.col_offset);
        if lr >= self.loss.height || lc >= self.loss.width {
            return f32::NAN;
        }
        self.loss.data[lr * self.loss.width + lc]
    }
}

/// Peak bytes held by the polar loss table for a sector-chunked sweep.
///
/// `sectors = 1` is the whole disc: one sector spanning every azimuth, plus
/// its (then redundant) wrap row. Sectors are computed ONE AT A TIME —
/// parallelism lives inside a sector, over its radials — because S tables
/// alive at once would give back exactly what the split is for.
/// Public on purpose: the number an operator or a budget-setting caller needs
/// to reason about `table_budget_bytes`, and the one the tests pin.
pub fn peak_table_bytes(n_az: usize, m: usize, sectors: usize) -> usize {
    let s = sectors.clamp(1, n_az.max(1));
    // The largest sector under the `s*n_az/S` split is ceil(n_az/S) rows, and
    // every sector carries one overlapping row past its end.
    (n_az.div_ceil(s) + 1) * m * 4
}

/// FEWEST sectors whose peak table fits `budget_bytes`.
///
/// Fewest, because every extra sector costs: one more parallel barrier, one
/// more radial recomputed at the seam, and one more wedge bounding box walked
/// over the raster. Measured by `sector::tests::bench_disc_against_sectored`
/// on a 601² 10 m fixture (n_az 1571, m 250, min of 9 interleaved reps,
/// quietest of four runs): +0.1 % of the disc sweep at S = 1, +1.6 % at S = 4,
/// +6.2 % at S = 8, +33.9 % at S = 64. So the sweep splits only as far as the
/// budget forces it, and returns 1 whenever the whole disc fits.
///
/// A budget too small for even two rows cannot be honoured: two ADJACENT rows
/// are what one bilinear gather reads, so `n_az` sectors (2 rows each) is the
/// floor. The caller gets that rather than a refusal — a coverage answer that
/// overruns a soft budget beats no answer at all.
pub(crate) fn sectors_for_budget(n_az: usize, m: usize, budget_bytes: usize) -> usize {
    if n_az == 0 || m == 0 {
        return 1;
    }
    // Rows a sector may hold, INCLUDING its overlapping row; one row is
    // m * 4 bytes.
    let rows = budget_bytes / (m * 4);
    let owned = rows.saturating_sub(1);
    if owned == 0 {
        return n_az;
    }
    if owned >= n_az {
        return 1;
    }
    // ceil(n_az / S) <= owned holds for S = ceil(n_az / owned).
    n_az.div_ceil(owned)
}

/// Half-open azimuth range `[lo, hi)` sector `s` OWNS. The row it also
/// computes but does not own is `hi % n_az`.
fn sector_range(n_az: usize, sectors: usize, s: usize) -> (usize, usize) {
    (s * n_az / sectors, (s + 1) * n_az / sectors)
}

/// Inclusive cell window of the output raster that can contain any point of
/// the circular wedge spanning azimuth indices `[a_lo, a_hi]`.
///
/// This is a VISIT FILTER, not a correctness boundary — the per-cell `ia0`
/// test inside the rasteriser decides ownership. It is padded by two cells so
/// that the ~1e-12 m of disagreement between `ang/2π·n_az` and `2π·a/n_az` at
/// a wedge edge, and the exactly-north cell whose `ex` rounds to a tiny
/// NEGATIVE and therefore wraps to `ia0 = 0`, are both inside it with a margin
/// of ~12 orders of magnitude.
///
/// Returns `None` when the wedge misses the raster entirely.
fn wedge_window(
    out: &Grid,
    tx: Xy,
    radius_m: f64,
    n_az: usize,
    a_lo: usize,
    a_hi: usize,
) -> Option<(usize, usize, usize, usize)> {
    // A margin, not a measurement: with PAD_CELLS = 0 the whole suite and an
    // independent 18-fixture old-vs-new oracle (off-lattice transmitter,
    // transmitter on the grid corner included) stayed bit-identical. The two
    // cells cover the ~1e-12 m wedge-edge disagreement between the bearing
    // test here and the rasteriser's atan2, which nothing in the tree has yet
    // demonstrated crossing a cell boundary. Cheap, and kept until it is.
    const PAD_CELLS: i64 = 2;
    let two_pi = 2.0 * std::f64::consts::PI;
    let t0 = two_pi * a_lo as f64 / n_az as f64;
    let t1 = two_pi * a_hi as f64 / n_az as f64;
    // Bearing θ measured from north, clockwise: direction (sin θ, cos θ). The
    // wedge contains its apex, so 0 is always a candidate extreme.
    let spans = |target: f64| t0 <= target && target <= t1;
    let (mut xmin, mut xmax) = (0.0f64, 0.0f64);
    let (mut ymin, mut ymax) = (0.0f64, 0.0f64);
    for t in [t0, t1] {
        xmin = xmin.min(t.sin());
        xmax = xmax.max(t.sin());
        ymin = ymin.min(t.cos());
        ymax = ymax.max(t.cos());
    }
    if spans(std::f64::consts::FRAC_PI_2) {
        xmax = 1.0;
    }
    if spans(3.0 * std::f64::consts::FRAC_PI_2) {
        xmin = -1.0;
    }
    if spans(0.0) || spans(two_pi) {
        ymax = 1.0;
    }
    if spans(std::f64::consts::PI) {
        ymin = -1.0;
    }

    let axis = |lo: f64, hi: f64, origin: f64, step: f64, n: usize| -> Option<(usize, usize)> {
        let (a, b) = ((lo - origin) / step, (hi - origin) / step);
        let (a, b) = (a.min(b), a.max(b));
        let i0 = (a.floor() as i64 - PAD_CELLS).max(0);
        let i1 = (b.ceil() as i64 + PAD_CELLS).min(n as i64 - 1);
        if i0 > i1 {
            None
        } else {
            Some((i0 as usize, i1 as usize))
        }
    };
    let (c0, c1) = axis(
        tx.x + radius_m * xmin,
        tx.x + radius_m * xmax,
        out.origin.x,
        out.dx_m,
        out.width,
    )?;
    let (r0, r1) = axis(
        tx.y + radius_m * ymin,
        tx.y + radius_m * ymax,
        out.origin.y,
        out.dy_m,
        out.height,
    )?;
    Some((r0, r1, c0, c1))
}

/// Scratch a worker reuses across every step of every radial it walks.
/// Allocating per evaluation would hand back the time the decimation saves.
pub(crate) struct RadialScratch {
    h: Vec<f64>,
    g: Vec<f64>,
    clut: Vec<f64>,
    sd: Vec<f64>,
    sh: Vec<f64>,
    sg: Vec<f64>,
}

impl RadialScratch {
    pub(crate) fn new(m: usize, cap: usize) -> Self {
        let c = cap.min(m + 1);
        Self {
            h: Vec::with_capacity(m + 1),
            g: Vec::with_capacity(m + 1),
            clut: Vec::with_capacity(m + 1),
            sd: Vec::with_capacity(c),
            sh: Vec::with_capacity(c),
            sg: Vec::with_capacity(c),
        }
    }
}

/// ONE radial of the sweep, written into `row` (length m, indexed `k - 1`).
///
/// The single copy of the kernel: the sectored sweep below and the reference
/// disc sweep in `sector.rs` both call this, so the bit-identity test they
/// share compares the two TABLE LAYOUTS and nothing else. It used to exist
/// twice — inline here and transcribed there — and a review that mutated the
/// terminal terms in one copy showed how quietly the two could diverge.
///
/// Both radial termination rules live here, and only here: `stop_above_db`
/// with `stop_after_m`, and the nodata/off-grid break in the walk.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_radial(
    terrain: &Grid,
    clutter: Option<&Grid>,
    p: &CoverageParams,
    res: f64,
    m: usize,
    n_az: usize,
    d_km: &[f64],
    az: usize,
    row: &mut [f32],
    s: &mut RadialScratch,
) {
    let ang = 2.0 * std::f64::consts::PI * az as f64 / n_az as f64;
    let (dx, dy) = (ang.sin(), ang.cos()); // az 0 = north, clockwise
    // The transmitter's own surroundings cost the same on every cell of this
    // radial — the roof edge that shadows it does not move along the ray — so
    // it is evaluated once here rather than per step.
    let tx_ah = p.tx_terminal_db.as_ref().map(|f| f(ang)).unwrap_or(0.0);
    // And the height the model is run at on this bearing, which goes with it:
    // raised to the clutter the correction was measured against.
    let tx_h = p.tx_model_h_m.as_ref().map(|f| f(ang)).unwrap_or(p.link.tx_h_agl_m);
    let raised;
    let link = if tx_h != p.link.tx_h_agl_m {
        raised = LinkParams { tx_h_agl_m: tx_h, ..p.link.clone() };
        &raised
    } else {
        &p.link
    };
    let (h, g, clut) = (&mut s.h, &mut s.g, &mut s.clut);
    h.clear();
    g.clear();
    clut.clear();
    let mut valid = 0usize;
    for k in 0..=m {
        let pt = Xy {
            x: p.tx.x + dx * k as f64 * res,
            y: p.tx.y + dy * k as f64 * res,
        };
        match terrain.sample_bilinear(pt) {
            // A nodata sample ends the radial exactly like leaving the grid
            // does. It must NOT be walked over: P.1812's internals are full of
            // max/min clamps, and `NaN.max(x)` is `x` in Rust, so a hole in the
            // DTM does not poison the answer — it comes back as a plausible
            // finite loss computed over ground nobody measured. Measured: a
            // single nodata column between a site and a settlement 900 m away
            // still reported 91 dB, i.e. fully covered. Stopping here leaves
            // NaN, which the callers already read as "not evaluated" rather
            // than "no signal".
            Some(v) if v.is_finite() => {
                let c = clutter
                    .and_then(|cg| cg.sample_bilinear(pt))
                    .unwrap_or(0.0)
                    .max(0.0) as f64;
                h.push(v as f64);
                g.push(v as f64 + c);
                clut.push(c);
                valid = k;
            }
            _ => break,
        }
    }
    if valid < 2 {
        return;
    }
    // Stride the coarse profile is built on. Fixed for the whole radial so the
    // prefix only ever grows. TWO independent floors, coarser wins:
    //
    //  - the point budget (MAX_PROFILE_POINTS), which is about speed;
    //  - the §3.2.1 spacing floor, which is about the answer being valid at
    //    all. The radial has to be WALKED at the cell size because the step
    //    index is also the output raster's range index, but handing those
    //    samples to the model puts an intermediate profile point one cell from
    //    the transmitter, where eq. (13)'s 1/d_i divisor gives it a 100
    //    (m/km)-per-metre grip on the antenna height — on a 10 m pack that is
    //    the same near-mast artefact `link.json` shipped at 5 m, at half the
    //    leverage. Decimate for the model, keep every sample for the raster.
    let cap = p.max_profile_points.max(3);
    let stride = ((valid + 1).div_ceil(cap)).max(SURFACE_METHOD.stride_for(res));
    let mut cur_count = usize::MAX;
    let mut over_budget_run = 0usize;
    for k in 2..=valid {
        let d_total = d_km[k];
        if d_total < 0.25 {
            // P.1812 REFUSES anything under 0.25 km (§1), and this used to
            // skip the cell — leaving a hole around every transmitter that the
            // map drew as "not evaluated" and the gap census counted as
            // SERVED. True in open country; in a city the first 250 m is the
            // courtyard wall, the block opposite and the node's own
            // Dachgeschoss, which is the geometry that decides whether a mesh
            // node hears its neighbour at all.
            //
            // The FULL-RESOLUTION prefix, deliberately, not the decimated one:
            // the decimation exists to satisfy §3.2's spacing floor for
            // P.1812, and at 60 m it leaves two points — too few to have an
            // obstacle between them. This model has no such floor and wants
            // every sample it can get.
            if let Some(lb0) =
                planner_propag::near_field::loss_db(&planner_propag::near_field::NearFieldPath {
                    d_km: &d_km[..=k],
                    h_masl: &h[..=k],
                    g_masl: &g[..=k],
                    f_mhz: link.freq_mhz,
                    tx_h_agl_m: link.tx_h_agl_m,
                    rx_h_agl_m: link.rx_h_agl_m,
                })
            {
                let rx_ah = p
                    .rx_terminal
                    .and_then(|t| {
                        planner_propag::p2108::height_gain_correction(
                            t.freq_ghz,
                            t.h_agl_m,
                            clut[k],
                            t.ws_m,
                            planner_propag::p2108::TerminalClutter::Obstructed,
                        )
                    })
                    .unwrap_or(0.0) as f32;
                row[k - 1] = lb0 as f32 + tx_ah + rx_ah;
            }
            continue;
        }
        // Decimate the profile to a bounded number of points (see
        // MAX_PROFILE_POINTS). Both terminals are always kept: P.1812 reads
        // the endpoints for terminal heights and clutter, so dropping either
        // changes the answer rather than its precision.
        //
        // The decimated arrays are built on a FIXED stride and reused across
        // steps, with only the receiver terminal rewritten each time.
        // Re-deriving all `cap` points per step (the obvious way) costs O(cap)
        // per evaluation and gave back most of the speed the decimation was
        // there to win — measured 19.6 s -> 13.0 s instead of the ~7 s the
        // arithmetic predicts. Rebuilding only when the prefix actually grows
        // makes the setup amortized O(1).
        let (pd, ph, pg) = if stride == 1 {
            // Cell size already meets the floor, and stride 1 implies the
            // whole prefix fits the budget (otherwise the budget stride above
            // would exceed 1), so the radial arrays serve directly.
            (&d_km[..=k], &h[..=k], &g[..=k])
        } else {
            // Coarse points at 0, stride, 2*stride, ..., then the exact
            // receiver appended. The last coarse index is (k/stride − 1)
            // *stride rather than the nearest one below k, so the FINAL
            // interval is stride..2*stride−1 cells instead of possibly a
            // single cell: without that the receiver end reacquires the
            // near-terminal artefact through eq. (17)'s 1/(d − d_i), which is
            // just as sharp on rx_h as eq. (13) is on tx_h. The count still
            // advances once per stride, so the rebuild below stays amortized
            // O(1) per step.
            let count = k / stride;
            if count < 2 {
                // No room for an intermediate point at the required spacing
                // (n ≥ 3, §3.2). Leave the pixel unevaluated rather than
                // answer from a profile the Rec disowns.
                continue;
            }
            if count != cur_count {
                s.sd.clear();
                s.sh.clear();
                s.sg.clear();
                for i in 0..count {
                    let idx = i * stride;
                    s.sd.push(d_km[idx]);
                    s.sh.push(h[idx]);
                    s.sg.push(g[idx]);
                }
                // Placeholder for the terminal, overwritten every step.
                s.sd.push(0.0);
                s.sh.push(0.0);
                s.sg.push(0.0);
                cur_count = count;
            }
            let last = s.sd.len() - 1;
            s.sd[last] = d_km[k];
            s.sh[last] = h[k];
            s.sg[last] = g[k];
            (&s.sd[..], &s.sh[..], &s.sg[..])
        };
        let x = ArrayInputs {
            d_km: pd,
            h_masl: ph,
            // Surface profile (1c); terminal entries are never read (eq. 1d
            // semantics live in the diffraction internals).
            g_masl: pg,
            surface_method: SURFACE_METHOD,
            omega: 0.0,
            dct_km: d_total,
            dcr_km: d_total,
            d_tm_km: d_total,
            d_lm_km: d_total,
            // Representative clutter at the receiver location (eq. 65).
            r_rx_m: clut[k],
        };
        if let Ok(loss) = lb_from_arrays(&x, link) {
            // P.1812 + the terminal surroundings it does not model. Kept out
            // of `loss.lb_db` itself so that number stays exactly what the
            // Recommendation says.
            let rx_ah = p
                .rx_terminal
                .and_then(|t| {
                    planner_propag::p2108::height_gain_correction(
                        t.freq_ghz,
                        t.h_agl_m,
                        clut[k],
                        t.ws_m,
                        planner_propag::p2108::TerminalClutter::Obstructed,
                    )
                })
                .unwrap_or(0.0) as f32;
            let lb = loss.lb_db as f32 + tx_ah + rx_ah;
            row[k - 1] = lb;
            // Terminate this radial once the budget is clearly gone.
            //
            // The run is measured in METRES, not cells. Loss along a real path
            // is badly non-monotonic: a household node inside city clutter goes
            // over budget within a few hundred metres, and then the path
            // REOPENS towards an elevated site kilometres away — the
            // Teufelsberg case, a link that exists in the field at 12.8 km.
            // Counting 8 cells meant 240 m on a 30 m pack and only 80 m on a
            // 10 m one, so radials died in the near field and long links to
            // high ground were never computed. Requiring a continuous
            // over-budget stretch keeps the compute saving in open terrain
            // without amputating the paths that matter.
            if let Some(limit) = p.stop_above_db {
                if lb > limit {
                    over_budget_run += 1;
                    if over_budget_run as f64 * res >= p.stop_after_m {
                        break;
                    }
                } else {
                    over_budget_run = 0;
                }
            }
        }
    }
}

/// Where a sweep's wall time went. The split is worth reporting because the
/// two halves scale in OPPOSITE directions with the sector count: the radial
/// sweep pays S parallel barriers and S recomputed seam radials, while the
/// rasteriser pays the overlap between S wedge bounding boxes.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Timings {
    /// Radial evaluation — P.1812 over every azimuth of every sector.
    pub sweep_ms: f64,
    /// The polar-to-raster gather.
    pub raster_ms: f64,
}

/// Run the sweep. `terrain` (bare-earth) is sampled bilinearly along rays at
/// its own resolution; `clutter` (representative heights above ground, same
/// CRS) feeds P.1812's surface profile g = h + clutter and the terminal
/// clutter correction — None means open ground (or a legacy SurfaceDsm pack
/// used as terrain). The output raster shares the terrain grid's geometry.
///
/// The azimuths are swept in sectors sized by `p.table_budget_bytes`; the
/// raster is the same, bit for bit, whatever that budget is.
pub fn coverage(
    terrain: &Grid,
    clutter: Option<&Grid>,
    p: &CoverageParams,
) -> Result<CoverageResult, CoverageError> {
    coverage_timed(terrain, clutter, p, &mut Timings::default())
}

/// `coverage` with the sweep/raster split reported. Same answer, same cost —
/// two `Instant::now()` calls per sector.
pub(crate) fn coverage_timed(
    terrain: &Grid,
    clutter: Option<&Grid>,
    p: &CoverageParams,
    timings: &mut Timings,
) -> Result<CoverageResult, CoverageError> {
    coverage_inner(terrain, clutter, p, None, true, timings)
}

/// The sweep proper.
///
/// `sectors = None` sizes the split from `p.table_budget_bytes`; `Some(s)` is
/// for the tests that have to pin a particular split.
///
/// `overlap = false` drops each sector's one extra azimuth row and clamps the
/// gather's `ia1` back onto the sector's last OWNED row. That is the seam bug
/// this design exists to avoid, and a test pins it: the switch is here so the
/// bug can be reproduced on demand rather than reasoned about.
fn coverage_inner(
    terrain: &Grid,
    clutter: Option<&Grid>,
    p: &CoverageParams,
    sectors: Option<usize>,
    overlap: bool,
    timings: &mut Timings,
) -> Result<CoverageResult, CoverageError> {
    *timings = Timings::default();
    if p.radius_m <= 0.0 {
        return Err(CoverageError::BadRadius);
    }
    let res = terrain.dx_m.abs();
    if terrain.sample_bilinear(p.tx).is_none() {
        return Err(CoverageError::TxOutsideGrid);
    }
    // The per-step evaluation in `fill_radial` drops a failed `lb_from_arrays`
    // on the floor, which is right for one awkward path and catastrophic for a
    // parameter set the model refuses outright: P.1812 validates frequency and
    // the time/location percentages per CALL, so a bad one fails every step
    // and the sweep returns an all-NaN raster that reads exactly like "this
    // transmitter reaches nobody". Ask once, up front, and say which it is.
    crate::gaps::probe_link(&p.link)?;

    let m = (p.radius_m / res).floor() as usize; // steps per radial (excl. TX)
    let n_az = p
        .max_azimuths
        .unwrap_or_else(|| ((2.0 * std::f64::consts::PI * p.radius_m / res).ceil() as usize).clamp(360, 5760));
    let d_km: Vec<f64> = (0..=m).map(|k| k as f64 * res / 1000.0).collect();

    // Output window, allocated up front so each sector paints its own wedge
    // into it. Covers only the radius window around the transmitter — a
    // region-sized output per candidate cost ~294 MB on the Berlin+Brandenburg
    // pack and made batch siting infeasible.
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

    let sectors = sectors
        .unwrap_or_else(|| sectors_for_budget(n_az, m, p.table_budget_bytes))
        .clamp(1, n_az.max(1));
    let two_pi = 2.0 * std::f64::consts::PI;
    use rayon::prelude::*;

    for s in 0..sectors {
        // Abandon the moment a newer request supersedes this one. A cancelled
        // sweep must not return a HALF-FILLED raster: the rows the workers
        // skipped are still NaN, which the consumer reads as "not evaluated"
        // and paints as a gap — a plausible-looking coverage map with wedges
        // missing, and nothing in the result to say so.
        if let Some(c) = &p.cancel {
            if c.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(CoverageError::Cancelled);
            }
        }
        let (a_lo, a_hi) = sector_range(n_az, sectors, s);
        // Rows the sector holds: its own [a_lo, a_hi), plus the single
        // overlapping azimuth a_hi (wrapping to 0 for the last sector) that
        // the gather needs when ia0 = a_hi - 1.
        let rows = (a_hi - a_lo) + usize::from(overlap);
        let mut table = vec![f32::NAN; rows * m];
        let t_sweep = std::time::Instant::now();

        // Parallel over the sector's RADIALS: radials are independent, so this
        // is the natural parallel axis — one task per azimuth, each writing
        // only its own row. Sectors themselves stay sequential, because S
        // tables alive at once is the disc allocation again.
        table.par_chunks_mut(m).enumerate().for_each_init(
            || RadialScratch::new(m, p.max_profile_points),
            |scratch, (i, row)| {
                // Checked per AZIMUTH, which is the finest granularity that
                // costs nothing — a relaxed atomic load against a whole radial
                // of P.1812 evaluations, and rayon has already handed this
                // worker the row.
                if let Some(c) = &p.cancel {
                    if c.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                }
                let az = (a_lo + i) % n_az;
                fill_radial(terrain, clutter, p, res, m, n_az, &d_km, az, row, scratch);
            },
        );
        timings.sweep_ms += t_sweep.elapsed().as_secs_f64() * 1e3;

        // Rasterize this sector's wedge, interpolating BILINEARLY in polar
        // space (between the two bracketing azimuths and the two bracketing
        // range steps).
        //
        // Snapping each pixel to the nearest radial and nearest step makes the
        // loss field piecewise-constant on a polar grid, which reads as blocks
        // and spokes no matter how fine the terrain is — the artefact survives
        // even a 1 m pack, because it comes from the sweep geometry rather
        // than the data. Interpolating costs three extra table reads per pixel
        // and no extra propagation work.
        let t_raster = std::time::Instant::now();
        if let Some((wr0, wr1, wc0, wc1)) = wedge_window(&out, p.tx, p.radius_m, n_az, a_lo, a_hi) {
            let (o_origin, o_dx, o_dy) = (out.origin, out.dx_m, out.dy_m);
            let table = &table;
            // Row-parallel, as the disc rasteriser was: up to 64 M cells at
            // 20 km on a 5 m pack, the table read-only, each row written by
            // exactly one thread.
            out.data[wr0 * ow..(wr1 + 1) * ow]
                .par_chunks_mut(ow)
                .enumerate()
                .for_each(|(i, orow)| {
                    let y = o_origin.y + (wr0 + i) as f64 * o_dy;
                    for col in wc0..=wc1 {
                        let x = o_origin.x + col as f64 * o_dx;
                        let (ex, ey) = (x - p.tx.x, y - p.tx.y);
                        let dist = (ex * ex + ey * ey).sqrt();
                        // Only the radius bounds this. The 0.25 km exclusion
                        // that used to sit here mirrored P.1812's validity
                        // floor, and with the table empty inside it that was
                        // the honest thing to do. The table is no longer empty
                        // there — the near-field model fills it — so
                        // discarding those cells would throw away the answer
                        // after computing it, and leave the hole around every
                        // transmitter that that change exists to close.
                        if dist > p.radius_m {
                            continue;
                        }
                        let kf = dist / res;
                        if kf < 1.0 || kf > m as f64 {
                            continue;
                        }
                        let mut ang = ex.atan2(ey); // 0 = north, clockwise
                        if ang < 0.0 {
                            ang += two_pi;
                        }
                        let azf = ang / two_pi * n_az as f64;
                        let a0 = azf.floor();
                        let ia0 = a0 as usize % n_az;
                        // Ownership: exactly one sector's half-open range
                        // contains ia0, so every cell is rasterised exactly
                        // once and from the same two rows the disc gather
                        // would have used. The wedge box above is only a visit
                        // filter and may be loose; this is what makes it safe.
                        if ia0 < a_lo || ia0 >= a_hi {
                            continue;
                        }
                        let la0 = ia0 - a_lo;
                        let la1 = if overlap { la0 + 1 } else { (la0 + 1).min(rows - 1) };
                        let ta = azf - a0;
                        let k0 = kf.floor().max(1.0);
                        let (ik0, ik1) = (k0 as usize, (k0 as usize + 1).min(m));
                        let tk = kf - k0;

                        // Weighted mean over whatever of the four corners is
                        // defined. A radial that stopped early (budget
                        // termination) leaves NaN, and averaging it in would
                        // bleed "no signal" across the boundary.
                        let mut acc = 0.0f64;
                        let mut wsum = 0.0f64;
                        for (la, wa) in [(la0, 1.0 - ta), (la1, ta)] {
                            for (ik, wk) in [(ik0, 1.0 - tk), (ik1, tk)] {
                                let v = table[la * m + (ik - 1)];
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
        }
        timings.raster_ms += t_raster.elapsed().as_secs_f64() * 1e3;
        drop(table); // explicit: the whole point is that it does not survive
    }

    // A sweep cancelled after its last sector still has NaN wedges in it.
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
        sectors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use planner_core::model::PathLossModel;
    use planner_core::profile::{ClutterClass, Profile, ProfilePoint, Zone};
    use planner_propag::P1812;

    fn flat_grid(size: usize, res: f64, h: f32) -> Grid {
        Grid::with_axes(
            Xy { x: 0.0, y: 0.0 },
            res,
            -res,
            size,
            size,
            vec![h; size * size],
        )
        .unwrap()
    }

    fn params() -> CoverageParams {
        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 868.0;
        link.tx_h_agl_m = 12.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        CoverageParams {
            tx_terminal_db: None,
            tx_model_h_m: None,
            rx_terminal: None,
            cancel: None,
            tx: Xy { x: 0.0, y: -3000.0 }, // center-ish of a north-up grid
            radius_m: 2500.0,
            link,
            max_azimuths: Some(360),
            stop_above_db: None,
            stop_after_m: DEFAULT_STOP_AFTER_M,
                max_profile_points: crate::MAX_PROFILE_POINTS,
                table_budget_bytes: DEFAULT_TABLE_BUDGET_BYTES,
        }
    }

    /// The sweep's own version of the transmitter-height cliff.
    ///
    /// A 10 m pack, a mast standing in a 15 m urban cell, and a path that runs
    /// over a low 5 m canopy — nothing between the terminals obstructs a mast
    /// at ~15 m. Walking the radial at the cell size and handing those samples
    /// straight to P.1812 put an intermediate profile point 10 m from the mast
    /// still carrying the mast's own cell, where eq. (13)'s 1/d_i divisor gave
    /// it 100 (m/km) of slope per metre of antenna height. The result was tens
    /// of dB of phantom loss that vanished the moment the antenna cleared its
    /// own roof — on a path that was never obstructed at all.
    ///
    /// This is the raster half of the defect; the model half is pinned in
    /// planner-propag. Both are needed: the sweep is what feeds the coverage
    /// rasters and the gap census, and it has to decimate for the model while
    /// still stepping the raster at the cell size.
    #[test]
    fn coverage_loss_does_not_jump_when_the_mast_clears_its_own_clutter_cell() {
        let n = 301usize;
        let res = 10.0; // the Berlin pack's resolution — where this bit
        let terrain = flat_grid(n, res, 62.0);
        let tx = Xy { x: 1500.0, y: -1500.0 };
        // 5 m canopy everywhere, 15 m in the mast's own cell and its
        // immediate neighbours (one 10 m cell either way, which is what a
        // bilinear read a few metres down the path actually sees).
        let mut c = vec![5f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let p = Xy { x: col as f64 * res, y: -(row as f64) * res };
                if (p.x - tx.x).abs() <= res && (p.y - tx.y).abs() <= res {
                    c[row * n + col] = 15.0;
                }
            }
        }
        let clutter = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, c).unwrap();

        let mut link = LinkParams::eu868_defaults();
        link.rx_h_agl_m = 12.0;
        link.loc_pct = 50.0;
        let at = |tx_h: f64| -> f64 {
            let mut l = link.clone();
            l.tx_h_agl_m = tx_h;
            let p = CoverageParams {
                tx_terminal_db: None,
                tx_model_h_m: None,
                rx_terminal: None,
                cancel: None,
                tx,
                radius_m: 1300.0,
                link: l,
                max_azimuths: Some(8),
                stop_above_db: None,
                stop_after_m: DEFAULT_STOP_AFTER_M,
                max_profile_points: crate::MAX_PROFILE_POINTS,
                table_budget_bytes: DEFAULT_TABLE_BUDGET_BYTES,
            };
            let r = coverage(&terrain, Some(&clutter), &p).unwrap();
            // Due north of the transmitter at ~1.15 km.
            let col = ((tx.x - terrain.origin.x) / terrain.dx_m).round() as usize;
            let row = ((tx.y + 1150.0 - terrain.origin.y) / terrain.dy_m).round() as usize;
            let lb = r.loss_at(row, col);
            assert!(lb.is_finite(), "sample pixel not evaluated at tx_h {tx_h}");
            lb as f64
        };

        // Sweep the mast across its own cell's 15 m clutter height.
        let mut prev: Option<(f64, f64)> = None;
        let mut worst = 0.0f64;
        let mut th = 14.5;
        while th <= 15.1001 {
            let lb = at(th);
            if let Some((ph, pl)) = prev {
                assert!(lb <= pl + 1e-3, "L_b rose with tx_h: {pl:.3} -> {lb:.3}");
                worst = worst.max(((lb - pl) / (th - ph)).abs());
            }
            prev = Some((th, lb));
            th += 0.1;
        }
        // Measured on this fixture: 0.0001 dB/m decimated to §3.2.1's floor,
        // against 10.64 dB/m when the 10 m samples went to the model directly
        // (and that is a 0.1 m secant — the true peak is far steeper).
        assert!(worst < 1.0, "|dL_b/dh_tx| reached {worst:.2} dB/m over 14.5–15.1 m");
    }

    /// The Teufelsberg case, in miniature.
    ///
    /// A transmitter buried in tall near-field clutter blows the link budget
    /// within a few hundred metres, then the path reopens at range. The sweep
    /// must still evaluate the far ground: terminating on a short over-budget
    /// run amputated exactly the long links that make a mesh worth planning
    /// (reported from the field at 12.8 km, invisible on the map).
    #[test]
    fn a_radial_survives_near_field_clutter_and_reaches_open_ground() {
        let n = 201usize;
        let res = 30.0;
        let terrain = flat_grid(n, res, 40.0);
        // Clutter: a 25 m wall for the first ~600 m around the TX, clear after.
        let tx = Xy { x: 3000.0, y: -3000.0 };
        let mut c = vec![0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let p = Xy { x: col as f64 * res, y: -(row as f64) * res };
                let d = ((p.x - tx.x).powi(2) + (p.y - tx.y).powi(2)).sqrt();
                if d > 250.0 && d < 600.0 {
                    c[row * n + col] = 25.0;
                }
            }
        }
        let clutter = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, c).unwrap();

        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 868.0;
        link.tx_h_agl_m = 12.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        let mk = |stop_after_m: f64| CoverageParams {
            tx_terminal_db: None,
            tx_model_h_m: None,
            rx_terminal: None,
            cancel: None,
            tx,
            radius_m: 2500.0,
            link: link.clone(),
            max_azimuths: Some(180),
            // A budget the near-field wall blows through.
            stop_above_db: Some(120.0),
            stop_after_m,
            max_profile_points: MAX_PROFILE_POINTS,
            table_budget_bytes: DEFAULT_TABLE_BUDGET_BYTES,
        };
        let evaluated = |p: &CoverageParams| {
            let r = coverage(&terrain, Some(&clutter), p).unwrap();
            r.loss.data.iter().filter(|v| v.is_finite()).count()
        };
        // 240 m of slack is the old 8-cells-at-30-m behaviour.
        let eager = evaluated(&mk(240.0));
        let patient = evaluated(&mk(DEFAULT_STOP_AFTER_M));
        assert!(
            patient > eager,
            "the patient sweep must reach ground the eager one abandoned \
             (eager {eager}, patient {patient})"
        );
    }

    /// The decimation in `MAX_PROFILE_POINTS` is a speed/accuracy trade, so it
    /// has to be measured rather than asserted. On a long path over real
    /// relief, evaluating P.1812 on 384 profile points must agree with
    /// evaluating it on every one of the raster's ~1300.
    ///
    /// If this test starts failing, the honest fix is to RAISE the cap, not to
    /// widen the tolerance: the whole point is that the decimation is invisible
    /// in the answer.
    #[test]
    fn decimating_the_profile_does_not_change_the_predicted_loss() {
        // Rolling terrain with features at several scales, so decimation has
        // something it could plausibly smooth away: a broad ridge, a sharp
        // knife edge, and fine roughness at the cell size.
        let n = 401usize;
        let res = 30.0;
        let mut data = vec![0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let x = col as f64 * res;
                let y = row as f64 * res;
                let broad = 60.0 * ((x / 4000.0).sin() + (y / 5000.0).cos());
                let knife = if (col as i64 - 260).abs() < 2 { 90.0 } else { 0.0 };
                let rough = 4.0 * ((x / 61.0).sin() * (y / 47.0).cos());
                data[row * n + col] = (120.0 + broad + knife + rough) as f32;
            }
        }
        let terrain = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap();

        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 869.618;
        link.tx_h_agl_m = 20.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        let mk = |cap: usize| CoverageParams {
            tx_terminal_db: None,
            tx_model_h_m: None,
            rx_terminal: None,
            cancel: None,
            tx: Xy { x: 1200.0, y: -6000.0 },
            radius_m: 5000.0,
            link: link.clone(),
            max_azimuths: Some(64),
            stop_above_db: None,
            stop_after_m: DEFAULT_STOP_AFTER_M,
            max_profile_points: cap,
            table_budget_bytes: DEFAULT_TABLE_BUDGET_BYTES,
        };
        let exact = coverage(&terrain, None, &mk(usize::MAX)).unwrap();
        let fast = coverage(&terrain, None, &mk(MAX_PROFILE_POINTS)).unwrap();
        assert_eq!(exact.loss.data.len(), fast.loss.data.len());

        let (mut worst, mut sum, mut cnt) = (0.0f32, 0.0f64, 0usize);
        for (a, b) in exact.loss.data.iter().zip(&fast.loss.data) {
            if a.is_finite() && b.is_finite() {
                let d = (a - b).abs();
                worst = worst.max(d);
                sum += d as f64;
                cnt += 1;
            }
        }
        assert!(cnt > 1000, "not enough evaluated cells to be meaningful: {cnt}");
        let mean = sum / cnt as f64;
        // Measured 2026-08-31 on this fixture: worst 0.00 dB, mean 0.00 dB
        // (the 5 km path at 30 m needs 167 points, under the cap, so the
        // decimation is exercised by the longer radials only). The bounds are
        // set well inside the model's own ~1 dB reproducibility.
        assert!(worst < 1.0, "worst decimation error {worst:.3} dB (mean {mean:.3})");
    }

    #[test]
    fn flat_coverage_matches_direct_profile_evaluation() {
        let g = flat_grid(201, 30.0, 40.0); // 6×6 km, origin row at y=0
        let p = params();
        let res = coverage(&g, None, &p).unwrap();

        // Pixel due east of TX at ~1.5 km.
        let col = ((1500.0 - g.origin.x) / g.dx_m).round() as usize;
        let row = ((-3000.0 - g.origin.y) / g.dy_m).round() as usize;
        let sweep_lb = res.loss_at(row, col);
        assert!(sweep_lb.is_finite());

        // Direct evaluation over an identical flat profile.
        let k = (1500.0f64 / 30.0).round() as usize;
        let pts: Vec<ProfilePoint> = (0..=k)
            .map(|i| ProfilePoint {
                d_m: i as f64 * 30.0,
                h_terrain_m: 40.0,
                h_clutter_m: 0.0,
                clutter: ClutterClass::Open,
                zone: Zone::Inland,
            })
            .collect();
        let direct = P1812
            .basic_transmission_loss(&Profile { points: pts }, &p.link)
            .unwrap()
            .lb_db;
        assert!(
            (sweep_lb as f64 - direct).abs() < 0.3,
            "sweep {sweep_lb} vs direct {direct}"
        );
    }

    #[test]
    fn rings_lose_more_with_distance_and_gaps_are_marked() {
        let g = flat_grid(201, 30.0, 40.0);
        let p = params();
        let res = coverage(&g, None, &p).unwrap();
        let sample_ring = |r_m: f64| -> f32 {
            let col = ((r_m - g.origin.x) / g.dx_m).round() as usize;
            let row = ((-3000.0 - g.origin.y) / g.dy_m).round() as usize;
            res.loss_at(row, col)
        };
        let l1 = sample_ring(1000.0);
        let l2 = sample_ring(2000.0);
        assert!(l1.is_finite() && l2.is_finite());
        assert!(l2 > l1, "{l2} vs {l1}");
        // Inside P.1812's 250 m validity floor the cell is EVALUATED, not
        // NaN. It used to be NaN, and that was defensible while nothing could
        // answer there — but the map drew the hole as unevaluated and the gap
        // census counted it as served, so a transmitter's own block was the
        // one part of the city nobody could see. The near-field model answers
        // it now.
        let near = sample_ring(120.0);
        assert!(near.is_finite(), "the first 250 m must not be a hole: {near}");
        // And it must not beat an unobstructed path. Over flat bare ground
        // with no clutter this is free space exactly; the guard is against a
        // near-field model that quietly manufactures coverage in the one
        // region an operator can check by walking outside.
        let fs = planner_propag::near_field::free_space_db(p.link.freq_mhz, 0.120) as f32;
        assert!(
            near >= fs - 0.01,
            "near-field loss {near} is below free space {fs}"
        );
        // Monotone out of the near field and across the model handover: the
        // 250 m boundary must not read as a step.
        let inner = sample_ring(200.0);
        let outer = sample_ring(300.0);
        assert!(
            inner.is_finite() && outer.is_finite() && outer > inner,
            "loss must keep rising across the handover: {inner} -> {outer}"
        );
        // Far outside the radius: outside the window entirely.
        assert!(res.loss_at(0, 0).is_nan());
    }

    /// A hole in the DTM is not ground. P.1812's internal clamps swallow NaN
    /// (`NaN.max(x)` is `x` in Rust), so walking a radial over a nodata sample
    /// returns a plausible finite loss for a path nobody has the terrain for —
    /// measured 91.4 dB straight through a nodata column. The radial has to
    /// end there, exactly as it does when it leaves the grid.
    #[test]
    fn a_radial_stops_at_nodata_instead_of_inventing_a_loss_beyond_it() {
        let n = 201usize;
        let res = 30.0;
        let mut data = vec![40.0f32; n * n];
        for row in 0..n {
            data[row * n + 60] = f32::NAN; // a nodata column at x = 1800 m
        }
        let holed = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap();
        let solid = flat_grid(n, res, 40.0);

        let mut p = params();
        p.tx = Xy { x: 1500.0, y: -1500.0 }; // west of the column
        p.radius_m = 2000.0;
        let probe = |g: &Grid| {
            let r = coverage(g, None, &p).unwrap();
            // 900 m past the column, still well inside the sweep.
            let col = ((2700.0 - g.origin.x) / g.dx_m).round() as usize;
            let row = ((-1500.0 - g.origin.y) / g.dy_m).round() as usize;
            r.loss_at(row, col)
        };
        assert!(probe(&solid).is_finite(), "the same path over data must evaluate");
        assert!(probe(&holed).is_nan(), "loss was invented across a hole in the DTM");
    }

    /// P.1812 validates the parameter SET per call, so a bad frequency or
    /// time percentage fails every step of every radial and the sweep comes
    /// back all-NaN — which reads as "this transmitter reaches nobody".
    #[test]
    fn a_link_the_model_refuses_is_an_error_not_an_empty_raster() {
        let g = flat_grid(201, 30.0, 40.0);
        let mut p = params();
        p.link.time_pct = 0.5; // P.1812 Table 1: 1..50
        assert!(matches!(coverage(&g, None, &p), Err(CoverageError::UnusableLink(_))));
        assert!(coverage(&g, None, &params()).is_ok());
    }

    #[test]
    fn tx_outside_grid_is_an_error() {
        let g = flat_grid(11, 30.0, 0.0);
        let mut p = params();
        p.tx = Xy { x: 1e6, y: 1e6 };
        assert!(matches!(coverage(&g, None, &p), Err(CoverageError::TxOutsideGrid)));
    }

    #[test]
    fn clutter_layer_raises_loss_for_embedded_receivers() {
        let g = flat_grid(201, 30.0, 40.0);
        let clut = flat_grid(201, 30.0, 15.0); // uniform 15 m urban canopy
        let p = params(); // rx_h 2 m — below the clutter tops
        let open = coverage(&g, None, &p).unwrap();
        let urban = coverage(&g, Some(&clut), &p).unwrap();
        let col = ((1500.0 - g.origin.x) / g.dx_m).round() as usize;
        let row = ((-3000.0 - g.origin.y) / g.dy_m).round() as usize;
        let lo = open.loss_at(row, col);
        let lu = urban.loss_at(row, col);
        assert!(lu > lo + 10.0, "urban {lu} vs open {lo}");
    }

    /// The polar table has to fit the budget the caller set — the reason the
    /// sweep is chunked into sectors at all.
    ///
    /// Measured BY CONSTRUCTION, not by watching the allocator: the peak is
    /// `(ceil(n_az/S) + 1) * m * 4` bytes for the S the sweep reports having
    /// used. Before the split there was no S — one `vec![f32::NAN; n_az * m]`
    /// per sweep — and the first two cases below are what that cost on the
    /// shapes the web layer actually asks for.
    #[test]
    fn the_polar_table_stays_inside_its_memory_budget() {
        // 20 km and the 30 km cap, both at the 32768-azimuth ceiling the web
        // layer sets and a 5 m pack.
        for (n_az, m, disc_mb) in [(25_133usize, 4_000usize, 402.1f64), (32_768, 6_000, 786.5)] {
            let disc = peak_table_bytes(n_az, m, 1) as f64 / 1e6;
            assert!(
                (disc - disc_mb).abs() < 0.5,
                "the disc table for n_az {n_az}, m {m} is {disc:.1} MB, not the {disc_mb} MB this test was written against"
            );
            let s = sectors_for_budget(n_az, m, DEFAULT_TABLE_BUDGET_BYTES);
            let peak = peak_table_bytes(n_az, m, s);
            assert!(
                peak <= DEFAULT_TABLE_BUDGET_BYTES,
                "n_az {n_az}, m {m}: S={s} peaks at {peak} B, over the {DEFAULT_TABLE_BUDGET_BYTES} B budget"
            );
            // ...and no further than it has to: every extra sector costs a
            // parallel barrier, a recomputed seam radial and a wedge window.
            assert!(
                s > 1 && peak_table_bytes(n_az, m, s - 1) > DEFAULT_TABLE_BUDGET_BYTES,
                "n_az {n_az}, m {m}: S={s} splits further than the budget forces"
            );
        }

        // End to end, on a sweep that actually runs: the S in the result is
        // the S the table was allocated at.
        let g = flat_grid(201, 30.0, 40.0);
        let mut p = params();
        p.table_budget_bytes = 20_000; // 60 rows of the 83 this sweep needs
        let tight = coverage(&g, None, &p).unwrap();
        let peak = peak_table_bytes(tight.azimuths, tight.steps_per_radial, tight.sectors);
        assert_eq!((tight.azimuths, tight.steps_per_radial), (360, 83));
        assert!(tight.sectors > 1, "a 20 kB budget must split 360x83x4 = 119852 B");
        assert!(
            peak <= p.table_budget_bytes,
            "S={} peaks at {peak} B over a {} B budget",
            tight.sectors,
            p.table_budget_bytes
        );
        // A fixture this small fits the default budget whole, and must not pay
        // for sectors it does not need.
        assert_eq!(coverage(&g, None, &params()).unwrap().sectors, 1);
    }

    /// Cancellation survives the sector split.
    ///
    /// What this pins is the contract the callers rely on, unchanged by the
    /// sector split: a cancelled sweep is an ERROR, never a raster with wedges
    /// quietly missing, and a flag that is never set changes nothing. The
    /// sweep also polls the flag once per sector before allocating that
    /// sector's table -- an early-out that saves an allocation and that this
    /// test does NOT distinguish from the per-azimuth poll (deleting it leaves
    /// the suite green; independent review). It is not load-bearing for
    /// correctness, only for promptness.
    #[test]
    fn a_cancelled_sweep_is_an_error_not_a_half_filled_raster() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let g = flat_grid(201, 30.0, 40.0);
        let mut p = params();
        // Six rows of the 83 this sweep needs, so the sector loop runs many
        // times and its own cancel check is the one under test.
        p.table_budget_bytes = 6 * 83 * 4;
        let flag = std::sync::Arc::new(AtomicBool::new(true));
        p.cancel = Some(flag.clone());
        assert!(matches!(coverage(&g, None, &p), Err(CoverageError::Cancelled)));

        // Cleared, the same params must produce exactly what an uncancellable
        // sweep produces — the flag is not allowed to perturb the answer.
        flag.store(false, Ordering::Relaxed);
        let watched = coverage(&g, None, &p).unwrap();
        let mut q = params();
        q.table_budget_bytes = p.table_budget_bytes;
        let plain = coverage(&g, None, &q).unwrap();
        assert!(watched.sectors > 1, "the fixture stopped exercising the sector loop");
        assert_eq!(watched.sectors, plain.sectors);
        let mut finite = 0usize;
        for (a, b) in plain.loss.data.iter().zip(&watched.loss.data) {
            assert!(a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()));
            if a.is_finite() {
                finite += 1;
            }
        }
        assert!(finite > 500, "only {finite} evaluated cells — the fixture proves nothing");
    }
}
