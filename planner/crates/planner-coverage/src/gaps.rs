//! Gap analysis over an ALREADY DEPLOYED network. Two questions get called
//! "gaps" in the same breath and they are different failures:
//!
//! (A) COVERAGE GAPS — where do people live that no deployed repeater
//!     reaches? This is a point-to-area question, answered on a raster at one
//!     receiver height (`GapParams::rx_h_agl_m`): a handheld in the street.
//!
//! (B) BACKBONE GAPS — which repeaters cannot reach the rest of the network?
//!     This is a point-to-point question between two mounted antennas, each
//!     at its OWN height.
//!
//! Why they cannot be answered by the same raster: the coverage raster asks
//! "can a 1.5 m handheld here hear site X", and a repeater 18 m up on a roof
//! is not a 1.5 m handheld. A site sitting inside another site's green area
//! therefore says nothing about whether the two can talk, and a site in a
//! white area may still have a clean rooftop-to-rooftop path over the houses
//! that block the street. So (B) re-runs P.1812 per site PAIR with both real
//! heights instead of reading anything off (A).
//!
//! The failures are also different in kind. An isolated repeater still lights
//! up its neighbourhood on the (A) raster while being useless as
//! infrastructure: nothing it hears can get out of the neighbourhood. Merging
//! the two numbers hides exactly that, so they are reported separately and a
//! caller should show both.
//!
//! Everything here is offline: rasters come from the pack, sites come from the
//! caller. No network access at analysis time.

use crate::{coverage, CoverageError, CoverageParams, DEFAULT_STOP_AFTER_M};
use planner_core::geo::Xy;
use planner_core::model::LinkParams;
use planner_propag::p1812::{lb_from_arrays, ArrayInputs};
use planner_terrain::Grid;
use rayon::prelude::*;

/// Per-site transmitter terminal correction: given the site's position and
/// antenna height, a function from bearing (radians, 0 = north, clockwise) to
/// extra loss in dB.
///
/// Position and height rather than the whole `SiteSpec`, because that is all
/// P.2108 §3.1 reads and it is what the accepted-site list carries after the
/// height clamp — passing the spec would hand the caller a pre-clamp height
/// and quietly measure a different antenna than the sweep uses.
pub type TxTerminalFn = std::sync::Arc<
    dyn Fn(Xy, f64) -> Option<std::sync::Arc<dyn Fn(f64) -> f32 + Send + Sync>> + Send + Sync,
>;

/// P.1812's lower validity bound is 0.25 km (§1). Inside it the model has
/// nothing to say — which is NOT the same as "no signal": a receiver 100 m
/// from a repeater is the best-served receiver there is.
const P1812_MIN_RANGE_M: f64 = 250.0;

/// The census raster's "this cell was never evaluated" code.
///
/// It is NOT zero. Zero means "evaluated, and no deployed site reaches it" —
/// a gap the operator has to close. 255 means the cell is outside every
/// site's sweep window, or carries no terrain data, and the map must draw it
/// as nothing rather than as a hole. The f32 raster this replaced spelled the
/// same distinction NaN vs 0.0.
pub const NOT_EVALUATED: u8 = u8::MAX;

/// Highest representable count; further sites saturate here rather than
/// wrapping into `NOT_EVALUATED`.
///
/// The largest deployed network this has been run against is 258 nodes, of
/// which 253 are accepted, and the densest cell on that run is reached by far
/// fewer. Saturation is therefore not reachable in practice today — it exists
/// so that a future 300-site pack degrades into "at least 254 sites reach
/// this", which every consumer already reads correctly (all of them compare
/// against `k_target`), instead of wrapping to 0 = "nobody reaches it".
pub const MAX_COUNT: u8 = NOT_EVALUATED - 1;

/// Per demand cell, how many deployed sites reach it — ONE BYTE per cell.
///
/// WHY NOT A `Grid`. Every other raster in this crate is f32 because it holds
/// a physical quantity. This one holds a COUNT in `0..=254` plus the
/// `NOT_EVALUATED` code, and it is the one raster a server keeps for the
/// lifetime of the process after a census. On `berlin-city-5m-geo`
/// (10 742 × 9 127 = 98 042 234 cells, the pack the 2777 MB census peak was
/// measured on) the census window is the whole pack: 392 MB as f32, 98 MB as
/// u8 — and on the web server
/// that 392 MB was held until the next census replaced it, which on a server
/// nobody re-runs means for the life of the process.
///
/// Geometry is the same contract as `planner_terrain::Grid`: `origin` is the
/// CENTRE of cell (0, 0) and `dy_m` is signed.
#[derive(Debug, Clone)]
pub struct CountGrid {
    pub origin: Xy,
    pub dx_m: f64,
    pub dy_m: f64,
    pub width: usize,
    pub height: usize,
    /// Row-major, `width * height` entries. `NOT_EVALUATED` where the census
    /// has no answer.
    pub data: Vec<u8>,
}

impl CountGrid {
    /// Count at a cell of THIS raster, `NOT_EVALUATED` off the raster.
    #[inline]
    pub fn count(&self, row: usize, col: usize) -> u8 {
        if row >= self.height || col >= self.width {
            return NOT_EVALUATED;
        }
        self.data[row * self.width + col]
    }

    /// Nearest-cell sample as the f32 the renderers already read: the count,
    /// or NaN for `NOT_EVALUATED`. `None` outside the raster.
    ///
    /// NEAREST and never bilinear, for the same reason `overlay_network`
    /// samples nearest: averaging 0 and 2 repeaters into 1 invents a
    /// redundancy tier that no site provides.
    ///
    /// Domain and rounding mirror `Grid::sample_nearest` exactly (half a cell
    /// beyond the outer centres, pixel-area semantics) so that a census
    /// raster and a loss raster windowed for the same view land on the same
    /// cells. The empty-raster guard is the one difference: `Grid` panics
    /// there on `clamp(0, -1)`, and a census with no accepted site really
    /// does build a 0x0 raster.
    pub fn sample_nearest(&self, p: Xy) -> Option<f32> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let fx = (p.x - self.origin.x) / self.dx_m;
        let fy = (p.y - self.origin.y) / self.dy_m;
        let eps = 1e-9;
        if fx < -0.5 - eps
            || fy < -0.5 - eps
            || fx > (self.width as f64) - 0.5 + eps
            || fy > (self.height as f64) - 0.5 + eps
        {
            return None;
        }
        let c = (fx.round() as isize).clamp(0, self.width as isize - 1) as usize;
        let r = (fy.round() as isize).clamp(0, self.height as isize - 1) as usize;
        Some(as_f32(self.data[r * self.width + c]))
    }

    /// Nearest-cell count at a world point as f32, NaN off the raster or
    /// where nothing was evaluated. The shape a per-building lookup wants.
    pub fn count_at_world(&self, x: f64, y: f64) -> f32 {
        // The same hard-bounds rule as the loss sample beside it in the web
        // layer (`grid_value_at`): round to the nearest cell and refuse
        // anything outside `0..width` / `0..height`. `sample_nearest` admits
        // half a cell beyond the outer centres, which is right for a display
        // resample and would have quietly widened the domain a building can
        // be valued in relative to its loss value.
        let col = ((x - self.origin.x) / self.dx_m).round();
        let row = ((self.origin.y - y) / self.dy_m.abs()).round();
        if col < 0.0 || row < 0.0 {
            return f32::NAN;
        }
        let (c, r) = (col as usize, row as usize);
        if c >= self.width || r >= self.height {
            return f32::NAN;
        }
        as_f32(self.data[r * self.width + c])
    }
}

/// A stored count as the f32 the rest of the stack reads: `NOT_EVALUATED`
/// becomes NaN, everything else its own value.
#[inline]
fn as_f32(v: u8) -> f32 {
    if v == NOT_EVALUATED {
        f32::NAN
    } else {
        v as f32
    }
}

/// One more site reaches this cell.
///
/// TWO things a plain `+= 1` gets wrong on a byte, and both of them invert the
/// answer rather than blur it:
///
///  * a `NOT_EVALUATED` cell (no terrain data under it) would become 0, i.e.
///    "we looked and nobody reaches it" — a claim about ground nobody
///    measured. It stays `NOT_EVALUATED`.
///  * a cell already at `MAX_COUNT` would become 255, which IS
///    `NOT_EVALUATED`: the most redundantly covered cell on the map would read
///    as unevaluated. It saturates instead.
#[inline]
fn bump_count(cell: &mut u8) {
    if *cell != NOT_EVALUATED {
        *cell = (*cell + 1).min(MAX_COUNT);
    }
}

/// Extra dB of sweep headroom above a site's own budget before radials are
/// abandoned. The sweep terminator is a compute saving, not a decision: cells
/// a few dB over budget must still be evaluated so the served/unserved
/// boundary comes out of the physics rather than out of where a radial gave
/// up. 6 dB is one fade-margin step.
const BUDGET_SWEEP_SLACK_DB: f32 = 6.0;

/// One deployed radio.
///
/// Deliberately NOT the pack's node type, and not tied to planner-pack at all:
/// this crate must stay usable with sites parsed from a MeshCore advert dump,
/// typed in by hand, or read from a pack, and planner-pack already depends on
/// this crate's neighbours. The caller does the parsing and the projection
/// into pack CRS; we only do radio physics.
#[derive(Debug, Clone)]
pub struct SiteSpec {
    /// Position in the pack's metric CRS.
    pub xy: Xy,
    /// Antenna height above GROUND, metres. This is a surveyed/declared
    /// height, so — unlike the siting optimizer's candidates — no clutter
    /// height is added to it: doing that would mount a node that is already
    /// on a roof a second time on the same roof.
    pub h_agl_m: f64,
    /// Power this node is actually configured for (dBm). None = the plan's
    /// reference power. A node running 14 dBm must not be credited with the
    /// coverage of a 27 dBm one.
    pub tx_power_dbm: Option<f64>,
    /// Free-form, for the report only (advert name, call sign, site name).
    pub label: String,
}

pub struct GapParams {
    /// Base link parameters — frequency, time/location percentages, and the
    /// pack region's ΔN / N0 / path-centre latitude. Heights are overridden
    /// per link (per site for (A), per site pair for (B)).
    pub link: LinkParams,
    /// A demand cell counts as reached when Lb ≤ this (dB).
    pub budget_db: f32,
    /// Receiver height the coverage question is asked at (m AGL).
    ///
    /// ONE height for the whole raster is exactly what makes (A) unable to
    /// represent a link to an elevated repeater: at 1.5 m the model answers
    /// for a handheld in the street, and a repeater on a 20 m roof inside the
    /// same pixel has a completely different horizon. That is the reason
    /// `BackboneReport` exists and is computed per pair instead of being read
    /// off this raster.
    pub rx_h_agl_m: f64,
    /// Per-site sweep cap (m) and the pruning distance for backbone pairs.
    ///
    /// A COMPUTE BUDGET, not a physical range: set it at or beyond the radio
    /// horizon. Demand further than this from every site is reported
    /// uncovered WITHOUT being evaluated, and the share that was inferred
    /// that way is reported as `PopulationStats::uncovered_beyond_sweep` so a
    /// cap set too small shows up as a number instead of silently deciding
    /// the answer.
    pub radius_m: f64,
    /// How many INDEPENDENT deployed sites must reach a cell before it counts
    /// as served. 1 answers "is there any signal"; 2 is what redundancy
    /// means — one mast down, one site off mains, and k=1 coverage is zero
    /// coverage.
    pub k_target: u8,
    /// Azimuth cap per site sweep (coarse is fine for a gap census).
    pub max_azimuths: Option<usize>,
    /// Extra loss margin a site-to-site backbone hop must have in hand
    /// (dB). A backbone edge carries other people's traffic all day, so it is
    /// held to a stricter budget than a handheld's last hop.
    pub backbone_margin_db: f32,
    /// Reference transmit power the budget was computed at (dBm). With this
    /// and `SiteSpec::tx_power_dbm` both set, each site's budget is shifted by
    /// its own power deficit; either being None means "everyone at the
    /// reference".
    pub ref_tx_power_dbm: Option<f64>,
    /// The terminal surroundings P.1812 does not model — see
    /// `planner_propag::p2108`.
    ///
    /// WHY THE CALLER SUPPLIES THE TRANSMITTER TERM. It is a function of the
    /// footprint geometry AROUND each site, and this crate has rasters, not
    /// polygons. Only the caller holds the building index the interactive map
    /// measures it from, so handing in a closure is the only way the census
    /// can run the same model as the map.
    ///
    /// WHAT IT COSTS TO LEAVE THEM NONE. The census then answers a more
    /// optimistic question than the single-site map does, and on this network
    /// the difference is not subtle: a node below its own roofline pays 14-17
    /// dB it was not being charged, and a street-level listener another 25.
    /// With 258 sites at k=1 the map goes flat green either way, which reads
    /// as "the whole city is covered" rather than as "this model cannot tell
    /// these places apart".
    pub tx_terminal: Option<TxTerminalFn>,
    pub rx_terminal: Option<crate::RxTerminal>,
    /// How many site sweeps run concurrently (0 = auto, see
    /// `sweep_batch_for`).
    ///
    /// Bounds peak memory: each in-flight sweep holds a radius-window raster,
    /// the sweep's polar table and its served-cell bitset, so an unbounded
    /// par_iter over 400 sites would hold 400 of them at once. On the 2c/2GB
    /// deployment target that is the difference between running and being
    /// OOM-killed.
    ///
    /// Auto used to mean "one per core", which is a statement about CPUs and
    /// not about the thing being bounded: on a 12-core workstation it holds
    /// twelve windows whatever their size, and on the 2 GB VPS it holds two
    /// that may not fit. Auto now derives the number from memory and caps it
    /// at the pool's thread count, so cores only ever LOWER it.
    ///
    /// MEASURED, `planner gaps` over 258 supplied sites (253 accepted) on the
    /// Berlin 5 m pack, 12 km cap, 512 azimuths, k=1, 12-core workstation with
    /// 32 GiB — peak working set sampled every 50 ms, two runs each in both
    /// orders to cancel machine contention:
    ///
    /// ```text
    ///                     peak RSS         wall
    ///   before      2250 MB / 2353 MB   237.5 s / 237.6 s
    ///   after       2008 MB / 2026 MB   223.7 s / 259.2 s
    /// ```
    ///
    /// The batch is 12 on that machine either way — 0.7 x 32 GiB affords 227
    /// sweeps — so the saving there is the u8 census raster and the hit
    /// bitset, not the batch. What the batch rule changes is the 2 GB case,
    /// which this workstation cannot exhibit; `sweep_batch_for`'s tests pin
    /// that arithmetic instead. Census percentages were bit-identical across
    /// all four runs.
    pub sweep_batch: usize,
}

/// Bytes one in-flight site sweep holds at its peak.
///
/// Three terms, all of them measured shapes of this code rather than
/// estimates:
///
///  * the sweep's own output raster, `coverage()`'s radius window —
///    `(2*ceil(R/res) + 3)^2` cells of f32. 12 km at 5 m is 4803^2 = 92 MB;
///  * the polar loss table `coverage()` sweeps into, `n_az * (R/res)` f32.
///    512 azimuths at 12 km / 5 m is 4.9 MB;
///  * this module's served-cell bitset over that same window, one BIT per
///    cell rounded up to a whole word per row (`SweepHits`) — 2.9 MB for the
///    same sweep. It used to be a `Vec<u32>` of served cell indices, which at
///    full coverage is the disc area times 4 bytes and doubles again while the
///    vector grows: 2*pi*(R/res)^2*4 = 145 MB, which made the LIST bigger than
///    the raster it was extracted from.
pub fn per_site_bytes(radius_m: f64, res_m: f64, n_az: usize) -> u64 {
    let steps = (radius_m / res_m.max(f64::MIN_POSITIVE)).ceil().max(0.0) as u64;
    let side = 2 * steps + 3;
    let raster = side * side * 4;
    let table = (n_az as u64) * steps * 4;
    let hits = side * side.div_ceil(64) * 8;
    raster + table + hits
}

/// How many sweeps a memory budget affords, capped by the worker count.
///
/// `available` is what `usable_memory_bytes()` reports; 70 % of it is the
/// share this run may claim, which leaves the other 30 % for the allocator's
/// own fragmentation, the pack's memory-mapped layers, and — on the
/// deployment target — the rest of a multi-tenant VPS. `fixed_floor` is what
/// the run holds for its whole duration regardless of the batch: the pack
/// windows the caller passed in plus the census raster.
///
/// Never returns 0: one site at a time is the smallest unit of work there is,
/// and refusing to run is not better than swapping.
pub fn sweep_batch_for(
    available_bytes: u64,
    fixed_floor_bytes: u64,
    per_site_bytes: u64,
    num_threads: usize,
) -> usize {
    let budget = (available_bytes as f64 * 0.7) as u64;
    let spare = budget.saturating_sub(fixed_floor_bytes);
    let fits = spare / per_site_bytes.max(1);
    (fits as usize).clamp(1, num_threads.max(1))
}

/// Memory this process may plan against, in bytes.
///
/// No `sysinfo`: the workspace has no such dependency and this is the only
/// caller, so the crate reads the two platforms that run the census.
///
///  * Linux (the deployment target) — `/proc/meminfo`'s `MemAvailable`, the
///    kernel's own estimate of what can be allocated without swapping. That
///    is the figure the 0.7 factor above is written against.
///  * Windows (the workstation) — `GetPhysicallyInstalledSystemMemory`, which
///    reports INSTALLED memory, not free memory. The number is therefore an
///    over-estimate of what is really available, and the 0.7 factor is doing
///    more work here than on Linux. Accepted because the workstation is where
///    the pack is large and the memory is plentiful, and because the
///    alternative (`GlobalMemoryStatusEx`) buys a second FFI declaration for a
///    figure that swings with whatever else happens to be open.
///  * anything else, or either probe failing — 2 GiB, the smallest machine
///    this is deployed on. Guessing high on an unknown platform is how a
///    planner gets OOM-killed; guessing low only makes it slower.
pub fn usable_memory_bytes() -> u64 {
    const FALLBACK: u64 = 2 * 1024 * 1024 * 1024;
    platform_memory_bytes().filter(|b| *b > 0).unwrap_or(FALLBACK)
}

#[cfg(target_os = "linux")]
fn platform_memory_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    // MemAvailable first; MemTotal only if the kernel is too old to publish
    // it (pre-3.14), where it is the only figure on offer.
    for key in ["MemAvailable:", "MemTotal:"] {
        if let Some(line) = text.lines().find(|l| l.starts_with(key)) {
            if let Some(kb) = line.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
                return Some(kb * 1024);
            }
        }
    }
    None
}

#[cfg(windows)]
fn platform_memory_bytes() -> Option<u64> {
    // The only FFI in this workspace. std has no memory-size API, and pulling
    // in a Windows binding crate for one call would put a platform dependency
    // into a crate that is otherwise pure arithmetic over grids. The call
    // writes one u64 and returns a BOOL; there is no allocation, no handle and
    // no lifetime involved, which is the whole reason it is the one worth
    // making.
    #[link(name = "kernel32")]
    extern "system" {
        fn GetPhysicallyInstalledSystemMemory(total_kb: *mut u64) -> i32;
    }
    let mut kb: u64 = 0;
    let ok = unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) };
    (ok != 0).then(|| kb * 1024)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_memory_bytes() -> Option<u64> {
    None
}

impl GapParams {
    pub fn defaults(link: LinkParams) -> Self {
        Self {
            link,
            budget_db: 135.0,
            rx_h_agl_m: 1.5,
            radius_m: 10_000.0,
            k_target: 1,
            max_azimuths: Some(512),
            backbone_margin_db: 6.0,
            ref_tx_power_dbm: None,
            tx_terminal: None,
            rx_terminal: None,
            sweep_batch: 0,
        }
    }
}

/// Why a supplied site never entered the analysis.
#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// Its coordinates fall outside the pack raster. Real advert dumps carry
    /// nodes from the whole planet and null islands at (0, 0); a planner that
    /// panics on those is unusable on real data.
    OutsidePack,
    /// Inside the extent but the terrain sample there is nodata.
    NoTerrainData,
    /// Lands on the same pack pixel as an earlier site (input index given).
    /// Two radios inside one 10 m cell are one rooftop: they share the mast,
    /// the power and the fate, so counting them twice would let a single
    /// building satisfy a k=2 redundancy target on its own.
    DuplicateOfSite(usize),
}

#[derive(Debug, Clone)]
pub struct SkippedSite {
    /// Index into the caller's `sites` slice.
    pub index: usize,
    pub label: String,
    pub xy: Xy,
    pub reason: SkipReason,
}

#[derive(Debug, Clone)]
pub struct SiteStatus {
    /// Index into the caller's `sites` slice.
    pub index: usize,
    pub xy: Xy,
    pub label: String,
    /// Height actually used, after clamping into P.1812's 1..3000 m range.
    pub h_agl_m: f64,
    pub tx_power_dbm: Option<f64>,
    /// Path loss this site was evaluated against (dB), after its power
    /// deficit was applied.
    pub budget_db: f32,
    /// The sweep window ran into the pack edge, so this site's reach is a
    /// LOWER BOUND — the missing part is outside the data, not unserved. The
    /// accounting never counts anything beyond the pack edge as demand, so
    /// this flag exists to explain a suspiciously small coverage number near
    /// the border rather than to correct one.
    pub window_clipped: bool,
    /// Raster cells this site reaches (all cells, not just inhabited ones).
    pub served_cells: usize,
}

/// A hole, aggregated to something an operator can act on.
#[derive(Debug, Clone)]
pub struct GapCluster {
    /// Demand-weighted centroid — where a new site should aim.
    pub centroid: Xy,
    pub uncovered_population: f64,
    /// Footprint of the UNCOVERED DEMAND cells only (km²), not of the bin:
    /// empty forest inside the same bin is not the operator's problem, and
    /// including it would make a hole look bigger than the thing that has to
    /// be fixed.
    pub area_km2: f64,
    /// Distance from the centroid to the nearest deployed site (m). This is
    /// the number that decides the remedy: a few hundred metres means an
    /// existing site needs a taller mast or more power, ten kilometres means
    /// a new site.
    pub nearest_site_m: f64,
    /// Index into `GapReport::sites`.
    pub nearest_site: Option<usize>,
    /// Part of `uncovered_population` that lies beyond every site's sweep cap
    /// and was therefore inferred from distance, not modelled.
    pub unevaluated_population: f64,
}

#[derive(Debug, Clone)]
pub struct BackboneEdge {
    /// Indices into `GapReport::sites`.
    pub a: usize,
    pub b: usize,
    /// NaN when the pair is closer than P.1812's 0.25 km floor — the edge
    /// exists, the model just has no number for it.
    pub loss_db: f32,
    pub distance_m: f64,
}

#[derive(Debug, Clone)]
pub struct BackboneReport {
    pub edges: Vec<BackboneEdge>,
    /// Connected components, largest first; members are indices into
    /// `GapReport::sites`.
    pub components: Vec<Vec<usize>>,
    /// Pairs that survived distance pruning — the denominator the three
    /// counters below are shares of. A pair reached a P.1812 verdict only if
    /// it is in none of them.
    pub pairs_evaluated: usize,
    pub pairs_pruned_by_distance: usize,
    /// Pairs whose profile left the pack or crossed nodata (no terrain to
    /// walk over).
    pub pairs_off_pack: usize,
    /// Pairs the model REFUSED — a different failure from having no terrain,
    /// and one that is almost always systematic rather than per-pair (see
    /// `probe_link`). Folding it into `pairs_off_pack` made a rejected
    /// parameter set read as "your pack has no data there".
    pub pairs_model_error: usize,
    /// Pairs closer than the model floor, admitted as edges without a number.
    pub pairs_below_model_floor: usize,
    pub elapsed_ms: u64,
}

impl BackboneReport {
    /// The largest component — the network everything else is measured
    /// against. None when there are no sites.
    pub fn giant(&self) -> Option<&[usize]> {
        self.components.first().map(|c| c.as_slice())
    }

    /// Every component that is not the giant one: islands and lone sites.
    pub fn islands(&self) -> &[Vec<usize>] {
        if self.components.is_empty() {
            &[]
        } else {
            &self.components[1..]
        }
    }

    /// Sites that reach nobody at all.
    pub fn isolated(&self) -> Vec<usize> {
        self.components.iter().filter(|c| c.len() == 1).map(|c| c[0]).collect()
    }
}

#[derive(Debug, Clone)]
pub struct PopulationStats {
    /// Denominator: demand over the WHOLE pack on cells that carry terrain
    /// data. See the module's accounting note on `analyse`.
    pub total: f64,
    /// Reached by ≥1 deployed site.
    pub served: f64,
    /// Reached by ≥`k_target` INDEPENDENT sites.
    pub served_k: f64,
    /// `total - served`, exactly.
    pub uncovered: f64,
    /// Share of `uncovered` that lies beyond every site's sweep cap and was
    /// never evaluated. Large values mean `radius_m` is set below the radio
    /// horizon and the answer is being decided by the compute budget.
    pub uncovered_beyond_sweep: f64,
    /// Demand sitting on cells with NO terrain data. Excluded from `total` —
    /// "we cannot say" is not "nobody covers it".
    pub no_data: f64,
    pub k_target: u8,
    /// True when no population layer was supplied and every demand cell
    /// weighs 1, i.e. all the population numbers are really cell counts.
    pub uniform_weights: bool,
    pub demand_cells: usize,
}

pub struct GapReport {
    /// Per demand cell, how many deployed sites reach it. Covers the UNION of
    /// the site sweep windows, not the pack — a pack-sized raster per run is
    /// what made the earlier coverage code infeasible.
    ///
    /// `NOT_EVALUATED` and 0 mean different things: 0 is "evaluated, nothing
    /// reaches it"; `NOT_EVALUATED` is "no data" — outside the pack's terrain,
    /// or outside every site's sweep window and therefore never evaluated.
    /// Cells outside this window are absent from the raster but still counted
    /// in `population` (as uncovered-beyond-sweep), so the raster is a view,
    /// never the accounting.
    pub served: CountGrid,
    /// Window position in the input grid.
    pub col_offset: usize,
    pub row_offset: usize,
    pub sites: Vec<SiteStatus>,
    pub skipped: Vec<SkippedSite>,
    pub population: PopulationStats,
    /// Uncovered demand aggregated into ranked clusters, biggest first.
    pub gaps: Vec<GapCluster>,
    pub backbone: BackboneReport,
    pub cell_area_km2: f64,
    /// Bin edge used to build `gaps` (m).
    pub cluster_bin_m: f64,
}

/// Analyse a deployed network.
///
/// POPULATION ACCOUNTING. The population layer is a 100 m Zensus grid
/// resampled onto pack cells by `planner_pack::zensus::accumulate_population`,
/// which splits each 100 m cell's residents uniformly over the pack cells it
/// covers. Every pack cell therefore already carries its own areal share, and
/// summing ANY set of cells is a valid areal-weighted total — there is no
/// block that has to be summed whole.
///
/// So this function deliberately DIFFERS from `siting::optimize_sites`: it
/// does not aggregate demand into stride blocks at all, it classifies every
/// pack cell individually. siting blocks because it needs a coarse demand set
/// for a combinatorial optimizer, and it was the block that made the earlier
/// bug possible (gating a whole block on its NW anchor pixel, dropping
/// inhabited cells from both the numerator and the denominator). Without
/// blocks there is no anchor to get wrong, and `total == served + uncovered`
/// holds by construction rather than by care.
///
/// The denominator is the WHOLE pack, not the analysed window: an operator
/// asking "how many residents does the deployed network miss" means all of
/// them, including the ones no site comes within a sweep radius of. Cells
/// outside the pack are not demand and are never counted — which is why a
/// site whose sweep window is clipped by the pack edge cannot inflate the
/// uncovered figure (see `SiteStatus::window_clipped`).
///
/// KNOWN LIMIT. A cell with good terrain data that sits in the RADIAL SHADOW
/// of a nodata hole is counted as uncovered, not as no-data: the sweep stops
/// at the hole (it has to — see the nodata note in `coverage`) but the cell
/// is inside a site's sweep disc, and the disc is what the "was this
/// evaluated" baseline is built from. The error is in the safe direction —
/// coverage is understated, never claimed over ground nobody measured — but
/// on a pack with a ragged DTM it shows up as holes that are really data
/// gaps. Splitting the two would need the baseline to come from the sweeps
/// themselves, which cannot distinguish a data shadow from a radial the
/// budget terminator gave up on.
pub fn analyse(
    terrain: &Grid,
    clutter: Option<&Grid>,
    population: Option<&Grid>,
    sites: &[SiteSpec],
    p: &GapParams,
) -> Result<GapReport, CoverageError> {
    if p.radius_m <= 0.0 {
        return Err(CoverageError::BadRadius);
    }
    probe_link(&p.link)?;
    let (w, h) = (terrain.width, terrain.height);
    let res = terrain.dx_m.abs();
    let cell_area_km2 = (terrain.dx_m * terrain.dy_m).abs() / 1.0e6;

    let pop_ok = population.is_some_and(|g| g.width == w && g.height == h);
    if population.is_some() && !pop_ok {
        eprintln!("gaps: population grid shape mismatch — falling back to uniform weights");
    }
    // A clutter grid that does not span the pack is worse than none: every
    // sample outside it comes back as 0 m of clutter, i.e. OPEN GROUND, and a
    // Berlin audit run with a mis-registered clutter layer reports the city as
    // a field. The sampling contract cannot tell that apart from a legitimate
    // "no clutter here", so say it out loud rather than silently returning the
    // optimistic answer.
    if let Some(cg) = clutter {
        let (x0, y0) = (terrain.origin.x, terrain.origin.y);
        let (x1, y1) = (
            x0 + (w.saturating_sub(1)) as f64 * terrain.dx_m,
            y0 + (h.saturating_sub(1)) as f64 * terrain.dy_m,
        );
        let corners =
            [Xy { x: x0, y: y0 }, Xy { x: x1, y: y0 }, Xy { x: x0, y: y1 }, Xy { x: x1, y: y1 }];
        if corners.iter().any(|c| cg.sample_bilinear(*c).is_none()) {
            eprintln!(
                "gaps: clutter layer does not cover the whole pack — paths outside it are \
                 modelled as open ground, which overstates coverage"
            );
        }
    }

    // ---- 1. Accept or skip sites ---------------------------------------
    let mut accepted: Vec<SiteStatus> = Vec::new();
    let mut accepted_px: Vec<(usize, usize)> = Vec::new();
    let mut skipped: Vec<SkippedSite> = Vec::new();
    for (i, s) in sites.iter().enumerate() {
        let col = ((s.xy.x - terrain.origin.x) / terrain.dx_m).round();
        let row = ((s.xy.y - terrain.origin.y) / terrain.dy_m).round();
        // Bounds first, then nodata: a global advert dump reaches this
        // function with nodes on other continents and rows with null
        // coordinates, and neither may panic or abort the run.
        if !(col >= 0.0 && row >= 0.0 && col < w as f64 && row < h as f64)
            || terrain.sample_bilinear(s.xy).is_none()
        {
            skipped.push(SkippedSite {
                index: i,
                label: s.label.clone(),
                xy: s.xy,
                reason: SkipReason::OutsidePack,
            });
            continue;
        }
        let (row, col) = (row as usize, col as usize);
        // Both the site's OWN ground and the bilinear neighbourhood every
        // radial starts from have to be data. An advert lands anywhere, not on
        // a cell centre, so a site half a cell from a hole rounds to a finite
        // pixel while sampling NaN at its own position; since the sweep stops
        // a radial at the first nodata sample, such a site comes back covering
        // only its 250 m near-field disc — a silently near-empty result where
        // a named skip reason is the honest answer.
        if !terrain.data[row * w + col].is_finite()
            || !terrain.sample_bilinear(s.xy).is_some_and(|v| v.is_finite())
        {
            skipped.push(SkippedSite {
                index: i,
                label: s.label.clone(),
                xy: s.xy,
                reason: SkipReason::NoTerrainData,
            });
            continue;
        }
        if let Some(k) = accepted_px.iter().position(|&px| px == (row, col)) {
            skipped.push(SkippedSite {
                index: i,
                label: s.label.clone(),
                xy: s.xy,
                reason: SkipReason::DuplicateOfSite(accepted[k].index),
            });
            continue;
        }
        let (.., clipped) = sweep_window(terrain, s.xy, p.radius_m);
        accepted.push(SiteStatus {
            index: i,
            xy: s.xy,
            label: s.label.clone(),
            // P.1812 rejects terminal heights outside 1..3000 m; advert data
            // regularly says 0. Clamp and report what was used.
            h_agl_m: s.h_agl_m.clamp(1.0, 3000.0),
            tx_power_dbm: s.tx_power_dbm,
            budget_db: site_budget_db(p, s.tx_power_dbm),
            window_clipped: clipped,
            served_cells: 0,
        });
        accepted_px.push((row, col));
    }

    // ---- 2. Union window ------------------------------------------------
    // Mirrors the window `coverage()` computes internally. Results are merged
    // by ABSOLUTE parent indices and anything landing outside is dropped, so
    // if the two ever drift the merge stays correct instead of scrambling.
    let (mut uc0, mut ur0, mut uc1, mut ur1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for s in &accepted {
        let (c0, r0, c1, r1, _) = sweep_window(terrain, s.xy, p.radius_m);
        uc0 = uc0.min(c0);
        ur0 = ur0.min(r0);
        uc1 = uc1.max(c1);
        ur1 = ur1.max(r1);
    }
    // With no accepted site there is nothing to analyse; a 0x0 window keeps
    // the rest of the function branch-free and makes every resident come out
    // as uncovered-beyond-sweep, which is the correct answer to "who does a
    // network of no repeaters reach".
    if accepted.is_empty() {
        uc0 = 0;
        ur0 = 0;
        uc1 = 0;
        ur1 = 0;
    }
    let (uw, uh) =
        if accepted.is_empty() { (0usize, 0usize) } else { (uc1 - uc0 + 1, ur1 - ur0 + 1) };
    let mut served = CountGrid {
        origin: Xy {
            x: terrain.origin.x + uc0 as f64 * terrain.dx_m,
            y: terrain.origin.y + ur0 as f64 * terrain.dy_m,
        },
        dx_m: terrain.dx_m,
        dy_m: terrain.dy_m,
        width: uw,
        height: uh,
        data: vec![NOT_EVALUATED; uw * uh],
    };

    // ---- 3. Baseline: which cells were evaluated at all ------------------
    // A cell inside some site's sweep disc and carrying terrain data becomes
    // 0 = "evaluated, nothing reaches it". Everything else stays
    // NOT_EVALUATED, which is what keeps "no data" distinguishable from "no
    // coverage" downstream.
    // Parallel over rows: rows are disjoint, so no site's disc can race.
    //
    // Each row takes each disc's exact column span rather than testing every
    // cell against every site: the naive form is cells x sites, which at the
    // Berlin pack's 23 M cells and ~400 repeaters is 9 G distance tests to
    // decide something that is pure geometry. Spans cost the area of the
    // discs and nothing else.
    let sites_xy: Vec<Xy> = accepted.iter().map(|s| s.xy).collect();
    if uw > 0 {
        let (ox, oy) = (served.origin.x, served.origin.y);
        let (dx, dy) = (served.dx_m, served.dy_m);
        served.data.par_chunks_mut(uw).enumerate().for_each(|(lr, out_row)| {
            let y = oy + lr as f64 * dy;
            let trow = &terrain.data[(ur0 + lr) * w..(ur0 + lr) * w + w];
            for s in &sites_xy {
                let dy_m = y - s.y;
                if dy_m.abs() > p.radius_m {
                    continue;
                }
                let half = (p.radius_m * p.radius_m - dy_m * dy_m).sqrt();
                // Endpoints ordered by min/max rather than by assuming a
                // west-to-east column axis: dx_m is signed on a Grid.
                let (a, b) = (((s.x - half) - ox) / dx, ((s.x + half) - ox) / dx);
                let lo = a.min(b).ceil().max(0.0) as usize;
                let hi = a.max(b).floor().max(0.0) as usize;
                for lc in lo..=hi.min(uw - 1) {
                    if trow[uc0 + lc].is_finite() {
                        out_row[lc] = 0;
                    }
                }
            }
        });
    }

    // ---- 4. Per-site sweeps ---------------------------------------------
    // Batched rather than one big par_iter: see GapParams::sweep_batch.
    //
    // Auto-sizing is against MEMORY, not cores. What the batch bounds is bytes
    // — one radius window, one polar table and one hit bitset per in-flight
    // sweep — and the core count says nothing about how big those are. The
    // fixed floor is the pack windows the caller passed in (f32) plus this
    // run's own census raster (u8); the census raster is only the union of the
    // sweep windows, so charging it at pack size over-states the floor, which
    // is the direction to err in.
    let batch = if p.sweep_batch == 0 {
        let cells = w as u64 * h as u64;
        let windows = 1 + u64::from(clutter.is_some()) + u64::from(population.is_some());
        let fixed_floor = cells * (windows * 4 + 1);
        // The same default `coverage()` computes when max_azimuths is None.
        let n_az = p.max_azimuths.unwrap_or_else(|| {
            ((2.0 * std::f64::consts::PI * p.radius_m / res).ceil() as usize).clamp(360, 5760)
        });
        sweep_batch_for(
            usable_memory_bytes(),
            fixed_floor,
            per_site_bytes(p.radius_m, res, n_az),
            // The POOL's threads, not the machine's cores: the web server runs
            // this inside a smaller pool so tiles keep a core, and sizing to
            // the machine would hold more windows than there are sweeps.
            rayon::current_num_threads(),
        )
    } else {
        p.sweep_batch
    };
    for chunk_start in (0..accepted.len()).step_by(batch.max(1)) {
        let chunk_end = (chunk_start + batch.max(1)).min(accepted.len());
        let swept: Vec<Result<SweepHits, CoverageError>> = accepted[chunk_start..chunk_end]
            .par_iter()
            .map(|s| {
                let mut link = p.link.clone();
                link.tx_h_agl_m = s.h_agl_m;
                link.rx_h_agl_m = p.rx_h_agl_m.clamp(1.0, 3000.0);
                let cp = CoverageParams {
                    // Measured per site by the caller, which is the only holder
                    // of the footprint geometry it comes from. None here means
                    // the census answers a more optimistic question than the
                    // single-site map — see `GapParams::tx_terminal`.
                    tx_terminal_db: p.tx_terminal.as_ref().and_then(|f| f(s.xy, s.h_agl_m)),
                    tx_model_h_m: None,
                    rx_terminal: p.rx_terminal,
                    // The census and the optimiser run to completion; only the
                    // interactive map supersedes its own sweeps.
                    cancel: None,
                    tx: s.xy,
                    radius_m: p.radius_m,
                    link,
                    max_azimuths: p.max_azimuths,
                    stop_above_db: Some(s.budget_db + BUDGET_SWEEP_SLACK_DB),
                    stop_after_m: DEFAULT_STOP_AFTER_M,
                    max_profile_points: crate::MAX_PROFILE_POINTS,
                    table_budget_bytes: crate::DEFAULT_TABLE_BUDGET_BYTES,
                };
                let r = coverage(terrain, clutter, &cp)?;
                // A BIT per cell of this site's own sweep window, not a list
                // of cell indices. The list was a `Vec<u32>`, i.e. 4 BYTES per
                // reached cell with a doubling growth curve behind it: on the
                // Berlin 5 m pack a 12 km sweep is 23.1 M cells, so a site
                // that reaches most of its disc held up to 145 MB of indices
                // next to the 92 MB raster they were read out of. The bitset
                // is 2.9 MB whatever the coverage, and the merge below walks
                // it by 64-bit words so the empty corners cost one test each.
                let mut out =
                    SweepHits::new(r.row_offset, r.col_offset, r.loss.width, r.loss.height);
                for lr in 0..r.loss.height {
                    let prow = r.row_offset + lr;
                    if prow < ur0 || prow > ur1 {
                        continue;
                    }
                    let y = terrain.origin.y + prow as f64 * terrain.dy_m;
                    for lc in 0..r.loss.width {
                        let pcol = r.col_offset + lc;
                        if pcol < uc0 || pcol > uc1 {
                            continue;
                        }
                        let lb = r.loss.data[lr * r.loss.width + lc];
                        let reached = if lb.is_finite() {
                            lb <= s.budget_db
                        } else {
                            // NaN is "not evaluated", never "no signal". The
                            // one case where that matters for the answer is
                            // the near field: inside 250 m P.1812 has no
                            // number, and a receiver that close to a repeater
                            // is the best-served receiver on the map. Count
                            // it as served; the alternative is a black hole
                            // punched through the middle of every cell.
                            let x = terrain.origin.x + pcol as f64 * terrain.dx_m;
                            let d2 = (x - s.xy.x) * (x - s.xy.x) + (y - s.xy.y) * (y - s.xy.y);
                            d2 <= P1812_MIN_RANGE_M * P1812_MIN_RANGE_M
                        };
                        if reached {
                            out.set(lr, lc);
                        }
                    }
                }
                Ok(out)
            })
            .collect();
        for (k, hits) in swept.into_iter().enumerate() {
            let hits = hits?;
            accepted[chunk_start + k].served_cells = hits.count();
            hits.for_each_cell(|prow, pcol| {
                // The scan above already dropped anything outside the union
                // window, so this arithmetic cannot go negative.
                let idx = (prow - ur0) * uw + (pcol - uc0);
                bump_count(&mut served.data[idx]);
            });
        }
    }

    // ---- 5. Accounting and clustering over the whole pack ----------------
    // Bin size: one coverage DIAMETER. A hole narrower than that can be
    // closed by a single new site, so a bin is "one site's worth of hole" —
    // finer bins split one fixable hole into several ranked entries, coarser
    // bins merge two holes that need two sites. Same rule as
    // siting::stride_tile_px, for the same reason.
    let bin_px = ((2.0 * p.radius_m / res).ceil() as usize).max(1);
    let bins_across = w.div_ceil(bin_px);
    let bins_down = h.div_ceil(bin_px);
    let mut tiles = vec![TileAcc::default(); bins_across * bins_down];

    let k_target = p.k_target.max(1);
    let mut st = PopulationStats {
        total: 0.0,
        served: 0.0,
        served_k: 0.0,
        uncovered: 0.0,
        uncovered_beyond_sweep: 0.0,
        no_data: 0.0,
        k_target: p.k_target.max(1),
        uniform_weights: !pop_ok,
        demand_cells: 0,
    };
    for row in 0..h {
        let in_rows = uw > 0 && row >= ur0 && row <= ur1;
        for col in 0..w {
            let weight = if pop_ok {
                let v = population.unwrap().data[row * w + col];
                if v.is_finite() && v > 0.0 {
                    v as f64
                } else {
                    0.0
                }
            } else {
                1.0
            };
            if weight <= 0.0 {
                continue;
            }
            if !terrain.data[row * w + col].is_finite() {
                // Demand on a nodata cell: neither served nor uncovered, and
                // out of the denominator. Claiming it is uncovered would
                // invent a hole out of a hole in the DATA.
                st.no_data += weight;
                continue;
            }
            st.total += weight;
            st.demand_cells += 1;
            let count = if in_rows && col >= uc0 && col <= uc1 {
                served.data[(row - ur0) * uw + (col - uc0)]
            } else {
                NOT_EVALUATED
            };
            if count != NOT_EVALUATED && count >= 1 {
                st.served += weight;
                if count >= k_target {
                    st.served_k += weight;
                }
                continue;
            }
            st.uncovered += weight;
            let unevaluated = count == NOT_EVALUATED;
            if unevaluated {
                // Outside every site's sweep window. With radius_m at the
                // radio horizon this is genuinely out of range; with a
                // smaller cap it is an admission, which is why it is counted.
                st.uncovered_beyond_sweep += weight;
            }
            let t = (row / bin_px) * bins_across + col / bin_px;
            let x = terrain.origin.x + col as f64 * terrain.dx_m;
            let y = terrain.origin.y + row as f64 * terrain.dy_m;
            let acc = &mut tiles[t];
            acc.w += weight;
            acc.wx += weight * x;
            acc.wy += weight * y;
            acc.cells += 1;
            if unevaluated {
                acc.unevaluated += weight;
            }
        }
    }

    let mut gaps: Vec<GapCluster> = tiles
        .iter()
        .filter(|t| t.w > 0.0)
        .map(|t| {
            let centroid = Xy { x: t.wx / t.w, y: t.wy / t.w };
            let (mut nearest_site, mut nearest_site_m) = (None, f64::INFINITY);
            for (i, s) in accepted.iter().enumerate() {
                let d = centroid.dist_m(&s.xy);
                if d < nearest_site_m {
                    nearest_site_m = d;
                    nearest_site = Some(i);
                }
            }
            GapCluster {
                centroid,
                uncovered_population: t.w,
                area_km2: t.cells as f64 * cell_area_km2,
                nearest_site_m,
                nearest_site,
                unevaluated_population: t.unevaluated,
            }
        })
        .collect();
    gaps.sort_by(|a, b| b.uncovered_population.total_cmp(&a.uncovered_population));

    // ---- 6. Backbone ----------------------------------------------------
    let backbone = backbone_graph(terrain, clutter, &accepted, p);

    Ok(GapReport {
        served,
        col_offset: uc0,
        row_offset: ur0,
        sites: accepted,
        skipped,
        population: st,
        gaps,
        backbone,
        cell_area_km2,
        cluster_bin_m: bin_px as f64 * res,
    })
}

#[derive(Clone, Default)]
struct TileAcc {
    w: f64,
    wx: f64,
    wy: f64,
    unevaluated: f64,
    cells: usize,
}

/// Which cells of ONE site's sweep window that site reaches — a bit each.
///
/// This is the hand-off between the parallel sweep and the serial merge, and
/// it is the only per-site allocation that outlives the sweep's own raster. As
/// a `Vec<u32>` of cell indices it was 4 bytes per reached cell (plus the
/// vector's growth slack), which on a 12 km / 5 m sweep peaks ABOVE the 92 MB
/// raster the indices are read from. As a bitset it is
/// `ceil(width * height / 8)` = 2.9 MB for that sweep, independent of how much
/// of the disc is covered.
struct SweepHits {
    /// Position of the sweep window's first row/column in the PARENT grid.
    row_offset: usize,
    col_offset: usize,
    width: usize,
    height: usize,
    /// Words per ROW. Rows start on a word boundary — 63 wasted bits per row
    /// at worst, 37 KB on a 4803-row window — so that neither setting a bit
    /// nor walking the set ones needs an integer division by `width`. The
    /// merge visits one cell per reached pixel, which on this pack is a few
    /// hundred million calls per census, and a 20-cycle divide in that loop is
    /// not a rounding error.
    words_per_row: usize,
    bits: Vec<u64>,
}

impl SweepHits {
    fn new(row_offset: usize, col_offset: usize, width: usize, height: usize) -> Self {
        let words_per_row = width.div_ceil(64);
        Self {
            row_offset,
            col_offset,
            width,
            height,
            words_per_row,
            bits: vec![0u64; words_per_row * height],
        }
    }

    #[inline]
    fn set(&mut self, row: usize, col: usize) {
        self.bits[row * self.words_per_row + (col >> 6)] |= 1u64 << (col & 63);
    }

    /// How many cells are set. A popcount rather than a running counter: the
    /// caller reaches each cell once, so the two agree, and a count that
    /// cannot disagree with the bitset is one fewer thing to keep in step.
    fn count(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Visit every set cell as PARENT grid (row, col), in raster order.
    ///
    /// Word at a time: the sweep window is a square around a disc, so at least
    /// 21 % of it is empty by geometry and much more than that once the budget
    /// terminator has cut the radials, and an all-zero word costs one test for
    /// 64 cells.
    fn for_each_cell(&self, mut visit: impl FnMut(usize, usize)) {
        for lr in 0..self.height {
            let base = lr * self.words_per_row;
            let prow = self.row_offset + lr;
            for wi in 0..self.words_per_row {
                let mut w = self.bits[base + wi];
                while w != 0 {
                    let lc = (wi << 6) + w.trailing_zeros() as usize;
                    w &= w - 1;
                    // The last word of a row carries padding past `width`.
                    // Nothing sets it; the guard is there so a future
                    // whole-word write cannot walk into the next row.
                    if lc >= self.width {
                        break;
                    }
                    visit(prow, self.col_offset + lc);
                }
            }
        }
    }
}

/// Reject a link the model cannot evaluate AT ALL, before any work is done.
///
/// P.1812 rejects whole parameter SETS, not individual paths: a frequency
/// outside 30 MHz–6 GHz, a time percentage outside 1..50 or a location
/// percentage outside 1..99 makes every single evaluation fail. Both passes
/// here treat a failed evaluation as "nothing at this pixel" / "no path over
/// this pair", so without this probe a typo in the link parameters comes back
/// as a fully populated report saying the deployed network reaches nobody and
/// every repeater is an island — which is indistinguishable from a real and
/// catastrophic result, and is exactly the answer a responder would act on.
/// Measured before this check existed: `time_pct = 0.5` on a flat pack with
/// two sites 700 m apart returned Ok with 1.1% of residents served (only the
/// near-field discs, which never call the model) and two isolated components.
///
/// The probe is a synthetic 1 km flat path at heights this module always
/// clamps into range, so the only thing it can fail on is the parameter set.
pub(crate) fn probe_link(link: &LinkParams) -> Result<(), CoverageError> {
    let d_km = [0.0, 0.5, 1.0];
    let z = [0.0, 0.0, 0.0];
    let mut probe = link.clone();
    probe.tx_h_agl_m = 10.0;
    probe.rx_h_agl_m = 10.0;
    let x = ArrayInputs {
        d_km: &d_km,
        h_masl: &z,
        g_masl: &z,
        surface_method: crate::SURFACE_METHOD,
        omega: 0.0,
        dct_km: 1.0,
        dcr_km: 1.0,
        d_tm_km: 1.0,
        d_lm_km: 1.0,
        r_rx_m: 0.0,
    };
    match lb_from_arrays(&x, &probe) {
        Ok(_) => Ok(()),
        Err(e) => Err(CoverageError::UnusableLink(e.to_string())),
    }
}

/// A site's budget after its own power deficit. Both the reference and the
/// site's power must be known for the shift to mean anything.
fn site_budget_db(p: &GapParams, site_power_dbm: Option<f64>) -> f32 {
    match (p.ref_tx_power_dbm, site_power_dbm) {
        (Some(reference), Some(actual)) => p.budget_db - (reference - actual) as f32,
        _ => p.budget_db,
    }
}

/// The pixel window a sweep from `tx` can touch, plus whether it was clipped
/// by the pack edge. Same geometry `coverage()` uses to size its output.
fn sweep_window(terrain: &Grid, tx: Xy, radius_m: f64) -> (usize, usize, usize, usize, bool) {
    let res = terrain.dx_m.abs();
    let rad_px = (radius_m / res).ceil() as isize + 1;
    let col_c = ((tx.x - terrain.origin.x) / terrain.dx_m).round() as isize;
    let row_c = ((tx.y - terrain.origin.y) / terrain.dy_m).round() as isize;
    let clipped = col_c - rad_px < 0
        || row_c - rad_px < 0
        || col_c + rad_px > terrain.width as isize - 1
        || row_c + rad_px > terrain.height as isize - 1;
    let c0 = (col_c - rad_px).max(0) as usize;
    let r0 = (row_c - rad_px).max(0) as usize;
    let c1 = ((col_c + rad_px).max(0) as usize).min(terrain.width - 1);
    let r1 = ((row_c + rad_px).max(0) as usize).min(terrain.height - 1);
    (c0, r0, c1, r1, clipped)
}

enum PairOutcome {
    Edge(BackboneEdge),
    NearField(BackboneEdge),
    OverBudget,
    OffPack,
    ModelError,
}

/// Site-to-site adjacency: P.1812 between every pair closer than the sweep
/// cap, with EACH END AT ITS OWN HEIGHT. That is the whole reason this pass
/// exists and cannot be read off the coverage raster, which knows only one
/// receiver height.
fn backbone_graph(
    terrain: &Grid,
    clutter: Option<&Grid>,
    sites: &[SiteStatus],
    p: &GapParams,
) -> BackboneReport {
    let t0 = std::time::Instant::now();
    let res = terrain.dx_m.abs().max(1.0);
    let n = sites.len();

    // Prune by distance FIRST: pair count is O(n²) — ~80k pairs at the ~400
    // repeaters Berlin actually has — and a profile is orders of magnitude
    // more expensive than a distance check.
    let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
    let mut pruned = 0usize;
    for i in 0..n {
        for j in (i + 1)..n {
            let d = sites[i].xy.dist_m(&sites[j].xy);
            if d > p.radius_m {
                pruned += 1;
                continue;
            }
            pairs.push((i, j, d));
        }
    }

    let outcomes: Vec<PairOutcome> = pairs
        .par_iter()
        .map_init(
            || (Vec::<f64>::new(), Vec::<f64>::new(), Vec::<f64>::new()),
            |(d_km, h_masl, g_masl), &(i, j, dist)| {
                if dist < P1812_MIN_RANGE_M {
                    // Below the model floor. Two repeaters 200 m apart hear
                    // each other; refusing to draw the edge would split a
                    // rooftop cluster into fake islands.
                    return PairOutcome::NearField(BackboneEdge {
                        a: i,
                        b: j,
                        loss_db: f32::NAN,
                        distance_m: dist,
                    });
                }
                let (a, b) = (sites[i].xy, sites[j].xy);
                // Step the MODEL profile at the pack's cell size or the §3.2.1
                // floor, whichever is coarser. Unlike the coverage sweep this
                // pass has no raster to feed, so it can simply sample coarsely
                // instead of decimating afterwards. `floor` (not `round`)
                // because the resulting interval is dist/steps, and rounding
                // up would land it just under the floor on most path lengths.
                let step_m = res.max(crate::SURFACE_METHOD.min_spacing_m());
                let steps = ((dist / step_m).floor() as usize).clamp(3, 20_000);
                d_km.clear();
                h_masl.clear();
                g_masl.clear();
                let mut r_rx_m = 0.0f64;
                for k in 0..=steps {
                    let t = k as f64 / steps as f64;
                    let pt = Xy { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
                    let Some(hv) = terrain.sample_bilinear(pt) else {
                        return PairOutcome::OffPack;
                    };
                    if !hv.is_finite() {
                        return PairOutcome::OffPack;
                    }
                    let c = clutter
                        .and_then(|g| g.sample_bilinear(pt))
                        .unwrap_or(0.0)
                        .max(0.0) as f64;
                    d_km.push(dist / 1000.0 * t);
                    h_masl.push(hv as f64);
                    // (1d): representative clutter is added at the intermediate
                    // points only — a terminal does not diffract over the cell
                    // it stands in. The diffraction code never reads the two
                    // terminal entries, so this changes no number today; it is
                    // here so that stays true by construction rather than by
                    // luck if anything downstream ever does read them. `r_rx_m`
                    // still carries the receiver's own clutter, which eq. (65)
                    // does want.
                    let terminal = k == 0 || k == steps;
                    g_masl.push(hv as f64 + if terminal { 0.0 } else { c });
                    r_rx_m = c;
                }
                let mut link = p.link.clone();
                // Each end at its own mounted height — the point of (B).
                link.tx_h_agl_m = sites[i].h_agl_m;
                link.rx_h_agl_m = sites[j].h_agl_m;
                let d_total = dist / 1000.0;
                let x = ArrayInputs {
                    d_km,
                    h_masl,
                    g_masl,
                    surface_method: crate::SURFACE_METHOD,
                    omega: 0.0,
                    dct_km: d_total,
                    dcr_km: d_total,
                    d_tm_km: d_total,
                    d_lm_km: d_total,
                    r_rx_m,
                };
                // "The model refused" is not "there is no terrain here": one
                // is a parameter set that will refuse every pair on the pack,
                // the other is a real hole in the data, and an operator has to
                // be able to tell them apart from the report alone.
                let Ok(loss) = lb_from_arrays(&x, &link) else {
                    return PairOutcome::ModelError;
                };
                // A backbone hop must close in BOTH directions, so the
                // limiting end is the one with less power: charge the pair
                // the larger of the two deficits.
                let budget = sites[i].budget_db.min(sites[j].budget_db) - p.backbone_margin_db;
                if loss.lb_db <= budget as f64 {
                    PairOutcome::Edge(BackboneEdge {
                        a: i,
                        b: j,
                        loss_db: loss.lb_db as f32,
                        distance_m: dist,
                    })
                } else {
                    PairOutcome::OverBudget
                }
            },
        )
        .collect();

    let mut edges: Vec<BackboneEdge> = Vec::new();
    let (mut off_pack, mut near_field, mut model_err) = (0usize, 0usize, 0usize);
    let mut dsu: Vec<usize> = (0..n).collect();
    for o in outcomes {
        match o {
            PairOutcome::Edge(e) => {
                dsu_union(&mut dsu, e.a, e.b);
                edges.push(e);
            }
            PairOutcome::NearField(e) => {
                near_field += 1;
                dsu_union(&mut dsu, e.a, e.b);
                edges.push(e);
            }
            PairOutcome::OverBudget => {}
            PairOutcome::OffPack => off_pack += 1,
            PairOutcome::ModelError => model_err += 1,
        }
    }

    let mut by_root: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..n {
        by_root.entry(dsu_find(&mut dsu, i)).or_default().push(i);
    }
    let mut components: Vec<Vec<usize>> = by_root.into_values().collect();
    // Largest first; ties broken by lowest member so the report is stable
    // between runs (HashMap iteration order is not).
    components.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));

    BackboneReport {
        edges,
        components,
        pairs_evaluated: pairs.len(),
        pairs_pruned_by_distance: pruned,
        pairs_off_pack: off_pack,
        pairs_model_error: model_err,
        pairs_below_model_floor: near_field,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    }
}

fn dsu_find(parent: &mut [usize], mut a: usize) -> usize {
    while parent[a] != a {
        parent[a] = parent[parent[a]];
        a = parent[a];
    }
    a
}

fn dsu_union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (dsu_find(parent, a), dsu_find(parent, b));
    if ra != rb {
        parent[rb.max(ra)] = rb.min(ra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(size: usize, res: f64, hgt: f32) -> Grid {
        Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, size, size, vec![hgt; size * size])
            .unwrap()
    }

    fn link() -> LinkParams {
        let mut l = LinkParams::eu868_defaults();
        l.freq_mhz = 868.0;
        l.loc_pct = 50.0;
        l
    }

    fn site(x: f64, y: f64, h: f64, label: &str) -> SiteSpec {
        SiteSpec { xy: Xy { x, y }, h_agl_m: h, tx_power_dbm: None, label: label.into() }
    }

    fn params(radius_m: f64, budget_db: f32) -> GapParams {
        let mut p = GapParams::defaults(link());
        p.radius_m = radius_m;
        p.budget_db = budget_db;
        p.rx_h_agl_m = 2.0;
        p.max_azimuths = Some(96);
        p.backbone_margin_db = 0.0;
        p
    }

    /// Two sites far enough apart that neither budget reaches the middle. The
    /// people in that middle must show up as the top-ranked cluster, and the
    /// cluster must point at the nearest site so an operator can tell "raise
    /// an existing mast" from "build something new".
    #[test]
    fn two_separated_sites_leave_a_hole_between_them_that_ranks_first() {
        let n = 301usize;
        let res = 30.0; // 9 km square
        let terrain = flat(n, res, 40.0);
        // Residents in three blobs: one on each site, one exactly between.
        let mut pop = vec![0f32; n * n];
        let mut blob = |cx: f64, cy: f64, r: f64, v: f32| {
            for row in 0..n {
                for col in 0..n {
                    let x = col as f64 * res;
                    let y = -(row as f64) * res;
                    if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() <= r {
                        pop[row * n + col] = v;
                    }
                }
            }
        };
        blob(2000.0, -4500.0, 300.0, 5.0);
        blob(7000.0, -4500.0, 300.0, 5.0);
        blob(4500.0, -4500.0, 400.0, 9.0);
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, pop).unwrap();

        let sites =
            vec![site(2000.0, -4500.0, 15.0, "west"), site(7000.0, -4500.0, 15.0, "east")];
        // Measured on this flat profile at 868 MHz, 15 m over 2 m: 80.7 dB at
        // 300 m, 105.9 dB at 2.5 km. A 95 dB budget therefore serves the two
        // blobs sitting on the sites and leaves the 2.5 km midpoint dark —
        // while the midpoint still lies inside both sweep windows (radius
        // 3 km), so the hole is MODELLED, not inferred from distance.
        let p = params(3000.0, 95.0);
        let out = analyse(&terrain, None, Some(&population), &sites, &p).unwrap();

        assert_eq!(out.sites.len(), 2);
        assert!(out.population.uncovered > 0.0);
        let top = &out.gaps[0];
        assert!(
            (top.centroid.x - 4500.0).abs() < 800.0 && (top.centroid.y + 4500.0).abs() < 800.0,
            "top gap at ({}, {})",
            top.centroid.x,
            top.centroid.y
        );
        assert!(top.uncovered_population > 0.0);
        assert!(top.area_km2 > 0.0);
        // The middle blob is ~2.5 km from either site: a new site, not a
        // taller mast on an existing one.
        assert!(
            top.nearest_site_m > 1500.0 && top.nearest_site_m < 3500.0,
            "nearest site {} m",
            top.nearest_site_m
        );
        assert!(top.nearest_site.is_some());
        // It was inside a sweep window, so it is a computed hole.
        assert_eq!(top.unevaluated_population, 0.0);
        // And the two sites' own blobs are served.
        assert!(out.population.served > 0.0);
    }

    /// Redundancy is a stricter question than coverage, and the numbers must
    /// say so: k=2 can never report more served demand than k=1.
    #[test]
    fn requiring_two_sites_serves_strictly_less_population_than_requiring_one() {
        let n = 201usize;
        let res = 30.0;
        let terrain = flat(n, res, 40.0);
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, vec![2.0f32; n * n])
                .unwrap();
        // Overlapping pair plus a loner: the loner's neighbourhood has k=1
        // coverage only, so k=2 must lose it.
        let sites = vec![
            site(1500.0, -1500.0, 15.0, "a"),
            site(2200.0, -1500.0, 15.0, "b"),
            site(4500.0, -4500.0, 15.0, "c"),
        ];
        let p = params(2500.0, 130.0);
        let out1 = analyse(&terrain, None, Some(&population), &sites, &p).unwrap();
        let mut p2 = params(2500.0, 130.0);
        p2.k_target = 2;
        let out2 = analyse(&terrain, None, Some(&population), &sites, &p2).unwrap();

        assert_eq!(out1.population.served_k, out1.population.served);
        assert!(
            out2.population.served_k < out2.population.served,
            "k=2 {} vs k=1 {}",
            out2.population.served_k,
            out2.population.served
        );
        // The k=1 figure itself is unchanged by asking a harder question.
        assert_eq!(out1.population.served, out2.population.served);
    }

    /// A global advert dump contains nodes on other continents and rows at
    /// (0, 0). Skipping them must be a line in the report, not a panic.
    #[test]
    fn a_site_outside_the_pack_is_reported_and_skipped() {
        let n = 101usize;
        let terrain = flat(n, 30.0, 40.0);
        let sites = vec![
            site(1500.0, -1500.0, 12.0, "inside"),
            site(9.0e6, 9.0e6, 12.0, "another continent"),
            site(-4000.0, 4000.0, 12.0, "north-west of the pack"),
        ];
        let p = params(1500.0, 130.0);
        let out = analyse(&terrain, None, None, &sites, &p).unwrap();
        assert_eq!(out.sites.len(), 1);
        assert_eq!(out.skipped.len(), 2);
        assert!(out.skipped.iter().all(|s| s.reason == SkipReason::OutsidePack));
        assert_eq!(out.skipped[0].label, "another continent");
    }

    /// Two radios on one rooftop are one node, not two: they may not satisfy
    /// a k=2 redundancy target between them.
    #[test]
    fn co_located_sites_are_deduplicated_and_cannot_satisfy_k_target_twice() {
        let n = 101usize;
        let res = 30.0;
        let terrain = flat(n, res, 40.0);
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, vec![1.0f32; n * n])
                .unwrap();
        // Both inside the same 30 m pixel.
        let sites = vec![
            site(1500.0, -1500.0, 12.0, "roof radio 1"),
            site(1505.0, -1495.0, 12.0, "roof radio 2"),
        ];
        let mut p = params(1500.0, 135.0);
        p.k_target = 2;
        let out = analyse(&terrain, None, Some(&population), &sites, &p).unwrap();
        assert_eq!(out.sites.len(), 1);
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].reason, SkipReason::DuplicateOfSite(0));
        assert_eq!(out.population.served_k, 0.0);
        assert!(out.population.served > 0.0);
    }

    /// Nothing may leak: every resident is served, uncovered, on a nodata
    /// cell, or outside the pack. The grid here is built so the arithmetic is
    /// exact.
    #[test]
    fn population_totals_do_not_leak_between_served_and_uncovered() {
        let n = 101usize;
        let res = 30.0;
        let mut t = vec![40.0f32; n * n];
        // A nodata hole with residents in it: "we cannot say" must not become
        // "nobody covers them".
        for row in 10..14 {
            for col in 10..14 {
                t[row * n + col] = f32::NAN;
            }
        }
        let terrain = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, t).unwrap();
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, vec![1.0f32; n * n])
                .unwrap();
        let sites = vec![site(1500.0, -1500.0, 15.0, "only")];
        let p = params(1200.0, 130.0);
        let out = analyse(&terrain, None, Some(&population), &sites, &p).unwrap();

        let all = (n * n) as f64;
        assert_eq!(out.population.no_data, 16.0);
        assert_eq!(out.population.total, all - 16.0);
        assert_eq!(
            out.population.served + out.population.uncovered,
            out.population.total
        );
        assert!(out.population.served > 0.0);
        // Demand outside the single sweep window exists on this grid and must
        // be counted as uncovered-but-unevaluated rather than silently lost.
        assert!(out.population.uncovered_beyond_sweep > 0.0);
        assert!(out.population.uncovered_beyond_sweep <= out.population.uncovered);
        // Cluster totals are the same uncovered demand, just binned.
        let binned: f64 = out.gaps.iter().map(|g| g.uncovered_population).sum();
        assert!((binned - out.population.uncovered).abs() < 1e-6);
    }

    /// The near-field rule: inside P.1812's 0.25 km floor the raster is NaN
    /// ("no number"), and those cells must still count as served.
    #[test]
    fn cells_inside_the_model_floor_count_as_served() {
        let n = 101usize;
        let res = 30.0;
        let terrain = flat(n, res, 40.0);
        let sites = vec![site(1500.0, -1500.0, 15.0, "only")];
        let p = params(1200.0, 130.0);
        let out = analyse(&terrain, None, None, &sites, &p).unwrap();
        let probe = |x: f64, y: f64| -> u8 {
            let col = ((x - out.served.origin.x) / out.served.dx_m).round() as usize;
            let row = ((y - out.served.origin.y) / out.served.dy_m).round() as usize;
            out.served.data[row * out.served.width + col]
        };
        assert_eq!(probe(1500.0, -1500.0), 1, "the site's own pixel");
        assert_eq!(probe(1590.0, -1500.0), 1, "90 m out, under the floor");
        // A window corner lies outside the sweep DISC: never evaluated, so
        // NOT_EVALUATED — "no data", not a zero-coverage claim.
        assert_eq!(probe(330.0, -330.0), NOT_EVALUATED, "window corner must stay no-data");
    }

    /// The distinction (B) exists for: two sites that can talk are one
    /// component, and a third out of reach is an island of its own even
    /// though it covers its own neighbourhood perfectly well.
    #[test]
    fn close_sites_form_one_backbone_component_and_a_distant_site_is_an_island() {
        let n = 301usize;
        let res = 30.0; // 9 km square
        let terrain = flat(n, res, 40.0);
        let sites = vec![
            site(1500.0, -1500.0, 20.0, "a"),
            site(2600.0, -1500.0, 20.0, "b"),
            site(7500.0, -7500.0, 20.0, "far"),
        ];
        // Sweep cap spans the whole grid, so the far site is separated by
        // physics, not by the distance filter. Measured 20 m to 20 m on this
        // flat profile: 91.9 dB at the 1.1 km pair, 109.8 dB at the 8.5 km
        // one, so a 100 dB budget less 5 dB of backbone margin cuts exactly
        // between them.
        let mut p = params(9000.0, 100.0);
        p.backbone_margin_db = 5.0;
        p.max_azimuths = Some(32); // the raster is irrelevant to this test
        let out = analyse(&terrain, None, None, &sites, &p).unwrap();
        let bb = &out.backbone;
        assert_eq!(bb.components.len(), 2, "components: {:?}", bb.components);
        assert_eq!(bb.giant().unwrap(), &[0, 1]);
        assert_eq!(bb.islands().len(), 1);
        assert_eq!(bb.isolated(), vec![2]);
        assert!(bb.edges.iter().any(|e| e.a == 0 && e.b == 1));
        assert!(bb.pairs_evaluated >= 3);
    }

    /// A parameter set P.1812 refuses makes EVERY evaluation fail, and both
    /// passes read a failed evaluation as "nothing here". Before the up-front
    /// probe this returned Ok with 442 of 40 401 residents served (the
    /// near-field discs, which never call the model) and every site an island:
    /// a report that looks like a catastrophic network, not like a typo.
    #[test]
    fn a_link_the_model_refuses_is_an_error_not_an_empty_network() {
        let n = 201usize;
        let terrain = flat(n, 30.0, 40.0);
        let sites =
            vec![site(1500.0, -1500.0, 15.0, "a"), site(2200.0, -1500.0, 15.0, "b")];
        for bad in [
            {
                let mut p = params(2500.0, 130.0);
                p.link.time_pct = 0.5; // P.1812 Table 1: 1..50
                p
            },
            {
                let mut p = params(2500.0, 130.0);
                p.link.freq_mhz = 12.0; // below the model's 30 MHz floor
                p
            },
            {
                let mut p = params(2500.0, 130.0);
                p.link.loc_pct = 99.9; // P.1812: 1..99
                p
            },
        ] {
            let e = analyse(&terrain, None, None, &sites, &bad);
            assert!(
                matches!(e, Err(CoverageError::UnusableLink(_))),
                "expected UnusableLink, got a report"
            );
        }
        // The same run with the parameters left alone still works, so the
        // probe is not rejecting anything legitimate.
        assert!(analyse(&terrain, None, None, &sites, &params(2500.0, 130.0)).is_ok());
    }

    /// "The model refused this pair" and "there is no terrain under this pair"
    /// are different failures and used to share a counter, so a rejected
    /// parameter set read as a hole in the pack.
    #[test]
    fn a_refused_pair_is_counted_as_a_model_error_not_as_missing_terrain() {
        let n = 201usize;
        let terrain = flat(n, 30.0, 40.0);
        let mut p = params(3000.0, 130.0);
        let status: Vec<SiteStatus> = [(1500.0, -1500.0), (3000.0, -1500.0)]
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| SiteStatus {
                index: i,
                xy: Xy { x, y },
                label: format!("s{i}"),
                h_agl_m: 15.0,
                tx_power_dbm: None,
                budget_db: p.budget_db,
                window_clipped: false,
                served_cells: 0,
            })
            .collect();
        let good = super::backbone_graph(&terrain, None, &status, &p);
        assert_eq!(good.pairs_model_error, 0);
        assert_eq!(good.pairs_off_pack, 0);
        assert_eq!(good.edges.len(), 1);

        p.link.time_pct = 0.5;
        let bad = super::backbone_graph(&terrain, None, &status, &p);
        assert_eq!(bad.edges.len(), 0);
        assert_eq!(bad.pairs_model_error, 1, "the refusal must be named as one");
        assert_eq!(bad.pairs_off_pack, 0, "the pack has terrain everywhere here");
    }

    /// A hole in the DTM must not be walked over. P.1812's internal clamps eat
    /// NaN (`NaN.max(x)` is `x` in Rust), so feeding a nodata sample into a
    /// profile returns a plausible finite loss over ground nobody measured:
    /// measured 91.4 dB across a nodata column, i.e. "fully covered".
    #[test]
    fn a_nodata_column_is_not_walked_over_as_if_it_were_ground() {
        let n = 201usize;
        let res = 30.0;
        let mut t = vec![40.0f32; n * n];
        // One nodata column at x = 1800 m, between the site and the residents.
        for row in 0..n {
            t[row * n + 60] = f32::NAN;
        }
        let terrain = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, t).unwrap();
        let mut pop = vec![0f32; n * n];
        for row in 45..55 {
            for col in 78..84 {
                pop[row * n + col] = 1.0; // x ~ 2.4 km, well inside the sweep
            }
        }
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, pop).unwrap();
        let sites = vec![site(1500.0, -1500.0, 15.0, "only")];
        let p = params(2500.0, 135.0);
        let out = analyse(&terrain, None, Some(&population), &sites, &p).unwrap();
        assert_eq!(out.population.total, 60.0);
        assert_eq!(
            out.population.served, 0.0,
            "coverage was claimed across a hole in the terrain"
        );
        assert_eq!(out.population.uncovered, 60.0);

        // The same geometry with the hole filled in IS covered, so the test
        // above is about the nodata and not about the budget or the range.
        let solid = flat(n, res, 40.0);
        let out2 = analyse(&solid, None, Some(&population), &sites, &p).unwrap();
        assert_eq!(out2.population.served, 60.0);
    }

    /// A site standing on nodata cannot start a profile. Its own pixel and the
    /// bilinear neighbourhood the radials sample both have to be data, so a
    /// site one pixel from a hole is skipped by name rather than coming back
    /// covering only its near-field disc.
    #[test]
    fn a_site_standing_on_nodata_is_skipped_with_its_own_reason() {
        let n = 101usize;
        let res = 30.0;
        let mut t = vec![40.0f32; n * n];
        t[50 * n + 50] = f32::NAN; // x = 1500, y = -1500
        let terrain = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, t).unwrap();
        // The second one rounds to the intact pixel next door, but it sits
        // half a cell off centre, so the bilinear stencil every radial starts
        // from still straddles the hole. (An advert lands anywhere; only a
        // site exactly on a cell centre reads one pixel.)
        let sites = vec![
            site(1500.0, -1500.0, 12.0, "on the hole"),
            site(1515.0, -1500.0, 12.0, "half a cell from the hole"),
            site(2400.0, -2400.0, 12.0, "clear"),
        ];
        let p = params(1200.0, 130.0);
        let out = analyse(&terrain, None, None, &sites, &p).unwrap();
        assert_eq!(out.sites.len(), 1);
        assert_eq!(out.sites[0].label, "clear");
        assert_eq!(out.skipped.len(), 2);
        assert!(out.skipped.iter().all(|s| s.reason == SkipReason::NoTerrainData));
    }

    /// A node running 14 dBm may not be credited with a 27 dBm node's
    /// coverage. `ref_tx_power_dbm` plus a per-site power is the only way the
    /// budget shifts, and neither the shift nor its arithmetic was exercised.
    #[test]
    fn a_site_running_below_the_reference_power_gets_a_smaller_budget_and_less_reach() {
        let n = 201usize;
        let res = 30.0;
        let terrain = flat(n, res, 40.0);
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, vec![1.0f32; n * n])
                .unwrap();
        let mut weak = vec![site(3000.0, -3000.0, 15.0, "weak")];
        weak[0].tx_power_dbm = Some(14.0);
        let strong = vec![site(3000.0, -3000.0, 15.0, "strong")];

        // 100 dB and 87 dB bracket a real difference in reach on this flat
        // profile (measured 15 m over 2 m: 80.7 dB at 300 m, 105.9 dB at
        // 2.5 km), whereas two budgets both above the loss at the sweep cap
        // would come out identical and prove nothing.
        let mut p = params(2500.0, 100.0);
        p.ref_tx_power_dbm = Some(27.0);
        let out_weak = analyse(&terrain, None, Some(&population), &weak, &p).unwrap();
        // 27 - 14 = 13 dB of deficit comes straight off the budget.
        assert!((out_weak.sites[0].budget_db - 87.0).abs() < 1e-4);
        let out_ref = analyse(&terrain, None, Some(&population), &strong, &p).unwrap();
        assert!((out_ref.sites[0].budget_db - 100.0).abs() < 1e-4);
        assert!(
            out_weak.population.served < out_ref.population.served,
            "weak {} vs reference {}",
            out_weak.population.served,
            out_ref.population.served
        );

        // Without a reference power there is nothing to be short of, so the
        // per-site power must be ignored rather than half-applied.
        let mut p_noref = params(2500.0, 100.0);
        p_noref.ref_tx_power_dbm = None;
        let out_noref = analyse(&terrain, None, Some(&population), &weak, &p_noref).unwrap();
        assert!((out_noref.sites[0].budget_db - 100.0).abs() < 1e-4);
    }

    /// Cluster bins are addressed row-major over the PACK, so a pack that is
    /// not square is the only shape that can tell a correct bin index from a
    /// transposed one. Every other test here uses a square grid, where the two
    /// agree.
    #[test]
    fn gap_clusters_land_in_the_right_bin_on_a_non_square_pack() {
        let (w, h) = (301usize, 101usize); // 9 km x 3 km
        let res = 30.0;
        let terrain =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, w, h, vec![40.0f32; w * h])
                .unwrap();
        // Residents in the far south-east corner only, nowhere near the site.
        let mut pop = vec![0f32; w * h];
        for row in 90..100 {
            for col in 290..300 {
                pop[row * w + col] = 3.0;
            }
        }
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, w, h, pop).unwrap();
        let sites = vec![site(300.0, -300.0, 15.0, "north-west")];
        let p = params(1500.0, 130.0); // bin edge = 2 x 1500 m = 100 px
        let out = analyse(&terrain, None, Some(&population), &sites, &p).unwrap();
        assert_eq!(out.cluster_bin_m, 3000.0);
        assert_eq!(out.gaps.len(), 1, "one blob, one cluster");
        let g = &out.gaps[0];
        assert!(
            (g.centroid.x - 8835.0).abs() < 200.0 && (g.centroid.y + 2835.0).abs() < 200.0,
            "cluster centroid at ({}, {})",
            g.centroid.x,
            g.centroid.y
        );
        assert_eq!(g.uncovered_population, 300.0);
        assert_eq!(g.nearest_site, Some(0));
        assert!(g.nearest_site_m > 8000.0);
    }

    /// `DuplicateOfSite` carries an index into the CALLER's slice, not into
    /// `GapReport::sites`. The two are equal whenever nothing was skipped
    /// first, which is what every other duplicate test happened to arrange.
    #[test]
    fn the_duplicate_reason_indexes_the_callers_slice_not_the_accepted_list() {
        let n = 101usize;
        let terrain = flat(n, 30.0, 40.0);
        let sites = vec![
            site(9.0e6, 9.0e6, 12.0, "another continent"), // caller index 0
            site(1500.0, -1500.0, 12.0, "roof radio 1"),   // caller 1, accepted 0
            site(1505.0, -1495.0, 12.0, "roof radio 2"),   // duplicate of caller 1
        ];
        let p = params(1200.0, 130.0);
        let out = analyse(&terrain, None, None, &sites, &p).unwrap();
        assert_eq!(out.sites.len(), 1);
        assert_eq!(out.sites[0].index, 1);
        let dup = out.skipped.iter().find(|s| s.index == 2).unwrap();
        assert_eq!(dup.reason, SkipReason::DuplicateOfSite(1));
    }

    /// Cost of the OTHER half: one point-to-area sweep at Berlin's resolution
    /// and sweep cap. The backbone measurement below covers the pair pass
    /// only, and the pair pass is the cheap half — a full gap run is this
    /// number times the number of accepted sites (divided by the sweep batch),
    /// which is what decides whether the tool is usable on the 2c/2GB VPS.
    /// Ignored by default: a measurement, not an assertion.
    /// `cargo test -p planner-coverage --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn one_site_sweep_cost_at_berlin_resolution() {
        let n = 2100usize; // 21 km at 10 m: one 10 km sweep plus margin
        let res = 10.0;
        let mut data = vec![0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let (fr, fc) = (row as f32 / 180.0, col as f32 / 220.0);
                data[row * n + col] = 35.0 + 25.0 * (fr.sin() + fc.cos());
            }
        }
        let terrain = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap();
        let sites = vec![site(10_500.0, -10_500.0, 18.0, "centre")];
        for azimuths in [128usize, 512] {
            let mut p = params(10_000.0, 135.0);
            p.max_azimuths = Some(azimuths);
            let t0 = std::time::Instant::now();
            let out = analyse(&terrain, None, None, &sites, &p).unwrap();
            println!(
                "sweep 10 km @ 10 m, {} azimuths: {} ms, {} cells served, \
                 {:.1}% of the pack",
                azimuths,
                t0.elapsed().as_millis(),
                out.sites[0].served_cells,
                100.0 * out.population.served / out.population.total
            );
        }
    }

    /// Cost of the backbone pass at the size Berlin actually is: ~400
    /// repeaters inside a 30 km box on a 10 m pack. Ignored by default
    /// because it is a measurement, not an assertion.
    /// `cargo test -p planner-coverage --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn backbone_pass_cost_at_four_hundred_sites() {
        let n = 3000usize; // 30 km at 10 m, the Berlin pack's resolution
        let res = 10.0;
        // Gently rolling ground so profiles exercise the diffraction path
        // rather than a degenerate flat one.
        let mut data = vec![0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let (fr, fc) = (row as f32 / 180.0, col as f32 / 220.0);
                data[row * n + col] = 35.0 + 25.0 * (fr.sin() + fc.cos());
            }
        }
        let terrain = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap();
        // 400 sites on a jittered lattice across the box.
        let mut sites = Vec::new();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for i in 0..400 {
            let (gx, gy) = ((i % 20) as f64, (i / 20) as f64);
            let x = (gx + rnd()) * 1450.0 + 200.0;
            let y = -((gy + rnd()) * 1450.0 + 200.0);
            sites.push(site(x, y, 8.0 + 20.0 * rnd(), &format!("n{i}")));
        }
        let mut p = params(10_000.0, 135.0);
        p.max_azimuths = Some(64);
        let status: Vec<SiteStatus> = sites
            .iter()
            .enumerate()
            .map(|(i, s)| SiteStatus {
                index: i,
                xy: s.xy,
                label: s.label.clone(),
                h_agl_m: s.h_agl_m,
                tx_power_dbm: None,
                budget_db: p.budget_db,
                window_clipped: false,
                served_cells: 0,
            })
            .collect();
        println!(
            "threads: {}",
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
        );
        // Once with a realistic 10 km cap (pruning does most of the work),
        // once with a cap that spans the box so every one of the 79 800 pairs
        // is profiled — the honest worst case for n = 400.
        for cap_m in [10_000.0, 45_000.0] {
            p.radius_m = cap_m;
            let bb = super::backbone_graph(&terrain, None, &status, &p);
            println!(
                "backbone n=400 cap={:.0} km: {} pairs profiled, {} pruned, {} edges, \
                 {} components, {} ms",
                cap_m / 1000.0,
                bb.pairs_evaluated,
                bb.pairs_pruned_by_distance,
                bb.edges.len(),
                bb.components.len(),
                bb.elapsed_ms
            );
            assert!(bb.pairs_evaluated > 0);
        }
    }

    // -- the census working set -------------------------------------------

    /// The shape of the Berlin 5 m census, restated so the memory tests below
    /// are arguing about the run that motivated them: `berlin-city-5m-geo`,
    /// 10 742 × 9 127 cells (the pack the 2777 MB peak was measured on --
    /// `berlin-city-5m-ops` is 10 363 × 8 675 and is NOT that pack), 12 km
    /// sweep cap, 5 m cells, 512 azimuths.
    const BERLIN_R_M: f64 = 12_000.0;
    const BERLIN_RES_M: f64 = 5.0;
    const BERLIN_AZ: usize = 512;
    const BERLIN_PACK_CELLS: u64 = 10_742 * 9_127;
    /// Three f32 pack windows (terrain, clutter, population) plus the u8
    /// census raster — what the run holds for its whole duration.
    const BERLIN_FLOOR: u64 = BERLIN_PACK_CELLS * (3 * 4 + 1);

    /// The per-sweep cost, term by term, at the Berlin numbers.
    ///
    /// Pinned rather than derived in the test because these three figures are
    /// the whole argument for the batch size, and a formula that quietly
    /// changed shape would otherwise still "agree with itself".
    #[test]
    fn one_sweeps_working_set_is_the_window_raster_plus_two_small_things() {
        let steps = 2400u64; // 12 km / 5 m
        let side = 2 * steps + 3; // 4803
        let raster = side * side * 4; // 92.3 MB
        let table = BERLIN_AZ as u64 * steps * 4; // 4.9 MB
        let hits = side * side.div_ceil(64) * 8; // 2.9 MB, a whole word per row
        assert_eq!(
            per_site_bytes(BERLIN_R_M, BERLIN_RES_M, BERLIN_AZ),
            raster + table + hits,
            "per-site cost is the sweep raster, its polar table and the hit bitset"
        );
        assert_eq!(per_site_bytes(BERLIN_R_M, BERLIN_RES_M, BERLIN_AZ), 100_110_660);
        // The bitset replaced a `Vec<u32>` of served cell indices, whose own
        // worst case is 2*pi*(R/res)^2*4 = 145 MB — bigger than the raster the
        // indices were extracted from, and the single largest term in the old
        // per-site cost.
        let old_index_list = (2.0 * std::f64::consts::PI * (steps * steps) as f64 * 4.0) as u64;
        assert!(
            old_index_list > raster,
            "the index list really was the biggest term: {old_index_list} vs {raster}"
        );
        assert!(
            old_index_list / hits >= 40,
            "and the bitset is at least 40x smaller: {old_index_list} vs {hits}"
        );
    }

    /// THE DEFECT: the batch was one sweep per core, which is a statement
    /// about CPUs and not about the bytes it exists to bound.
    ///
    /// The old rule was `min(cores, 16)` whatever the sweeps cost. Measured
    /// peak RSS of the Berlin 5 m census (253 accepted sites, 12 km cap, 512
    /// azimuths) on a 12-core workstation: 2250 MB. That is what a 2 GB
    /// machine would have been asked for had it had twelve cores, and it is
    /// the same arithmetic as the assertion below — 1.27 GB of pack windows
    /// plus twelve 100 MB sweeps. Memory has to be the input.
    #[test]
    fn a_small_machine_gets_a_small_batch_however_many_cores_it_has() {
        let per = per_site_bytes(BERLIN_R_M, BERLIN_RES_M, BERLIN_AZ);
        let two_gib = 2 * 1024 * 1024 * 1024;
        assert_eq!(
            sweep_batch_for(two_gib, BERLIN_FLOOR, per, 12),
            2,
            "0.7 * 2 GiB less a 1.27 GB floor affords two 100 MB sweeps, not twelve"
        );
        // The rule this replaced, spelled out: cores, clamped to 16, memory
        // never consulted. On this pack it asks for more than the machine is.
        let old_rule = 12usize.clamp(1, 16);
        assert_ne!(sweep_batch_for(two_gib, BERLIN_FLOOR, per, 12), old_rule);
        assert!(
            BERLIN_FLOOR + old_rule as u64 * per > two_gib,
            "the old rule really did over-commit a 2 GB machine"
        );
        // Cores may only LOWER the answer, never raise it.
        assert_eq!(sweep_batch_for(two_gib, BERLIN_FLOOR, per, 1), 1);
        // And a workstation is limited by its pool, not by its RAM: 32 GiB
        // affords 227 sweeps and gets the 12 there are threads for.
        let thirty_two_gib = 32 * 1024 * 1024 * 1024;
        assert_eq!(sweep_batch_for(thirty_two_gib, BERLIN_FLOOR, per, 12), 12);
    }

    /// A budget the floor already exceeds still runs, one site at a time.
    ///
    /// Returning 0 would `step_by(0)` and panic, and refusing to run is not a
    /// better answer than running slowly: the alternative to a small batch is
    /// no census at all.
    #[test]
    fn the_batch_is_never_zero_however_hopeless_the_budget() {
        let per = per_site_bytes(BERLIN_R_M, BERLIN_RES_M, BERLIN_AZ);
        assert_eq!(sweep_batch_for(0, BERLIN_FLOOR, per, 12), 1);
        assert_eq!(sweep_batch_for(64 * 1024 * 1024, BERLIN_FLOOR, per, 12), 1);
        // A degenerate per-site figure must not divide by zero either.
        assert_eq!(sweep_batch_for(8 * 1024 * 1024 * 1024, 0, 0, 8), 8);
    }

    /// Whatever the platform probe does, the batch built on it must be usable.
    ///
    /// Deliberately NOT asserting a lower bound of 2 GiB: on Linux the probe
    /// reports `MemAvailable`, and a busy CI container legitimately has less
    /// than that. What must hold on every platform is that the figure is a
    /// plausible number of bytes and that a batch derived from it still runs.
    #[test]
    fn the_memory_probe_reports_something_a_batch_can_be_built_on() {
        let b = usable_memory_bytes();
        assert!(b > 0, "a zero budget would divide the batch to nothing");
        assert!(b < 1 << 50, "and 1 PiB is not a machine this runs on, got {b}");
        let per = per_site_bytes(BERLIN_R_M, BERLIN_RES_M, BERLIN_AZ);
        assert!((1..=12).contains(&sweep_batch_for(b, BERLIN_FLOOR, per, 12)));
    }

    /// Counting in a byte has two ways to invert its own answer, and both of
    /// them are silent.
    #[test]
    fn a_count_saturates_and_never_wraps_into_not_evaluated() {
        let mut c = 0u8;
        bump_count(&mut c);
        assert_eq!(c, 1);
        // The cell every site on the map reaches must not read as one nobody
        // looked at: 254 + 1 is 255, which IS the not-evaluated code.
        let mut c = MAX_COUNT;
        bump_count(&mut c);
        assert_eq!(c, MAX_COUNT, "saturate, do not wrap into NOT_EVALUATED");
        // And a cell with no terrain data under it stays that way — bumping it
        // would turn "we cannot say" into "evaluated, nobody reaches it".
        let mut c = NOT_EVALUATED;
        bump_count(&mut c);
        assert_eq!(c, NOT_EVALUATED);
    }

    /// The bitset must hand the merge exactly the cells the sweep set, in
    /// PARENT coordinates, and count cells rather than calls.
    #[test]
    fn the_hit_bitset_round_trips_every_cell_it_was_given() {
        let (w, h) = (13usize, 7usize);
        let mut hits = SweepHits::new(100, 200, w, h);
        let want = [(0usize, 0usize), (0, 12), (3, 5), (6, 12), (4, 0)];
        for (r, c) in want {
            hits.set(r, c);
        }
        // A repeated set is not a second cell: `served_cells` is a count of
        // pixels, and double counting one would inflate every site's reach.
        hits.set(3, 5);
        assert_eq!(hits.count(), want.len());
        let mut got: Vec<(usize, usize)> = Vec::new();
        hits.for_each_cell(|r, c| got.push((r, c)));
        let mut expect: Vec<(usize, usize)> =
            want.iter().map(|(r, c)| (100 + r, 200 + c)).collect();
        expect.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, expect);
        // The last word carries padding past w*h; nothing may come out of it.
        assert!(got.iter().all(|(r, c)| *r < 100 + h && *c < 200 + w));

        // A window wider than one word, with cells ON the word boundaries —
        // the row-padded layout is what makes those the interesting indices,
        // and 64 landing in row r+1 instead of row r would move a site's
        // coverage a whole row north on every merge.
        let (w, h) = (200usize, 5usize);
        let mut wide = SweepHits::new(0, 0, w, h);
        let want = [(0usize, 63usize), (0, 64), (1, 127), (1, 128), (4, 199), (2, 0)];
        for (r, c) in want {
            wide.set(r, c);
        }
        assert_eq!(wide.count(), want.len());
        let mut got: Vec<(usize, usize)> = Vec::new();
        wide.for_each_cell(|r, c| got.push((r, c)));
        let mut expect = want.to_vec();
        expect.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, expect);
    }

    /// The census raster must land on the same cells a loss raster does.
    ///
    /// `CountGrid` carries its own copy of the nearest-sample domain because
    /// it is not a `Grid`. If the two ever disagree, the census overlay sits
    /// half a cell off the coverage overlay it is meant to be compared with —
    /// which reads as the two models disagreeing about a street.
    #[test]
    fn a_count_raster_samples_exactly_where_a_loss_raster_would() {
        let (w, h, res) = (9usize, 6usize, 25.0);
        let origin = Xy { x: 1000.0, y: 2000.0 };
        let values: Vec<u8> = (0..w * h).map(|i| (i % 7) as u8).collect();
        let counts = CountGrid {
            origin,
            dx_m: res,
            dy_m: -res,
            width: w,
            height: h,
            data: values.clone(),
        };
        let grid = Grid::with_axes(
            origin,
            res,
            -res,
            w,
            h,
            values.iter().map(|v| *v as f32).collect(),
        )
        .unwrap();
        // Walk well past the raster on both axes, at a third of a cell, so the
        // half-cell edges of the domain are crossed rather than stepped over.
        let step = res / 3.0;
        for i in -12i32..40 {
            for j in -12i32..40 {
                let p = Xy { x: origin.x + i as f64 * step, y: origin.y - j as f64 * step };
                assert_eq!(
                    counts.sample_nearest(p),
                    grid.sample_nearest(p),
                    "disagreement at {p:?}"
                );
            }
        }
        // The one deliberate difference: an empty raster is None, not a panic.
        let empty =
            CountGrid { origin, dx_m: res, dy_m: -res, width: 0, height: 0, data: Vec::new() };
        assert!(empty.sample_nearest(origin).is_none());
    }

    /// 255 is a code, not a count.
    #[test]
    fn not_evaluated_reads_as_no_number_and_never_as_255_repeaters() {
        let g = CountGrid {
            origin: Xy { x: 0.0, y: 0.0 },
            dx_m: 10.0,
            dy_m: -10.0,
            width: 2,
            height: 1,
            data: vec![3, NOT_EVALUATED],
        };
        assert_eq!(g.count_at_world(0.0, 0.0), 3.0);
        assert!(g.count_at_world(10.0, 0.0).is_nan());
        assert!(g.count_at_world(-100.0, 0.0).is_nan(), "off the raster");
        assert_eq!(g.count(0, 1), NOT_EVALUATED);
        assert_eq!(g.count(9, 9), NOT_EVALUATED, "off the raster is not evaluated either");
    }

    /// The census answer must not depend on how the raster is stored.
    ///
    /// The counts moved from f32 to u8 and the per-site hand-off from an index
    /// list to a bitset; neither is allowed to move a single resident between
    /// served, uncovered and no-data. This re-states the invariant on a pack
    /// with a nodata column, sites at k=2, and a cell reached by both sites,
    /// which is the combination that exercises every branch of the merge.
    #[test]
    fn every_resident_is_accounted_for_exactly_once_over_a_byte_raster() {
        let n = 141usize;
        let res = 30.0;
        let mut terrain = flat(n, res, 30.0);
        for row in 0..n {
            terrain.data[row * n + 4] = f32::NAN;
        }
        let mut pop = flat(n, res, 0.0);
        for (i, v) in pop.data.iter_mut().enumerate() {
            *v = ((i % 5) + 1) as f32;
        }
        let sites = vec![site(1800.0, -1800.0, 20.0, "a"), site(2400.0, -2100.0, 20.0, "b")];
        let mut p = params(2000.0, 140.0);
        p.k_target = 2;
        let out = analyse(&terrain, None, Some(&pop), &sites, &p).unwrap();
        let all: f64 = pop.data.iter().map(|v| *v as f64).sum();
        assert!(
            (out.population.total + out.population.no_data - all).abs() < 1e-6,
            "total {} + no_data {} != {}",
            out.population.total,
            out.population.no_data,
            all
        );
        assert!(
            (out.population.served + out.population.uncovered - out.population.total).abs() < 1e-6
        );
        assert!(out.population.served > 0.0, "the two sites reach somebody");
        assert!(out.population.served_k > 0.0, "and their overlap reaches somebody twice");
        assert!(out.population.served_k <= out.population.served);
        // The raster agrees with the accounting: no cell may hold a count that
        // is neither NOT_EVALUATED nor a plausible number of sites.
        assert!(out.served.data.iter().all(|v| *v == NOT_EVALUATED || *v as usize <= sites.len()));
        assert!(out.served.data.iter().any(|v| *v == 2), "some cell is reached by both");
        // Site reach reported from the bitset must match the raster it fed.
        assert!(out.sites.iter().all(|s| s.served_cells > 0));
        // A hit on a cell with no terrain data under it is DROPPED by the
        // merge, so the raster can only be short of the reported reach, never
        // over it. The exact identity is asserted on a pack without holes by
        // `the_raster_is_exactly_the_sum_of_the_sites_hit_bitsets`.
        let raster_sum: usize =
            out.served.data.iter().filter(|v| **v != NOT_EVALUATED).map(|v| *v as usize).sum();
        let reported: usize = out.sites.iter().map(|s| s.served_cells).sum();
        assert!(raster_sum <= reported, "{raster_sum} > {reported}");
    }

    /// The census raster IS the sites' hit bitsets, added up.
    ///
    /// This is the join between the parallel sweep and the serial merge, and
    /// the two halves are written in different index spaces — the bitset in
    /// the site's own sweep window, the raster in the union window. An
    /// off-by-one in either direction still produces a plausible-looking map,
    /// so the identity is asserted rather than eyeballed. On a pack with no
    /// nodata every set bit must land on a countable cell.
    #[test]
    fn the_raster_is_exactly_the_sum_of_the_sites_hit_bitsets() {
        let terrain = flat(121, 30.0, 25.0);
        // Deliberately off-centre and unequal, so the two sweep windows have
        // different offsets from the union window's origin.
        let sites = vec![site(1200.0, -900.0, 20.0, "a"), site(2400.0, -2400.0, 12.0, "b")];
        let out = analyse(&terrain, None, None, &sites, &params(1500.0, 140.0)).unwrap();
        let raster_sum: usize =
            out.served.data.iter().filter(|v| **v != NOT_EVALUATED).map(|v| *v as usize).sum();
        let reported: usize = out.sites.iter().map(|s| s.served_cells).sum();
        assert!(reported > 0, "the sites reach something at all");
        assert_eq!(raster_sum, reported);
    }
}
