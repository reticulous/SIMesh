//! Site optimization: candidate generation from the terrain layer, coverage
//! sets through the radial sweep, greedy weighted k-coverage selection
//! (planner-opt). v1 scope = single domain (review §8.3): one cell, one radio
//! config; demand weights are uniform until the population layer lands.

use crate::{coverage, CoverageError, CoverageParams};
use planner_core::geo::Xy;
use planner_core::model::LinkParams;
use planner_opt::Candidate;
use planner_terrain::Grid;
use std::collections::HashMap;

pub struct SitingParams {
    /// How many sites to propose.
    pub select_n: usize,
    /// Coverage multiplicity rewarded per demand point (k-redundancy).
    pub k_target: u8,
    pub radius_m: f64,
    /// A demand point counts as covered when Lb ≤ this (link budget).
    pub max_loss_db: f32,
    /// Demand grid stride in pixels (coarser = faster).
    pub demand_stride: usize,
    /// Local-max window half-size in pixels (candidate spacing).
    pub spacing_px: usize,
    /// Keep only the highest N candidates.
    pub max_candidates: usize,
    pub link: LinkParams,
    /// Azimuth cap per candidate sweep (coarse is fine for siting).
    pub max_azimuths: Option<usize>,
    /// Worker threads for candidate evaluation (0 = auto: min(100, cores)).
    /// Client runs stay single-core-friendly by leaving this at 1.
    pub threads: usize,
    /// Already-deployed sites (pack CRS) with the power each is actually
    /// configured to run at (dBm; None = plan ceiling). They are evaluated
    /// like candidates but LOCKED: greedy starts from the coverage they
    /// already provide and never proposes removing them. Changes the growth
    /// path — a seeded run answers "where do the next N nodes go?", which is
    /// a different question from clean-sheet siting.
    pub existing: Vec<(Xy, Option<f64>)>,
    /// Fixed-N local-search refinement rounds after greedy (0 = off).
    /// Optimizes the whole placement at the target N instead of accepting
    /// the greedy growth path; existing sites stay locked.
    pub refine_rounds: usize,
    /// Selectable transmit powers (dBm, ascending). TX power is a PER-SITE
    /// decision: siting runs at the highest level, then a reduction pass
    /// lowers each site to the least power that preserves what it uniquely
    /// contributes. Running every node at the ceiling inflates the
    /// carrier-sense radius far beyond the data radius, which merges
    /// nominally separate cells into one collision domain and degrades the
    /// mesh (the maintainer's field experiments, 2026-08-31).
    pub power_levels_dbm: Vec<f64>,
    pub antenna_gain_dbi: f64,
    pub fade_margin_db: f64,
    /// How much further carrier sense reaches than a closing data link
    /// (dB of extra tolerable path loss). Used only for the contention
    /// report, never for coverage.
    pub cs_margin_db: f64,
    /// Ceiling on total antenna height above ground (m).
    ///
    /// A site standing on clutter mounts at clutter + mast, but the clutter
    /// layer is a CELL MEAN: a 30 m cell containing a landmark tower reports
    /// ~100 m, and mounting "on the mean" silently turns a household-node
    /// planner into a broadcast-tower planner (observed: one such site
    /// claiming 3.7 M residents). Community deployments mount on ordinary
    /// roofs — Berlin's Traufhöhe is ~22 m — so cap it. Raise deliberately
    /// when planning a real mast.
    pub max_tx_agl_m: f64,
}

impl SitingParams {
    pub fn defaults(link: LinkParams) -> Self {
        Self {
            select_n: 5,
            k_target: 1,
            radius_m: 3000.0,
            max_loss_db: 135.0,
            demand_stride: 2,
            spacing_px: 6,
            max_candidates: 150,
            link,
            max_azimuths: Some(512),
            threads: 1,
            existing: Vec::new(),
            refine_rounds: 0,
            power_levels_dbm: Vec::new(),
            antenna_gain_dbi: 2.0,
            fade_margin_db: 10.0,
            cs_margin_db: 12.0,
            max_tx_agl_m: 30.0,
        }
    }
}

pub struct ChosenSite {
    pub pos: Xy,
    pub ground_masl: f32,
    /// Marginal demand weight this site added when picked (persons, when a
    /// population layer drives the weights).
    pub gain: f64,
    /// Transmit power assigned to THIS site (dBm) after the reduction pass.
    pub tx_power_dbm: f64,
    /// Other selected sites within this site's carrier-sense range at its
    /// assigned power — the co-channel contention it imposes.
    pub cs_neighbours: usize,
}

pub struct SitingResult {
    pub chosen: Vec<ChosenSite>,
    pub candidates_considered: usize,
    pub demand_points: usize,
    /// Demand weight covered ≥1× by the chosen set / total demand weight
    /// (population-weighted when a population grid was supplied).
    pub covered_fraction: f64,
    /// Total demand weight (persons with a population grid, else point count).
    pub total_weight: f64,
    /// Weight covered ≥1× (persons with a population grid).
    pub covered_weight: f64,
    /// True when weights came from a population layer.
    pub population_weighted: bool,
    /// Cumulative weight covered ≥1× after each NEW pick, index 0 = after
    /// the first pick (existing/locked sites are already accounted for in
    /// `baseline_covered`).
    ///
    /// Greedy's k-th pick never depends on the target N, so this prefix
    /// sequence IS the greedy solution at every smaller N — the curve is
    /// free. But every point on it is a GROWTH-PATH solution from this
    /// starting set; see `refined` for the fixed-N alternative.
    pub cumulative_covered: Vec<f64>,
    /// Weight already covered by the locked/existing sites before any pick.
    pub baseline_covered: f64,
    /// Fixed-N refinement outcome, when refine_rounds > 0.
    pub refined: Option<planner_opt::RefineStats>,
    /// Power-reduction outcome, when power levels were supplied.
    pub power_plan: Option<PowerPlanStats>,
}

/// What the power-reduction pass achieved. Contention is counted as
/// selected-site pairs inside each other's carrier-sense range: the
/// quantity that turns nominally separate cells into one collision domain
/// when every node runs at the ceiling.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerPlanStats {
    pub levels_dbm: Vec<f64>,
    /// Mean assigned power before/after reduction (dBm).
    pub mean_power_before: f64,
    pub mean_power_after: f64,
    /// Mean carrier-sense neighbour count per site, all-at-ceiling vs plan.
    pub mean_cs_neighbours_before: f64,
    pub mean_cs_neighbours_after: f64,
    /// Residents covered ≥1× under each regime (must be equal by
    /// construction — the pass only removes power that costs no coverage).
    pub covered_before: f64,
    pub covered_after: f64,
    pub sites_reduced: usize,
}

/// Coarse summed-area table over the population grid: O(1) rectangle sums
/// for candidate scoring without a full-resolution integral image (which
/// would cost 8 bytes/cell — 587 MB on the Berlin+Brandenburg pack).
struct PopulationPotential {
    /// Integral image over coarse blocks, (bw+1) × (bh+1).
    integral: Vec<f64>,
    bw: usize,
    bh: usize,
    block: usize,
}

impl PopulationPotential {
    fn new(pop: &Grid, block: usize) -> Self {
        let (w, h) = (pop.width, pop.height);
        let block = block.max(1);
        let (bw, bh) = (w.div_ceil(block), h.div_ceil(block));
        let mut sums = vec![0f64; bw * bh];
        for row in 0..h {
            let br = row / block;
            for col in 0..w {
                let v = pop.data[row * w + col];
                if v.is_finite() && v > 0.0 {
                    sums[br * bw + col / block] += v as f64;
                }
            }
        }
        let mut integral = vec![0f64; (bw + 1) * (bh + 1)];
        for r in 0..bh {
            let mut row_acc = 0.0;
            for c in 0..bw {
                row_acc += sums[r * bw + c];
                integral[(r + 1) * (bw + 1) + (c + 1)] =
                    integral[r * (bw + 1) + (c + 1)] + row_acc;
            }
        }
        Self { integral, bw, bh, block }
    }

    /// Population inside the pixel-space box around (row, col) of half-width
    /// `rad_px` — the reachable-demand upper bound for a candidate there.
    fn around(&self, row: usize, col: usize, rad_px: usize) -> f64 {
        let r0 = row.saturating_sub(rad_px) / self.block;
        let c0 = col.saturating_sub(rad_px) / self.block;
        let r1 = ((row + rad_px) / self.block + 1).min(self.bh);
        let c1 = ((col + rad_px) / self.block + 1).min(self.bw);
        if r1 <= r0 || c1 <= c0 {
            return 0.0;
        }
        let stride = self.bw + 1;
        self.integral[r1 * stride + c1] - self.integral[r0 * stride + c1]
            - self.integral[r1 * stride + c0]
            + self.integral[r0 * stride + c0]
    }
}

/// Candidate sites: local maxima of the surface within a (2s+1)² window,
/// then ranked and truncated to `cap`.
///
/// Ranking rule (fixed 2026-08-31 after the region-scale degeneracy found on
/// the build server): when a population grid exists, candidates rank by the
/// POPULATION REACHABLE within the coverage radius — an upper bound on the
/// site's achievable gain — with surface height as the tie-break. Ranking by
/// height alone made region-scale runs pick empty hilltops (best of 150
/// candidates covered 1 227 of 9.4 M residents), because the tallest points
/// in Brandenburg are nowhere near people. Height still decides *among*
/// candidates serving comparable populations, which is what it is good for.
fn candidate_pixels(
    surface: &Grid,
    population: Option<&PopulationPotential>,
    radius_px: usize,
    spacing: usize,
    cap: usize,
) -> Vec<(usize, usize)> {
    use std::collections::HashMap;
    let (w, h) = (surface.width, surface.height);
    let s = spacing as isize;
    let mut cands: Vec<(f64, f32, usize, usize)> = Vec::new();
    for row in 0..h {
        'px: for col in 0..w {
            let v = surface.data[row * w + col];
            if !v.is_finite() {
                continue;
            }
            for dr in -s..=s {
                for dc in -s..=s {
                    if dr == 0 && dc == 0 {
                        continue;
                    }
                    let (r2, c2) = (row as isize + dr, col as isize + dc);
                    if r2 < 0 || c2 < 0 || r2 >= h as isize || c2 >= w as isize {
                        continue;
                    }
                    let u = surface.data[r2 as usize * w + c2 as usize];
                    // Strict on value, index tie-break for plateaus.
                    if u > v || (u == v && (r2 as usize, c2 as usize) < (row, col)) {
                        continue 'px;
                    }
                }
            }
            let potential = population.map(|p| p.around(row, col, radius_px)).unwrap_or(0.0);
            cands.push((potential, v, row, col));
        }
    }
    if population.is_none() {
        cands.sort_by(|a, b| b.1.total_cmp(&a.1));
        cands.truncate(cap);
        return cands.into_iter().map(|(_, _, r, c)| (r, c)).collect();
    }

    // Reachable population first, height as tie-break; drop candidates that
    // reach nobody at all.
    cands.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
    cands.retain(|c| c.0 > 0.0);

    // SPATIAL STRATIFICATION. A pure global top-M cut collapses the pool
    // onto the densest metro area (measured: 150/150 candidates in the city,
    // 0 in the town), so a coverage-vs-N curve would flatten from POOL
    // EXHAUSTION and be misread as geographic saturation. Give every
    // populated tile a share of the cap proportional to its potential, with
    // a floor of one, then spend anything left over globally.
    let tile_px = (stride_tile_px(radius_px)).max(1);
    let tiles_across = w.div_ceil(tile_px);
    let mut by_tile: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut tile_potential: HashMap<usize, f64> = HashMap::new();
    for (idx, &(pot, _, r, c)) in cands.iter().enumerate() {
        let t = (r / tile_px) * tiles_across + c / tile_px;
        by_tile.entry(t).or_default().push(idx);
        *tile_potential.entry(t).or_insert(0.0) += pot;
    }
    let total_potential: f64 = tile_potential.values().sum();
    let n_tiles = by_tile.len();
    let mut taken = vec![false; cands.len()];
    let mut picked: Vec<usize> = Vec::with_capacity(cap);
    if n_tiles > 0 && total_potential > 0.0 {
        let mut tiles: Vec<(&usize, &Vec<usize>)> = by_tile.iter().collect();
        tiles.sort_by(|a, b| {
            tile_potential[b.0].total_cmp(&tile_potential[a.0]).then(a.0.cmp(b.0))
        });
        for (t, members) in tiles {
            let share = tile_potential[t] / total_potential;
            let quota = ((cap as f64 * share).floor() as usize).max(1).min(members.len());
            for &m in members.iter().take(quota) {
                if picked.len() < cap {
                    picked.push(m);
                    taken[m] = true;
                }
            }
        }
    }
    // Fill any remaining slots with the best untaken candidates globally.
    for idx in 0..cands.len() {
        if picked.len() >= cap {
            break;
        }
        if !taken[idx] {
            picked.push(idx);
            taken[idx] = true;
        }
    }
    picked.sort_unstable();
    picked
        .into_iter()
        .map(|i| (cands[i].2, cands[i].3))
        .collect()
}

/// Stratification tile size in pixels: about one coverage diameter, so a
/// tile is the neighborhood a single site could plausibly serve.
fn stride_tile_px(radius_px: usize) -> usize {
    (radius_px * 2).clamp(8, 4096)
}

/// `population`: persons-per-pack-cell grid sharing the terrain geometry
/// (the pack's Population layer). When present, demand weights become the
/// population of each demand point's stride block — the objective turns into
/// "residents reached"; zero-population points are dropped from the demand
/// set entirely.
pub fn optimize_sites(
    terrain: &Grid,
    clutter: Option<&Grid>,
    population: Option<&Grid>,
    p: &SitingParams,
) -> Result<SitingResult, CoverageError> {
    let (w, h) = (terrain.width, terrain.height);
    let px_center = |row: usize, col: usize| Xy {
        x: terrain.origin.x + col as f64 * terrain.dx_m,
        y: terrain.origin.y + row as f64 * terrain.dy_m,
    };

    // Candidates rank by SURFACE height (tall structures are good sites),
    // i.e. terrain + clutter when a clutter layer exists.
    let surface: Grid = match clutter {
        Some(c) if c.width == w && c.height == h => {
            let mut s = terrain.clone();
            for i in 0..s.data.len() {
                s.data[i] += c.data[i].max(0.0);
            }
            s
        }
        _ => terrain.clone(),
    };

    // Demand grid: strided pixels; weight = 1 (uniform) or the population of
    // the stride×stride block the point represents (conserves totals).
    let stride = p.demand_stride.max(1);
    let pop_ok = population.is_some_and(|g| g.width == w && g.height == h);
    if population.is_some() && !pop_ok {
        eprintln!("siting: population grid shape mismatch — falling back to uniform weights");
    }
    let mut demand_px: Vec<(usize, usize)> = Vec::new();
    let mut weights: Vec<f32> = Vec::new();
    for row in (0..h).step_by(stride) {
        for col in (0..w).step_by(stride) {
            // Anchor validity must not gate the BLOCK: testing only the NW
            // pixel dropped inhabited cells from the demand set *and* from
            // the denominator, silently inflating covered %. Accept the
            // block if any of its terrain pixels is usable.
            let mut any_terrain = false;
            let mut weight = 0f64;
            for r in row..(row + stride).min(h) {
                for c in col..(col + stride).min(w) {
                    if terrain.data[r * w + c].is_finite() {
                        any_terrain = true;
                    }
                    if pop_ok {
                        let v = population.unwrap().data[r * w + c];
                        if v.is_finite() {
                            weight += v as f64;
                        }
                    }
                }
            }
            if !any_terrain {
                continue;
            }
            let weight = if pop_ok { weight as f32 } else { 1.0 };
            if pop_ok && weight <= 0.0 {
                continue; // uninhabited — no demand
            }
            demand_px.push((row, col));
            weights.push(weight);
        }
    }
    let population_weighted = pop_ok && !demand_px.is_empty();

    // Candidate coverage sets via coarse sweeps. Seeding is
    // population-aware when a population grid exists (see candidate_pixels).
    let radius_px = (p.radius_m / terrain.dx_m.abs()).ceil() as usize;
    let potential = if pop_ok {
        // Coarse blocks ≈ radius/8 keeps the table tiny and the ranking
        // stable; the score is a bound, not a prediction.
        let block = (radius_px / 8).clamp(1, 64);
        Some(PopulationPotential::new(population.unwrap(), block))
    } else {
        None
    };
    let cand_px =
        candidate_pixels(&surface, potential.as_ref(), radius_px, p.spacing_px, p.max_candidates);
    if cand_px.is_empty() && p.existing.is_empty() {
        // "No candidate reaches anyone" must not look like "found nothing".
        return Err(CoverageError::NoCandidates);
    }
    // Per-candidate coverage sets. Each sweep is independent, so this is the
    // one place worth parallelizing (server batch runs); clients leave
    // threads = 1 and pay nothing. Demand lookup goes through a sparse index
    // so a candidate only scans the demand points inside its own footprint
    // instead of the whole region's list — the difference between O(cands ×
    // all-demand) and O(cands × local-demand) at region scale.
    let mut demand_at: HashMap<(usize, usize), u32> = HashMap::with_capacity(demand_px.len());
    for (i, &(r, c)) in demand_px.iter().enumerate() {
        demand_at.insert((r, c), i as u32);
    }
    // Power levels the optimizer may assign, ascending; the last is the
    // planning ceiling. With none supplied, behavior is the single fixed
    // budget the caller passed in.
    let levels: Vec<f64> = if p.power_levels_dbm.is_empty() {
        Vec::new()
    } else {
        let mut l = p.power_levels_dbm.clone();
        l.sort_by(f64::total_cmp);
        l
    };
    // Loss threshold per level; ceiling threshold == p.max_loss_db so the
    // no-levels path and the top level agree exactly.
    let thresholds: Vec<f32> = if levels.is_empty() {
        vec![p.max_loss_db]
    } else {
        let top = *levels.last().unwrap();
        levels.iter().map(|&pw| p.max_loss_db - (top - pw) as f32).collect()
    };

    let eval = |(_id, &(row, col)): (usize, &(usize, usize))| -> Result<Vec<Vec<u32>>, CoverageError> {
        let tx = px_center(row, col);
        // Mount height is above GROUND in P.1812 (eq. 1d excludes clutter at
        // the terminals), so a site standing on clutter must add that
        // clutter's height — otherwise a rooftop candidate is ranked by its
        // roof but simulated at street level. This is the household-install
        // geometry from review §8.1.
        let mut link = p.link.clone();
        if let Some(cg) = clutter {
            let c_h = cg.data.get(row * w + col).copied().unwrap_or(0.0);
            if c_h.is_finite() && c_h > 0.0 {
                // Clamped: see SitingParams::max_tx_agl_m — cell-mean clutter
                // can name a landmark tower, which is not a community site.
                link.tx_h_agl_m = (link.tx_h_agl_m + c_h as f64).min(p.max_tx_agl_m);
            }
        }
        let res = coverage(
            terrain,
            clutter,
            &CoverageParams {
                // The census and the optimiser compare SITES against each other.
                // A terminal correction the caller has not measured per site
                // would be the same constant everywhere and could not change
                // that comparison, so it is left to the caller to supply.
                tx_terminal_db: None,
                tx_model_h_m: None,
                rx_terminal: None,
                // The census and the optimiser run to completion; only the
                // interactive map supersedes its own sweeps.
                cancel: None,
                tx,
                radius_m: p.radius_m,
                link,
                max_azimuths: p.max_azimuths,
                // Past the budget (+ CS slack, which we also threshold on)
                // the ray adds nothing, so radials end where physics ends
                // rather than where the radius cap was set.
                stop_above_db: Some(p.max_loss_db + p.cs_margin_db as f32 + 6.0),
                stop_after_m: crate::DEFAULT_STOP_AFTER_M,
                max_profile_points: crate::MAX_PROFILE_POINTS,
                table_budget_bytes: crate::DEFAULT_TABLE_BUDGET_BYTES,
            },
        )?;
        // One sweep yields the loss field; each power level is just a
        // different threshold on it, so per-power cover sets are free.
        let mut per_power: Vec<Vec<u32>> = vec![Vec::new(); thresholds.len()];
        for lr in 0..res.loss.height {
            for lc in 0..res.loss.width {
                let lb = res.loss.data[lr * res.loss.width + lc];
                if !lb.is_finite() {
                    continue;
                }
                let key = (res.row_offset + lr, res.col_offset + lc);
                if let Some(&i) = demand_at.get(&key) {
                    for (li, &t) in thresholds.iter().enumerate() {
                        if lb <= t {
                            per_power[li].push(i);
                        }
                    }
                }
            }
        }
        Ok(per_power)
    };

    // Existing/deployed sites are evaluated exactly like candidates, then
    // locked: they seed the growth path and are never swapped out.
    let mut existing_power: Vec<Option<f64>> = Vec::new();
    let existing_px: Vec<(usize, usize)> = p
        .existing
        .iter()
        .filter_map(|(s, pw)| {
            let col = ((s.x - terrain.origin.x) / terrain.dx_m).round();
            let row = ((s.y - terrain.origin.y) / terrain.dy_m).round();
            if col < 0.0 || row < 0.0 || col >= w as f64 || row >= h as f64 {
                eprintln!("siting: existing site outside the pack — ignored");
                None
            } else {
                existing_power.push(*pw);
                Some((row as usize, col as usize))
            }
        })
        .collect();
    let n_locked = existing_px.len();
    // Locked sites occupy the first ids; candidate ids shift after them.
    let cand_px: Vec<(usize, usize)> =
        existing_px.iter().copied().chain(cand_px.into_iter()).collect();

    let per_power_all: Vec<Vec<Vec<u32>>> = if p.threads == 1 {
        cand_px.iter().enumerate().map(eval).collect::<Result<_, _>>()?
    } else {
        use rayon::prelude::*;
        let avail = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let n = if p.threads == 0 { avail.min(100) } else { p.threads };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .map_err(|e| CoverageError::Threads(e.to_string()))?;
        pool.install(|| {
            cand_px
                .par_iter()
                .enumerate()
                .map(|(id, px)| eval((id, px)))
                .collect::<Result<Vec<_>, _>>()
        })?
    };
    // Siting itself runs at the ceiling; the reduction pass spends the
    // lower levels afterwards. EXISTING sites instead use the power they
    // are actually configured for — a deployed node at 14 dBm must not be
    // credited with 27 dBm coverage.
    let top_level = thresholds.len() - 1;
    let level_for = |pw: Option<f64>| -> usize {
        match (pw, levels.is_empty()) {
            (Some(target), false) => levels
                .iter()
                .position(|&l| (l - target).abs() < 1e-9)
                .unwrap_or_else(|| {
                    // Nearest level at or below the configured power.
                    levels.iter().rposition(|&l| l <= target).unwrap_or(0)
                }),
            _ => top_level,
        }
    };
    let candidates: Vec<Candidate> = per_power_all
        .iter()
        .enumerate()
        .map(|(id, per_power)| {
            let li = if id < n_locked {
                level_for(existing_power[id])
            } else {
                top_level
            };
            Candidate { id: id as u32, covers: per_power[li].clone() }
        })
        .collect();

    // Greedy over the free candidates, seeded by whatever the locked sites
    // already cover.
    let locked: Vec<usize> = (0..n_locked).collect();
    let free_candidates: Vec<Candidate> = candidates[n_locked..].to_vec();
    let selection =
        planner_opt::greedy_max_coverage_seeded(&weights, &free_candidates, &candidates[..n_locked], p.select_n, p.k_target);

    // Optional fixed-N refinement: optimize the whole placement at the
    // target N instead of keeping the greedy growth path. Locked sites are
    // excluded from swapping (they are already installed).
    // Pick ids are already cand_px indices (Candidate.id was set that way),
    // so they must NOT be offset by n_locked again.
    let mut chosen_idx: Vec<usize> =
        selection.picks.iter().map(|p| p.candidate_id as usize).collect();
    let refined = if p.refine_rounds > 0 && !chosen_idx.is_empty() {
        // Refine over free candidates only; locked coverage is baked in by
        // seeding the counts through a pseudo-selection that is never moved.
        let mut pool: Vec<Candidate> = candidates.clone();
        // Force locked sites to stay: give the refiner the locked ones as
        // part of the selection but with a beam that never proposes them
        // for removal (they sort last by loss because we mark them).
        let mut sel = chosen_idx.clone();
        sel.extend(locked.iter().copied());
        let stats = planner_opt::refine_by_swaps(
            &weights,
            &pool,
            &mut sel,
            p.k_target,
            p.refine_rounds,
            32,
        );
        // Keep locked sites out of the reported picks.
        chosen_idx = sel.into_iter().filter(|i| *i >= n_locked).collect();
        pool.clear();
        Some(stats)
    } else {
        None
    };

    // Weighted coverage at ≥1×, accumulated pick by pick so the run also
    // yields the full coverage-vs-N curve. Locked sites form the baseline.
    let mut covered = vec![false; demand_px.len()];
    let mut baseline_covered = 0f64;
    for &s in &locked {
        for &i in &candidates[s].covers {
            if !covered[i as usize] {
                covered[i as usize] = true;
                baseline_covered += weights[i as usize] as f64;
            }
        }
    }
    let mut cumulative_covered = Vec::with_capacity(chosen_idx.len());
    let mut running = baseline_covered;
    for &ci in &chosen_idx {
        for &i in &candidates[ci].covers {
            if !covered[i as usize] {
                covered[i as usize] = true;
                running += weights[i as usize] as f64;
            }
        }
        cumulative_covered.push(running);
    }
    let total_weight: f64 = weights.iter().map(|&x| x as f64).sum();
    let covered_weight: f64 = covered
        .iter()
        .zip(&weights)
        .filter(|(&c, _)| c)
        .map(|(_, &x)| x as f64)
        .sum();
    let covered_fraction = if total_weight > 0.0 { covered_weight / total_weight } else { 0.0 };

    // ---- Per-site transmit power -------------------------------------
    // Every node at the regulatory ceiling is the configuration the maintainer's
    // field experiments show degrading the mesh: carrier-sense range grows
    // far faster than useful data range, so separate cells collapse into one
    // collision domain. Assign each site the LOWEST level that still keeps
    // the demand only it serves. Coverage is preserved exactly; contention
    // is not.
    let mut site_power: Vec<f64> = vec![levels.last().copied().unwrap_or(0.0); chosen_idx.len()];
    let power_plan = if levels.len() > 1 {
        // Coverage multiplicity across the selected set at full power.
        let mut cover_count: HashMap<u32, u32> = HashMap::new();
        for &ci in &chosen_idx {
            for &i in &per_power_all[ci][top_level] {
                *cover_count.entry(i).or_insert(0) += 1;
            }
        }
        let covered_before: f64 = cover_count.keys().map(|&i| weights[i as usize] as f64).sum();
        let mut sites_reduced = 0usize;
        for (slot, &ci) in chosen_idx.iter().enumerate() {
            let per_power = &per_power_all[ci];
            // Demand this site alone holds at full power.
            let unique: Vec<u32> = per_power[top_level]
                .iter()
                .copied()
                .filter(|i| cover_count.get(i).copied().unwrap_or(0) <= 1)
                .collect();
            for (li, &lvl) in levels.iter().enumerate() {
                let keeps_unique = {
                    let set: std::collections::HashSet<u32> =
                        per_power[li].iter().copied().collect();
                    unique.iter().all(|i| set.contains(i))
                };
                if keeps_unique {
                    if li < top_level {
                        sites_reduced += 1;
                        // Demand dropped by lowering power is no longer this
                        // site's; decrement so later sites see the truth.
                        let kept: std::collections::HashSet<u32> =
                            per_power[li].iter().copied().collect();
                        for i in &per_power[top_level] {
                            if !kept.contains(i) {
                                if let Some(c) = cover_count.get_mut(i) {
                                    *c = c.saturating_sub(1);
                                }
                            }
                        }
                    }
                    site_power[slot] = lvl;
                    break;
                }
            }
        }
        let covered_after: f64 = cover_count
            .iter()
            .filter(|(_, &c)| c > 0)
            .map(|(&i, _)| weights[i as usize] as f64)
            .sum();
        Some(PowerPlanStats {
            levels_dbm: levels.clone(),
            mean_power_before: levels.last().copied().unwrap_or(0.0),
            mean_power_after: site_power.iter().sum::<f64>() / site_power.len().max(1) as f64,
            mean_cs_neighbours_before: 0.0, // filled below
            mean_cs_neighbours_after: 0.0,
            covered_before,
            covered_after,
            sites_reduced,
        })
    } else {
        None
    };

    // Contention: count selected sites inside each other's carrier-sense
    // range, at the ceiling versus at the assigned plan. Distance-based
    // using each site's own budget, so it stays cheap for large N.
    let cs_range_m = |power_dbm: f64| -> f64 {
        // Invert a log-distance fit of the sweep's own budget: every 10·n dB
        // doubles-ish. n = 3.0 for mixed urban/suburban at 868 MHz.
        let budget = p.max_loss_db as f64 + p.cs_margin_db
            - (levels.last().copied().unwrap_or(power_dbm) - power_dbm);
        let ref_loss = p.max_loss_db as f64;
        p.radius_m * 10f64.powf((budget - ref_loss) / (10.0 * 3.0))
    };
    let positions: Vec<Xy> = chosen_idx.iter().map(|&ci| {
        let (r, c) = cand_px[ci];
        px_center(r, c)
    }).collect();
    let count_neighbours = |powers: &[f64]| -> Vec<usize> {
        positions
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let r = cs_range_m(powers[i]);
                positions
                    .iter()
                    .enumerate()
                    .filter(|(j, b)| *j != i && a.dist_m(b) <= r)
                    .count()
            })
            .collect()
    };
    let ceiling_powers = vec![levels.last().copied().unwrap_or(0.0); positions.len()];
    let cs_before = if levels.len() > 1 { count_neighbours(&ceiling_powers) } else { Vec::new() };
    let cs_after = count_neighbours(&site_power);
    let power_plan = power_plan.map(|mut s| {
        s.mean_cs_neighbours_before =
            cs_before.iter().sum::<usize>() as f64 / cs_before.len().max(1) as f64;
        s.mean_cs_neighbours_after =
            cs_after.iter().sum::<usize>() as f64 / cs_after.len().max(1) as f64;
        s
    });

    let gain_of: HashMap<usize, f64> =
        selection.picks.iter().map(|p| (p.candidate_id as usize, p.gain)).collect();
    let chosen = chosen_idx
        .iter()
        .enumerate()
        .map(|(slot, &ci)| {
            let (row, col) = cand_px[ci];
            ChosenSite {
                pos: px_center(row, col),
                ground_masl: surface.data[row * w + col],
                // Refined selections have no single greedy "gain"; report
                // the original where it exists, 0 for swapped-in sites.
                gain: gain_of.get(&ci).copied().unwrap_or(0.0),
                tx_power_dbm: site_power.get(slot).copied().unwrap_or(0.0),
                cs_neighbours: cs_after.get(slot).copied().unwrap_or(0),
            }
        })
        .collect();

    Ok(SitingResult {
        chosen,
        candidates_considered: candidates.len(),
        demand_points: demand_px.len(),
        covered_fraction,
        total_weight,
        covered_weight,
        population_weighted,
        cumulative_covered,
        baseline_covered,
        refined,
        power_plan,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two hills in opposite corners of a flat basin: selecting two sites
    /// should take both hills and beat any single site.
    #[test]
    fn two_hills_get_two_sites() {
        let n = 101;
        let res = 30.0;
        let mut data = vec![20.0f32; n * n];
        let hill = |data: &mut Vec<f32>, cr: usize, cc: usize| {
            for r in 0..n {
                for c in 0..n {
                    let d2 = ((r as f32 - cr as f32).powi(2) + (c as f32 - cc as f32).powi(2)).sqrt();
                    let bump = (60.0 - 3.0 * d2).max(0.0);
                    data[r * n + c] = data[r * n + c].max(20.0 + bump);
                }
            }
        };
        hill(&mut data, 25, 25);
        hill(&mut data, 75, 75);
        let terrain = Grid::with_axes(
            Xy { x: 0.0, y: 0.0 },
            res,
            -res,
            n,
            n,
            data,
        )
        .unwrap();

        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 868.0;
        link.tx_h_agl_m = 10.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        let mut p = SitingParams::defaults(link);
        p.select_n = 2;
        p.radius_m = 1200.0;
        p.max_loss_db = 140.0;
        p.demand_stride = 5;
        p.spacing_px = 8;
        p.max_candidates = 6;
        p.max_azimuths = Some(96);

        let out = optimize_sites(&terrain, None, None, &p).unwrap();
        assert_eq!(out.chosen.len(), 2);
        // The two picks sit on different hills (far apart).
        let a = &out.chosen[0].pos;
        let b = &out.chosen[1].pos;
        assert!(a.dist_m(b) > 1000.0, "picks too close: {:?} {:?}", (a.x, a.y), (b.x, b.y));
        assert!(out.chosen[0].ground_masl > 60.0, "should sit on a hill");
        assert!(out.covered_fraction > 0.1);
        // Second pick adds real marginal coverage.
        assert!(out.chosen[1].gain > 0.0);
    }

    /// With population concentrated near one hill, a single pick must go to
    /// the populated hill — even though both hills cover equal area.
    #[test]
    fn population_pulls_the_pick() {
        let n = 101;
        let res = 30.0;
        let mut data = vec![20.0f32; n * n];
        let hill = |data: &mut Vec<f32>, cr: usize, cc: usize| {
            for r in 0..n {
                for c in 0..n {
                    let d2 = ((r as f32 - cr as f32).powi(2) + (c as f32 - cc as f32).powi(2)).sqrt();
                    let bump = (60.0 - 3.0 * d2).max(0.0);
                    data[r * n + c] = data[r * n + c].max(20.0 + bump);
                }
            }
        };
        hill(&mut data, 25, 25);
        hill(&mut data, 75, 75);
        let terrain =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap();
        // People only around the second hill (rows/cols 60..90).
        let mut popd = vec![0.0f32; n * n];
        for r in 60..90 {
            for c in 60..90 {
                popd[r * n + c] = 12.0;
            }
        }
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, popd).unwrap();

        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 868.0;
        link.tx_h_agl_m = 10.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        let mut p = SitingParams::defaults(link);
        p.select_n = 1;
        p.radius_m = 1200.0;
        p.max_loss_db = 140.0;
        p.demand_stride = 5;
        p.spacing_px = 8;
        p.max_candidates = 6;
        p.max_azimuths = Some(96);

        let out = optimize_sites(&terrain, None, Some(&population), &p).unwrap();
        assert!(out.population_weighted);
        assert_eq!(out.chosen.len(), 1);
        // The populated hill sits near (75, 75) → world ≈ (2250, −2250).
        let s = &out.chosen[0].pos;
        assert!(s.x > 1500.0 && s.y < -1500.0, "picked ({}, {})", s.x, s.y);
        assert!(out.covered_weight > 0.0 && out.covered_weight <= out.total_weight);
    }

    /// Power reduction must preserve coverage exactly while lowering power
    /// on sites whose reach is redundant — the configuration that keeps a
    /// CSMA mesh out of one giant collision domain.
    #[test]
    fn power_reduction_keeps_coverage_and_lowers_contention() {
        let n = 120;
        let res = 30.0;
        // Flat ground so coverage is budget-limited, not terrain-limited,
        // with small rises to seed candidates.
        let mut data = vec![20.0f32; n * n];
        for c in (10usize..110).step_by(12) {
            for r in (10usize..110).step_by(12) {
                data[r * n + c] = 45.0;
            }
        }
        let terrain =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap();
        // Dense population everywhere: overlapping sites are redundant.
        let population = Grid::with_axes(
            Xy { x: 0.0, y: 0.0 },
            res,
            -res,
            n,
            n,
            vec![5.0f32; n * n],
        )
        .unwrap();

        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 868.0;
        link.tx_h_agl_m = 10.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        let mut p = SitingParams::defaults(link);
        p.select_n = 6;
        p.radius_m = 1500.0;
        p.max_loss_db = 150.0;
        p.demand_stride = 4;
        p.spacing_px = 8;
        p.max_candidates = 40;
        p.max_azimuths = Some(64);
        p.power_levels_dbm = vec![8.0, 14.0, 20.0, 27.0];

        let out = optimize_sites(&terrain, None, Some(&population), &p).unwrap();
        let plan = out.power_plan.expect("power plan produced");
        // Coverage is preserved by construction.
        assert!(
            (plan.covered_after - plan.covered_before).abs() < 1e-6,
            "coverage changed: {} → {}",
            plan.covered_before,
            plan.covered_after
        );
        // Some site should not need the ceiling.
        assert!(plan.sites_reduced > 0, "nothing reduced: {plan:?}");
        assert!(plan.mean_power_after <= plan.mean_power_before);
        // Contention must not increase.
        assert!(plan.mean_cs_neighbours_after <= plan.mean_cs_neighbours_before + 1e-9);
        // Every site carries an explicit assigned power.
        assert!(out.chosen.iter().all(|s| p.power_levels_dbm.contains(&s.tx_power_dbm)));
    }

    /// Regression for the region-scale degeneracy found on the build server:
    /// a tall EMPTY massif plus modest hills over a populated town. With a
    /// tight candidate cap, height-first seeding spends every slot on the
    /// empty massif; population-aware seeding must reach the town.
    #[test]
    fn candidate_seeding_prefers_reachable_population() {
        let n = 160;
        let res = 30.0;
        // Ridge of very tall empty peaks along the top edge.
        let mut data = vec![10.0f32; n * n];
        for c in (10usize..150).step_by(10) {
            for r in 5..15 {
                for cc in c.saturating_sub(3)..(c + 3).min(n) {
                    data[r * n + cc] = 400.0;
                }
            }
        }
        // Gentle rises over the populated south.
        for c in (20usize..140).step_by(20) {
            for r in 120..130 {
                for cc in c.saturating_sub(4)..(c + 4).min(n) {
                    data[r * n + cc] = 60.0;
                }
            }
        }
        let terrain =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap();
        let mut popd = vec![0.0f32; n * n];
        for r in 118..150 {
            for c in 20..140 {
                popd[r * n + c] = 25.0;
            }
        }
        let population =
            Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, popd).unwrap();

        let mut link = LinkParams::eu868_defaults();
        link.freq_mhz = 868.0;
        link.tx_h_agl_m = 10.0;
        link.rx_h_agl_m = 2.0;
        link.loc_pct = 50.0;
        let mut p = SitingParams::defaults(link);
        p.select_n = 1;
        p.radius_m = 900.0;
        p.max_loss_db = 145.0;
        p.demand_stride = 4;
        p.spacing_px = 6;
        p.max_candidates = 8; // tight: the whole point of good seeding
        p.max_azimuths = Some(64);

        let out = optimize_sites(&terrain, None, Some(&population), &p).unwrap();
        assert_eq!(out.chosen.len(), 1);
        // Must sit in the southern (populated) half, not on the empty ridge.
        let y = out.chosen[0].pos.y;
        assert!(y < -2000.0, "picked the empty ridge at y={y}");
        // And it must actually reach a meaningful share of the residents.
        assert!(
            out.covered_weight > 0.02 * out.total_weight,
            "covered {} of {}",
            out.covered_weight,
            out.total_weight
        );
    }
}
