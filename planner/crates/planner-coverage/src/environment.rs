//! Estimating what a deployed node's advert did NOT tell us: antenna height
//! and transmit power.
//!
//! WHY THIS EXISTS. The Berlin MeshCore import gave 269 live repeaters with
//! position and radio config, and with `height_agl_m: None` /
//! `tx_power_dbm: None` for essentially every one of them. Those two fields
//! dominate every answer the planner produces. Re-running the gap census over
//! the same 263 sites, changing ONLY the assumed antenna height (measured
//! 2026-08-30):
//!
//! ```text
//!   assumed h | residents reached | backbone edges | components | isolated
//!       8 m   | 3 090 194 (71.5%) |            117 |        176 |      141
//!      15 m   | 3 800 264 (87.9%) |          3 840 |          3 |        1
//!      25 m   | 4 075 249 (94.2%) |         10 342 |          1 |        0
//! ```
//!
//! An 88x change in backbone edge count, and the difference between "the
//! network is in 176 pieces" and "the network is one piece", produced by an
//! input nobody measured. A POINT ESTIMATE OF HEIGHT LAUNDERS THAT
//! UNCERTAINTY INTO A CONFIDENT WRONG ANSWER. So everything here returns an
//! INTERVAL plus the provenance of the number, and the batch entry point
//! returns a histogram of provenance so a report can say "94% of these sites
//! are guesses from a clutter raster" instead of printing one confident
//! percentage.
//!
//! Evidence is used best-first: an operator's own figure, then a LoD2 roof,
//! then the clutter raster over a neighbourhood, then a per-class convention,
//! then a documented fallback. Every tier reports the band it believes, and
//! the bands get wider as the evidence gets worse -- that widening is the
//! product, not a nuisance.
//!
//! Offline like the rest of the crate: the caller hands in slices of
//! already-loaded pack data. This module takes no new dependencies (in
//! particular not planner-pack or planner-buildings, which depend on this
//! crate's neighbours), so buildings arrive as plain `BuildingHint`s that the
//! caller fills from whatever it parsed.

use crate::gaps::SiteSpec;
use planner_core::geo::Xy;
use planner_core::preset::{eu_erp_ceiling_dbm, DevicePreset, RadioPreset};
use planner_core::profile::ClutterClass;
use planner_terrain::Grid;
use std::fmt;

// ---------------------------------------------------------------------------
// Constants. Every one of these is a judgement call, so every one says where
// the number came from and which failure it is protecting against.
// ---------------------------------------------------------------------------

/// How far a MeshCore advert's position can be from the actual antenna, in
/// metres, from quantisation alone.
///
/// The importer measured the Berlin snapshot: ordinary adverts carry
/// four-decimal degrees (the two-decimal ones are rejected outright as a
/// ~1.1 km grid -- see `planner_import::meshcore_map::position_quality`). At
/// Berlin's latitude four decimals is a cell of ~11.1 m north-south by ~6.8 m
/// east-west, so a fix can be displaced by up to half its diagonal, ~6.5 m,
/// and two adjacent true positions can be reported 11.1 m apart. 11.1 m is
/// the conservative reading of that: the distance within which two candidate
/// buildings are indistinguishable given the input.
pub const ADVERT_POSITION_UNCERTAINTY_M: f64 = 11.1;

/// Radius searched for a LoD2 building under a node (metres).
///
/// It must exceed the advert quantisation above, and it must also cover the
/// offset between a building's CENTROID (which is what `BuildingHint` carries)
/// and the corner of the roof the antenna is actually on. A Berlin
/// perimeter-block wing of ~600 m2 has a half-span of ~12 m. 11.1 + 12 ~= 23,
/// rounded to 25.
///
/// Bigger would be worse, not more forgiving: at 40 m a node in a Gruenderzeit
/// block starts attaching to the building across the street, which in the same
/// block can differ by two storeys.
pub const BUILDING_SEARCH_RADIUS_M: f64 = 25.0;

/// Smallest footprint that can plausibly host a community repeater (m2).
///
/// LoD2 contains every garden shed, garage, kiosk and transformer hut. The
/// pack sample has a 9.95 m2 record 27.7 m tall (a stairwell head or chimney
/// broken out of a larger block); attaching a node to that would report a
/// 30 m mast in a back yard. 40 m2 is about 6.3 x 6.3 m -- the smallest thing
/// that is plausibly a house rather than an outbuilding. Nodes with only
/// sub-threshold buildings nearby fall through to the clutter raster, which is
/// the honest answer: a cluster of sheds is not roof evidence.
pub const MIN_REPEATER_FOOTPRINT_M2: f32 = 40.0;

/// Typical mast on top of a roof (metres). Community installs are a pole in a
/// chimney bracket or a short tripod; galvanised mast tube is sold in 1.0,
/// 1.5 and 2.0 m lengths and a single length is what a volunteer carries up a
/// ladder. CONVENTION, not a measurement.
pub const ROOF_MAST_TYPICAL_M: f64 = 2.0;

/// Largest mast assumed for a rooftop install (metres). Above ~4 m a
/// non-professional install needs guying and a structural conversation with
/// the building owner, which is no longer the household case this planner is
/// for. CONVENTION, not a measurement.
pub const ROOF_MAST_MAX_M: f64 = 4.0;

/// Radius of the clutter neighbourhood sampled around a node (metres).
///
/// Sized at ONE BUILDING'S SCALE, deliberately between two failures. Reading
/// the single cell under the node underestimates a rooftop install, because
/// the clutter layer is a cell MEAN: a 10 m cell straddling a roof and a
/// courtyard reports half the roof height. Taking a high order statistic over
/// a district-sized radius does the opposite -- it names the tallest tower in
/// the district and turns a household planner into a broadcast-tower planner
/// (the same failure `SitingParams::max_tx_agl_m` exists for).
///
/// 30 m spans a Berlin perimeter-block wing (~12-15 m deep, 20-30 m long) plus
/// its immediate neighbours, and stops short of the far side of the street. On
/// the 10 m Berlin pack it yields 25 samples for a node on a cell centre and
/// 32 for one on a cell corner -- enough for a percentile to mean something.
pub const CLUTTER_NEIGHBOURHOOD_RADIUS_M: f64 = 30.0;

/// Percentile of the clutter neighbourhood taken as the local built-form top.
///
/// The antenna sits on top of the local built form, not at its average. In a
/// block that is 40-60% built (Berlin's typical), everything above the
/// (1 - built_fraction) quantile is a genuinely built cell, so the 80th
/// percentile lands on roof rather than on courtyard for any built fraction
/// above 0.2. The maximum would be wrong for the opposite reason: over the
/// 25-32 samples the radius above yields, the max IS the single tallest object
/// nearby, so one church spire or one Plattenbau end-wall sets the answer for a
/// whole block. At p80 over that many samples the value is the fifth- or
/// sixth-highest cell, which no single outlier can move.
pub const CLUTTER_PERCENTILE: f64 = 0.80;

/// Minimum half-width of the clutter-derived band (metres).
///
/// A perfectly uniform neighbourhood measures zero spread, which is not the
/// same as zero uncertainty -- we still do not know whether the node is on the
/// roof, on a balcony, or in a window. One storey (3 m, the same
/// metres-per-level convention `planner_buildings::M_PER_LEVEL` uses) is the
/// floor, so a uniform block reports a band rather than false precision.
pub const CLUTTER_MIN_HALF_BAND_M: f64 = 3.0;

/// Fewest cells a clutter neighbourhood needs before its percentile is worth
/// believing. Below this the radius is grown once (up to 2x, reported in the
/// basis) -- on a 30 m pack the nominal 30 m radius only reaches ~5 cells, and
/// a p80 of five samples is the second-highest of five, i.e. nearly the max,
/// which is exactly the failure the percentile was chosen to avoid.
pub const MIN_NEIGHBOURHOOD_CELLS: usize = 9;

/// Floor on any ESTIMATED antenna height (metres). Below ~2 m there is no
/// installed radio: even a handheld is carried at 1.5 m, and a node bolted to
/// a wall or standing on a balcony rail sits higher. Applies to the estimated
/// tiers only; an operator who reports 1.0 m is believed.
pub const MIN_ANTENNA_AGL_M: f64 = 2.0;

/// Height assumed for a node standing on unbuilt ground (metres). A clutter
/// neighbourhood of all zeros means open ground, and a node on open ground is
/// still on a pole or a fence post, not lying in the grass.
pub const OPEN_GROUND_MAST_M: f64 = 3.0;

/// Default ceiling on the PLANNING value (metres).
///
/// Same number and same rationale as `SitingParams::max_tx_agl_m`: inferred
/// heights come from cell means and order statistics that can name a landmark,
/// and one such site in an earlier run claimed to reach 3.7 M residents.
/// Community deployments mount on ordinary roofs -- Berlin's Traufhoehe is
/// ~22 m -- so the inferred value is capped. Raise it deliberately when
/// planning a real mast.
pub const DEFAULT_MAX_AGL_M: f64 = 30.0;

/// Planning value used when there is no evidence at all (metres).
///
/// Deliberately the PESSIMISTIC end of the census sweep quoted at the top of
/// this file. Assuming 15 m turns an unknown into "3 components, basically
/// fine"; assuming 8 m says "on this assumption the network is in pieces, go
/// and measure something". A planner that has no evidence must not manufacture
/// a working network, so the point estimate takes the low road and `low_m` /
/// `high_m` carry the rest.
pub const NO_EVIDENCE_H_M: f64 = 8.0;
/// Lower bound with no evidence: the `MIN_ANTENNA_AGL_M` floor.
pub const NO_EVIDENCE_LOW_M: f64 = MIN_ANTENNA_AGL_M;
/// Upper bound with no evidence: the top of the census sweep (25 m), i.e. a
/// tall rooftop install. The band therefore spans exactly the 71.5% -> 94.2%
/// range of outcomes the sweep measured.
pub const NO_EVIDENCE_HIGH_M: f64 = 25.0;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One building near a node, in pack-CRS metres.
///
/// Deliberately a plain struct rather than `planner_buildings::BuildingRec`:
/// this crate must not depend on planner-buildings, and callers feed it from
/// `buildings.jsonl`, from an OSM extract, or from a hand-typed list. Only the
/// three fields that decide the answer are carried.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuildingHint {
    /// Centroid, pack CRS.
    pub xy: Xy,
    pub footprint_m2: f32,
    /// Height above GROUND, metres (LoD2 measures to the roof).
    pub height_m: f32,
}

/// Everything the pack can say about the ground under a node.
///
/// All three are optional or possibly empty on purpose. The local
/// `packs/berlin-10m-ui` has exactly 1402 buildings (one cached LoD2 tile)
/// against the build server's full-Berlin pack of hundreds of thousands, so
/// building evidence must be treated as SPARSE and never assumed present --
/// code that only works when `buildings` is populated would pass every local
/// test and then quietly change tier on the real pack.
pub struct SiteEvidence<'a> {
    /// Clutter height above ground (buildings + canopy), metres. CELL MEANS.
    pub clutter: Option<&'a Grid>,
    /// `ClutterClass::code()` values stored as pixels.
    pub classes: Option<&'a Grid>,
    /// Sparse and optional. Callers pass what the pack has.
    pub buildings: &'a [BuildingHint],
}

impl<'a> SiteEvidence<'a> {
    /// Evidence-free, for callers that have nothing but positions.
    pub fn none() -> SiteEvidence<'static> {
        SiteEvidence { clutter: None, classes: None, buildings: &[] }
    }
}

/// Where a height estimate came from. The whole point of the module: the
/// number is never reported without this.
#[derive(Debug, Clone, PartialEq)]
pub enum HeightBasis {
    /// The operator told us. Believed outright.
    OperatorSupplied,
    /// A LoD2 roof was found under the node.
    Lod2Building { footprint_m2: f32, building_h_m: f32, distance_m: f64 },
    /// A percentile of the clutter raster over a neighbourhood.
    ClutterNeighbourhood { radius_m: f64, percentile: f64, sampled_cells: usize },
    /// A per-class convention. A GUESS, and labelled as one.
    ClassTypical(ClutterClass),
    /// Nothing at all. Counted separately so a report can say how much of the
    /// map is fabricated.
    NoEvidence,
}

/// Coarse provenance tier, for histograms. Mirrors the tiers of
/// `planner_buildings::HeightSource` in spirit (Lod2 > modelled > class
/// default) without depending on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BasisKind {
    OperatorSupplied,
    Lod2Building,
    ClutterNeighbourhood,
    ClassTypical,
    NoEvidence,
}

impl BasisKind {
    pub const ALL: [BasisKind; 5] = [
        BasisKind::OperatorSupplied,
        BasisKind::Lod2Building,
        BasisKind::ClutterNeighbourhood,
        BasisKind::ClassTypical,
        BasisKind::NoEvidence,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BasisKind::OperatorSupplied => "operator-supplied",
            BasisKind::Lod2Building => "lod2-building",
            BasisKind::ClutterNeighbourhood => "clutter-neighbourhood",
            BasisKind::ClassTypical => "class-typical",
            BasisKind::NoEvidence => "no-evidence",
        }
    }

    fn index(self) -> usize {
        match self {
            BasisKind::OperatorSupplied => 0,
            BasisKind::Lod2Building => 1,
            BasisKind::ClutterNeighbourhood => 2,
            BasisKind::ClassTypical => 3,
            BasisKind::NoEvidence => 4,
        }
    }
}

impl HeightBasis {
    pub fn kind(&self) -> BasisKind {
        match self {
            HeightBasis::OperatorSupplied => BasisKind::OperatorSupplied,
            HeightBasis::Lod2Building { .. } => BasisKind::Lod2Building,
            HeightBasis::ClutterNeighbourhood { .. } => BasisKind::ClutterNeighbourhood,
            HeightBasis::ClassTypical(_) => BasisKind::ClassTypical,
            HeightBasis::NoEvidence => BasisKind::NoEvidence,
        }
    }

    /// True when the number is a convention rather than an observation. Two
    /// tiers deserve a warning in any report that prints coverage: they are
    /// where the 71.5%-vs-94.2% spread lives.
    pub fn is_guess(&self) -> bool {
        matches!(self, HeightBasis::ClassTypical(_) | HeightBasis::NoEvidence)
    }
}

/// A height to plan with, the band it plausibly lies in, and where it came
/// from.
///
/// `low_m <= h_agl_m <= high_m` always holds. Callers that want an honest
/// answer re-run the analysis at `low_m` and `high_m` and report the spread;
/// callers that want one map use `h_agl_m` and print the provenance histogram
/// next to it.
#[derive(Debug, Clone, PartialEq)]
pub struct HeightEstimate {
    /// The value to plan with (metres AGL), after the ceiling clamp.
    pub h_agl_m: f64,
    /// Plausible lower bound (metres AGL).
    pub low_m: f64,
    /// Plausible upper bound (metres AGL). NOT clamped by the ceiling -- see
    /// `clamped_from_m`.
    pub high_m: f64,
    pub basis: HeightBasis,
    /// Set when the ceiling clamp moved the planning value: the height that
    /// would have been planned with had there been no ceiling.
    ///
    /// The clamp is a policy ("this is a household planner"), not a
    /// measurement, so it must never disappear into the number. `high_m` is
    /// left un-clamped for the same reason: if the evidence says 47 m, the
    /// report should be able to say "capped at 30, evidence went to 47" rather
    /// than silently presenting a 30 m band.
    pub clamped_from_m: Option<f64>,
}

impl HeightEstimate {
    /// Width of the plausible band (metres). Zero only for operator-supplied.
    pub fn band_m(&self) -> f64 {
        self.high_m - self.low_m
    }
}

/// Knobs for the estimator. Defaults are the constants above.
#[derive(Debug, Clone, PartialEq)]
pub struct EstimateParams {
    /// Ceiling on the PLANNING value (metres). See `DEFAULT_MAX_AGL_M`.
    pub max_agl_m: f64,
    /// Search radius for LoD2 roofs (metres).
    pub building_search_radius_m: f64,
    /// Radius of the clutter neighbourhood (metres).
    pub clutter_radius_m: f64,
    /// Percentile of that neighbourhood taken as the built-form top, 0..1.
    pub clutter_percentile: f64,
    /// TX power written into `SiteSpec` for nodes that do not report one.
    /// Get it from `allowed_max_tx_dbm`; None leaves the plan's reference
    /// power in charge (see `GapParams::ref_tx_power_dbm`).
    pub default_tx_power_dbm: Option<f64>,
}

impl Default for EstimateParams {
    fn default() -> Self {
        Self {
            max_agl_m: DEFAULT_MAX_AGL_M,
            building_search_radius_m: BUILDING_SEARCH_RADIUS_M,
            clutter_radius_m: CLUTTER_NEIGHBOURHOOD_RADIUS_M,
            clutter_percentile: CLUTTER_PERCENTILE,
            default_tx_power_dbm: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-class conventions
// ---------------------------------------------------------------------------

/// Typical / low / high antenna height for a node whose only evidence is its
/// clutter class (metres AGL).
///
/// THESE ARE CONVENTIONS, NOT MEASUREMENTS. They are built from two published
/// conventions already in the tree -- `planner_buildings::M_PER_LEVEL` (3.0 m
/// per storey, GeoClimate/Bocher 2022 range 2.8-3.5) and
/// `planner_buildings::PITCHED_ROOF_M` (2.0 m) -- plus Berlin's Traufhoehe
/// (eaves-height) tradition of ~22 m for Gruenderzeit blocks, and then a short
/// mast on top. Every band is wide because a class is a category, not an
/// observation.
pub fn class_typical_agl_m(class: ClutterClass) -> (f64, f64, f64) {
    match class {
        // A pole, a fence post, a farm building. Nothing to stand on.
        ClutterClass::Open => (3.0, 2.0, 10.0),
        // A node whose cell is water is almost certainly mis-placed: the
        // advert quantisation put it in the Spree or on a lake. It is really
        // on a bank, a bridge or a boat, so the band is as wide as Open's and
        // the low end stays at the floor.
        ClutterClass::Water => (3.0, 2.0, 10.0),
        ClutterClass::LowVegetation => (3.0, 2.0, 10.0),
        // The widest band of the lot, and honestly so: a node in forest is
        // either under the canopy on a 2 m pole or on a mast that clears it.
        // Those two cases differ by 20 dB of path loss and the class cannot
        // tell them apart.
        ClutterClass::Forest => (8.0, 2.0, 25.0),
        // Two storeys plus a pitched roof: 2*3 + 2 = 8, plus a short mast.
        ClutterClass::Suburban => (9.0, 4.0, 15.0),
        // Four to five storeys plus roof: 5*3 + 2 = 17, plus a short mast.
        ClutterClass::Urban => (18.0, 8.0, 25.0),
        // Berlin's Traufhoehe cap, ~22 m, is what "dense urban" means here;
        // roof plus a short mast lands near the 30 m ceiling.
        ClutterClass::DenseUrban => (22.0, 10.0, 30.0),
        // Big single-storey halls have 8-12 m eaves, but the class also holds
        // stacks, silos and gantries. Typical is the hall; the high end admits
        // the rest.
        ClutterClass::Industrial => (10.0, 4.0, 30.0),
    }
}

// ---------------------------------------------------------------------------
// Raster helpers
// ---------------------------------------------------------------------------

/// Nearest-neighbour sample, matching `Grid::sample_bilinear`'s half-pixel-out
/// domain.
///
/// Class rasters MUST be sampled this way. Bilinear interpolation of
/// `ClutterClass::code()` values is arithmetic on labels: an Open cell (0) next
/// to a DenseUrban one (6) averages to 3, which decodes as Forest. That is not
/// a rounding error, it is a different land cover.
fn sample_nearest(g: &Grid, p: Xy) -> Option<f32> {
    let fx = (p.x - g.origin.x) / g.dx_m;
    let fy = (p.y - g.origin.y) / g.dy_m;
    let eps = 1e-9;
    if !(fx.is_finite() && fy.is_finite())
        || fx < -0.5 - eps
        || fy < -0.5 - eps
        || fx > g.width as f64 - 0.5 + eps
        || fy > g.height as f64 - 0.5 + eps
    {
        return None;
    }
    let col = (fx.round().clamp(0.0, (g.width - 1) as f64)) as usize;
    let row = (fy.round().clamp(0.0, (g.height - 1) as f64)) as usize;
    Some(g.data[row * g.width + col])
}

/// Collect finite clutter heights whose cell CENTRES lie within `radius_m` of
/// `p`. Returns them sorted ascending.
///
/// Negative samples are clamped to zero, the same treatment the sweep gives
/// them in `coverage()`: a negative clutter height is a nodata convention or a
/// DSM-minus-DTM artefact, never a hole in the ground the antenna sits in.
fn neighbourhood_sorted(g: &Grid, p: Xy, radius_m: f64) -> Vec<f32> {
    let (dx, dy) = (g.dx_m.abs(), g.dy_m.abs());
    if dx <= 0.0 || dy <= 0.0 || !radius_m.is_finite() || radius_m <= 0.0 {
        return Vec::new();
    }
    let fx = (p.x - g.origin.x) / g.dx_m;
    let fy = (p.y - g.origin.y) / g.dy_m;
    if !(fx.is_finite() && fy.is_finite()) {
        return Vec::new();
    }
    let c0 = fx.round() as i64;
    let r0 = fy.round() as i64;
    let span_c = (radius_m / dx).ceil() as i64;
    let span_r = (radius_m / dy).ceil() as i64;
    // Clip to the raster BEFORE sizing and iterating, not inside the loop. The
    // radius is a caller-supplied `EstimateParams` field: an accidental
    // 100 km on a 10 m pack makes the unclipped span 10^7 cells per axis,
    // which is a 10^14-element `with_capacity` (an abort) and a loop that
    // never returns, for a grid that might be 41x41. Clipping first makes the
    // work proportional to the raster, which is the only real bound there is.
    // Saturating, because `(radius_m / dx).ceil() as i64` saturates at
    // i64::MAX for an absurd radius and `c0 - span_c` would then overflow.
    let r_lo = r0.saturating_sub(span_r).clamp(0, g.height as i64);
    let r_hi = r0.saturating_add(span_r).clamp(-1, g.height as i64 - 1);
    let c_lo = c0.saturating_sub(span_c).clamp(0, g.width as i64);
    let c_hi = c0.saturating_add(span_c).clamp(-1, g.width as i64 - 1);
    if r_hi < r_lo || c_hi < c_lo {
        return Vec::new();
    }
    let mut out =
        Vec::with_capacity((((r_hi - r_lo + 1) * (c_hi - c_lo + 1)) as usize).min(1 << 20));
    for row in r_lo..=r_hi {
        let cy = g.origin.y + row as f64 * g.dy_m;
        for col in c_lo..=c_hi {
            let cx = g.origin.x + col as f64 * g.dx_m;
            if (cx - p.x).hypot(cy - p.y) > radius_m {
                continue;
            }
            let v = g.data[row as usize * g.width + col as usize];
            if v.is_finite() {
                out.push(v.max(0.0));
            }
        }
    }
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Linear-interpolated quantile of an ascending slice (the "type 7" definition
/// used by numpy and R's default). Interpolating rather than nearest-rank
/// matters at the sample counts here: with 29 cells, nearest-rank p80 is a
/// single cell's value and moves in jumps as the radius changes.
fn quantile(sorted: &[f32], q: f64) -> f64 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0] as f64,
        n => {
            let pos = q.clamp(0.0, 1.0) * (n - 1) as f64;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(n - 1);
            let t = pos - lo as f64;
            sorted[lo] as f64 * (1.0 - t) + sorted[hi] as f64 * t
        }
    }
}

// ---------------------------------------------------------------------------
// The estimator
// ---------------------------------------------------------------------------

/// Estimate a node's antenna height and the band it plausibly lies in.
///
/// `operator_h_agl_m` is whatever the pack recorded (`DeployedNode::
/// height_agl_m`). Evidence tiers are tried best-first and the first one that
/// fires wins.
pub fn estimate_height(
    xy: Xy,
    operator_h_agl_m: Option<f64>,
    ev: &SiteEvidence<'_>,
    p: &EstimateParams,
) -> HeightEstimate {
    // TIER 1 -- the operator told us.
    //
    // Wins outright, with zero spread, and is NOT subject to the ceiling
    // clamp. The clamp exists to stop an INFERRED cell-mean from naming a
    // landmark tower; an operator who reports a 45 m mast has a 45 m mast, and
    // capping it to 30 would be the planner overruling the only real
    // measurement in the data set.
    //
    // A NEGATIVE or non-finite figure is not a report, it is corrupt input,
    // and believing it would be the usual disaster in reverse: `gaps::analyse`
    // silently repairs what it is given (`h_agl_m.clamp(1.0, 3000.0)`), so a
    // -9999 sentinel that leaked out of an importer would arrive at the model
    // as a plausible 1.0 m antenna carrying an "operator-supplied" label and
    // a zero-width band. Falling through to inference keeps it out of the
    // measured tally and puts it somewhere the histogram can show it. 0.0 IS
    // believed: a node lying on the ground is a real (bad) deployment, and
    // this repo has been bitten before by treating a legitimate zero as absent.
    if let Some(h) = operator_h_agl_m.filter(|v| v.is_finite() && *v >= 0.0) {
        return HeightEstimate {
            h_agl_m: h,
            low_m: h,
            high_m: h,
            basis: HeightBasis::OperatorSupplied,
            clamped_from_m: None,
        };
    }

    // TIER 2 -- a LoD2 roof under the node.
    if let Some(e) = estimate_from_buildings(xy, ev.buildings, p.building_search_radius_m) {
        return clamp_to_ceiling(e, p.max_agl_m);
    }

    // TIER 3 -- the clutter raster over a neighbourhood.
    if let Some(g) = ev.clutter {
        if let Some(e) = estimate_from_clutter(xy, g, p.clutter_radius_m, p.clutter_percentile) {
            return clamp_to_ceiling(e, p.max_agl_m);
        }
    }

    // TIER 4 -- a per-class convention.
    if let Some(g) = ev.classes {
        if let Some(code) = sample_nearest(g, xy) {
            // Class codes are stored as raster pixels, so a fractional or
            // out-of-range value is corrupt data rather than a class we should
            // guess at; fall through to NoEvidence so it is COUNTED instead of
            // being silently rounded into Open.
            if code.is_finite() && code >= 0.0 && code < 256.0 && code.fract() == 0.0 {
                if let Some(class) = ClutterClass::from_code(code as u8) {
                    let (typ, lo, hi) = class_typical_agl_m(class);
                    return clamp_to_ceiling(
                        HeightEstimate {
                            h_agl_m: typ,
                            low_m: lo.max(MIN_ANTENNA_AGL_M),
                            high_m: hi,
                            basis: HeightBasis::ClassTypical(class),
                            clamped_from_m: None,
                        },
                        p.max_agl_m,
                    );
                }
            }
        }
    }

    // TIER 5 -- nothing. Documented fallback, counted separately.
    clamp_to_ceiling(
        HeightEstimate {
            h_agl_m: NO_EVIDENCE_H_M,
            low_m: NO_EVIDENCE_LOW_M,
            high_m: NO_EVIDENCE_HIGH_M,
            basis: HeightBasis::NoEvidence,
            clamped_from_m: None,
        },
        p.max_agl_m,
    )
}

/// Pick the building a repeater would actually be on, if there is one.
fn estimate_from_buildings(
    xy: Xy,
    buildings: &[BuildingHint],
    radius_m: f64,
) -> Option<HeightEstimate> {
    // Candidates: inside the search radius, big enough to be a building rather
    // than an outbuilding, and with a usable height.
    let mut cands: Vec<(f64, &BuildingHint)> = buildings
        .iter()
        .filter(|b| {
            b.footprint_m2 >= MIN_REPEATER_FOOTPRINT_M2
                && b.height_m.is_finite()
                && b.height_m > 0.0
                && b.xy.x.is_finite()
                && b.xy.y.is_finite()
        })
        .map(|b| (xy.dist_m(&b.xy), b))
        .filter(|(d, _)| *d <= radius_m)
        .collect();
    if cands.is_empty() {
        return None;
    }

    // Among candidates, prefer the LARGER FOOTPRINT over a marginally closer
    // tiny one -- a 45 m2 back-yard annexe two metres nearer than the 900 m2
    // block is not where a repeater lives. "Marginally" is defined by the data,
    // not by taste: the advert position is only good to
    // ADVERT_POSITION_UNCERTAINTY_M, so buildings within that of the nearest
    // candidate are INDISTINGUISHABLE in distance and are ranked by footprint
    // instead. A building genuinely further away than that still loses, so a
    // huge block at the far edge of the radius cannot steal a node from the
    // house it is standing on.
    let d_min = cands.iter().map(|(d, _)| *d).fold(f64::INFINITY, f64::min);
    let window = d_min + ADVERT_POSITION_UNCERTAINTY_M;
    cands.retain(|(d, _)| *d <= window);
    // Largest footprint first; ties broken by proximity, then by height so the
    // pick is deterministic regardless of input order.
    cands.sort_by(|a, b| {
        b.1.footprint_m2
            .partial_cmp(&a.1.footprint_m2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .then(b.1.height_m.partial_cmp(&a.1.height_m).unwrap_or(std::cmp::Ordering::Equal))
    });
    let (distance_m, b) = cands[0];

    // The antenna is on the ROOF. German LoD2 measures `height_m` to the roof,
    // so mounting AT that height (antenna clamped at the eaves, on a balcony,
    // or against a gable) is already the pessimistic end of the band; the
    // optimistic end is the roof plus a full mast.
    let base = b.height_m as f64;
    Some(HeightEstimate {
        h_agl_m: (base + ROOF_MAST_TYPICAL_M).max(MIN_ANTENNA_AGL_M),
        low_m: base.max(MIN_ANTENNA_AGL_M),
        high_m: base + ROOF_MAST_MAX_M,
        basis: HeightBasis::Lod2Building {
            footprint_m2: b.footprint_m2,
            building_h_m: b.height_m,
            distance_m,
        },
        clamped_from_m: None,
    })
}

/// Percentile of the clutter neighbourhood, plus a band from the spread that
/// was actually measured there.
fn estimate_from_clutter(
    xy: Xy,
    clutter: &Grid,
    radius_m: f64,
    percentile: f64,
) -> Option<HeightEstimate> {
    // A percentile outside 0..=1, or NaN, is not evidence and must not be
    // quietly repaired into one. `quantile` clamps its argument, so p = 1.5
    // would silently become the MAXIMUM -- the single-tallest-object answer
    // this whole tier exists to avoid -- while the basis went on reporting
    // 1.5. NaN is worse, and in exactly the way this crate already documents
    // for the sweep: `p_top` comes out NaN, and `NaN.max(OPEN_GROUND_MAST_M)`
    // is 3.0 in Rust, so a uniform 20 m block was measured returning
    // h_agl_m = 3.0 -- "a pole on open ground" -- labelled
    // ClutterNeighbourhood with a narrow band. Refusing here drops the site to
    // a tier that is COUNTED instead.
    if !percentile.is_finite() || !(0.0..=1.0).contains(&percentile) {
        return None;
    }
    // The node must be inside the raster at all; otherwise the neighbourhood
    // is a handful of edge cells from somewhere else entirely.
    sample_nearest(clutter, xy)?;

    let mut used_radius = radius_m;
    let mut cells = neighbourhood_sorted(clutter, xy, used_radius);
    // Grow ONCE if the pack is coarse enough that the nominal radius reaches
    // too few cells (see MIN_NEIGHBOURHOOD_CELLS). The radius that was really
    // used is reported in the basis, so a coarse pack is visible rather than
    // being papered over.
    if cells.len() < MIN_NEIGHBOURHOOD_CELLS {
        used_radius = radius_m * 2.0;
        cells = neighbourhood_sorted(clutter, xy, used_radius);
    }
    if cells.is_empty() {
        return None;
    }

    let p_top = quantile(&cells, percentile);
    // Belt and braces against the same NaN swallow: everything below funnels
    // through `f64::max`, which RETURNS THE OTHER OPERAND for NaN, so a
    // non-finite percentile value would turn into a confident 3.0 m rather
    // than into a visible failure.
    if !p_top.is_finite() {
        return None;
    }
    // Band anchored on the spread MEASURED in this neighbourhood: p25 below
    // (the courtyards and streets the node might actually be standing in) and
    // p95 above (the tallest roof it might be on). A heterogeneous block
    // therefore reports a wide band and a uniform one a narrow band, which is
    // the honest signal -- see CLUTTER_MIN_HALF_BAND_M for why "narrow" is not
    // allowed to become "zero".
    let p_lo = quantile(&cells, 0.25);
    let p_hi = quantile(&cells, 0.95);

    let est = (p_top + ROOF_MAST_TYPICAL_M).max(OPEN_GROUND_MAST_M);
    let low = p_lo.min(est - CLUTTER_MIN_HALF_BAND_M).max(MIN_ANTENNA_AGL_M);
    let high = (p_hi + ROOF_MAST_MAX_M).max(est + CLUTTER_MIN_HALF_BAND_M);

    Some(HeightEstimate {
        h_agl_m: est,
        low_m: low.min(est),
        high_m: high,
        basis: HeightBasis::ClutterNeighbourhood {
            radius_m: used_radius,
            percentile,
            sampled_cells: cells.len(),
        },
        clamped_from_m: None,
    })
}

/// Apply the household-planner ceiling to the PLANNING value only.
///
/// What the clamp protects against: an inferred height that names a landmark.
/// Cell-mean clutter over a block containing a church spire, or a LoD2 record
/// that is really a chimney, produces 40-100 m, and planning at that height
/// turns one community node into a broadcast transmitter -- one such site
/// previously claimed to reach 3.7 M residents.
///
/// What it must NOT do is hide that it happened. `high_m` is left un-clamped,
/// and `clamped_from_m` records the value that would have been planned with,
/// so a report can print "capped at 30 m; the evidence said 47 m" and a caller
/// can count how many sites the policy is deciding for.
fn clamp_to_ceiling(mut e: HeightEstimate, ceiling_m: f64) -> HeightEstimate {
    if !ceiling_m.is_finite() || e.h_agl_m <= ceiling_m {
        return e;
    }
    e.clamped_from_m = Some(e.h_agl_m);
    e.h_agl_m = ceiling_m;
    // Only when the WHOLE band sits above the ceiling (the landmark case) does
    // low_m have to move, and `clamped_from_m` is already set to say so.
    e.low_m = e.low_m.min(e.h_agl_m);
    e
}

// ---------------------------------------------------------------------------
// TX power
// ---------------------------------------------------------------------------

/// Gain of a lossless half-wave dipole over an isotropic radiator (dB).
///
/// ERP (what ERC 70-03 / EN 300 220 write the SRD limits in, and what
/// `eu_erp_ceiling_dbm` returns) is referenced to a HALF-WAVE DIPOLE; EIRP and
/// `DevicePreset::antenna_gain_dbi` are referenced to an isotropic radiator.
/// The two differ by this constant, which is 10*log10(1.64) for the dipole's
/// directivity.
///
/// Getting it wrong is a silent 2.15 dB, and this function got it wrong first:
/// subtracting dBi straight off an ERP ceiling is the obvious reading and told
/// a 9 dBi colinear it had to run at 18.0 dBm conducted when 20.15 dBm is
/// still inside the 500 mW ERP limit. 2 dB of link budget is roughly a 25%
/// error in area on a flat-earth path, and it lands on exactly the high-gain
/// case the regulation is supposed to bind.
pub const DBI_TO_DBD_DB: f64 = 2.15;

/// The transmit power to assume when the node did not record one (dBm
/// conducted).
///
/// Operators generally run at the maximum the rules allow -- nobody turns a
/// repeater down without a reason -- so the ceiling is the prior. It is the
/// smaller of what the hardware can emit and what the band permits once the
/// antenna's gain is subtracted, because the regulation is written in ERP:
///
///   ERP = conducted + gain_dBd = conducted + gain_dBi - DBI_TO_DBD_DB,
///   so conducted_max = ERP_ceiling - gain_dBi + DBI_TO_DBD_DB.
///
/// BERLIN CASE, worked through: the community runs 869.618 MHz, which is
/// inside the 869.4-869.65 MHz slot, so `eu_erp_ceiling_dbm` gives 27.0 dBm
/// ERP (500 mW at 10% duty). A generic SX1262 node is 22.0 dBm conducted with
/// a 2.0 dBi stock whip, i.e. 21.85 dBm ERP -- about 5 dB under the ceiling.
/// So
///   min(22.0, 27.0 - 2.0 + 2.15 = 27.15) = 22.0 dBm.
/// THE HARDWARE IS THE BINDING LIMIT, NOT THE REGULATION. That matters for
/// how the planner reasons about the gap: a Berlin node cannot buy reach by
/// turning up, only by adding antenna gain, and gain is what pulls the ERP
/// ceiling into play (a 9 dBi colinear makes it min(22, 20.15) = 20.15 dBm
/// conducted, and now the regulation binds).
///
/// The band lookup is a planning default explicitly marked UNVERIFIED against
/// the current national frequency plan in `planner_core::preset` -- it is not
/// legal advice, and a deployment must check it.
pub fn allowed_max_tx_dbm(radio: &RadioPreset, device: &DevicePreset) -> f64 {
    let erp_ceiling_dbm = eu_erp_ceiling_dbm(radio.freq_mhz);
    device
        .tx_power_dbm
        .min(erp_ceiling_dbm - device.antenna_gain_dbi + DBI_TO_DBD_DB)
}

// ---------------------------------------------------------------------------
// Batch entry point
// ---------------------------------------------------------------------------

/// One node as the caller has it, before heights are filled in.
///
/// Mirrors the fields of `planner_pack::nodes::DeployedNode` that matter here
/// without depending on planner-pack.
#[derive(Debug, Clone)]
pub struct NodeInput {
    pub xy: Xy,
    pub label: String,
    /// Operator-supplied height, if the source had one. Wins outright.
    pub height_agl_m: Option<f64>,
    /// Operator-supplied power, if the source had one. Wins outright over
    /// `EstimateParams::default_tx_power_dbm`.
    pub tx_power_dbm: Option<f64>,
}

/// How many sites landed on each provenance tier, and how wide their bands
/// were.
///
/// This is the number a report must print NEXT TO any coverage figure. "The
/// network reaches 87.9% of residents" is not a finding if 94% of the heights
/// behind it are p80s of a clutter raster.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BasisCounts {
    counts: [usize; 5],
    band_sum_m: [f64; 5],
    /// Sites whose planning value was cut by the ceiling.
    pub ceiling_clamped: usize,
    /// Sites whose evidence-derived UPPER BOUND is above the ceiling, so the
    /// `Bound::High` sweep plans them at the ceiling rather than at the
    /// evidence. Counted separately from `ceiling_clamped` because the two
    /// answer different questions and the second is much the larger number: a
    /// clutter neighbourhood with two 60 m towers in it produced a planning
    /// value of 12 m (never clamped, `ceiling_clamped` = 0) and a `high_m` of
    /// 44 m, which is the landmark that `DEFAULT_MAX_AGL_M` exists to refuse.
    pub high_bound_clamped: usize,
    /// Ceiling that did the cutting, for the report line.
    pub ceiling_m: f64,
}

impl BasisCounts {
    pub fn count(&self, k: BasisKind) -> usize {
        self.counts[k.index()]
    }

    /// Mean band width for a tier (metres); 0.0 when the tier is empty.
    pub fn mean_band_m(&self, k: BasisKind) -> f64 {
        let n = self.counts[k.index()];
        if n == 0 {
            0.0
        } else {
            self.band_sum_m[k.index()] / n as f64
        }
    }

    pub fn total(&self) -> usize {
        self.counts.iter().sum()
    }

    /// Share of sites whose height is a convention -- a per-class typical or
    /// the no-evidence fallback -- rather than anything read off the ground.
    ///
    /// NOT the number to headline. On the Berlin import every site has a
    /// clutter raster under it, so this returns ~0.00 for a data set in which
    /// NOT ONE height was measured. `inferred_share` is the number that
    /// belongs next to a coverage percentage.
    pub fn guessed_share(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            return 0.0;
        }
        (self.count(BasisKind::ClassTypical) + self.count(BasisKind::NoEvidence)) as f64 / t as f64
    }

    /// Share of sites whose height this module INVENTED -- everything except
    /// `OperatorSupplied`.
    ///
    /// This is the honest denominator for the 71.5%-vs-94.2% argument at the
    /// top of the file: a p80 of a clutter raster is evidence-derived, but it
    /// is still not a measurement, and it is what all 263 Berlin sites land
    /// on. A report that prints a coverage figure without this next to it is
    /// laundering the same uncertainty the module exists to expose.
    pub fn inferred_share(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            return 0.0;
        }
        (t - self.count(BasisKind::OperatorSupplied)) as f64 / t as f64
    }

    fn record(&mut self, e: &HeightEstimate) {
        let i = e.basis.kind().index();
        self.counts[i] += 1;
        self.band_sum_m[i] += e.band_m();
        if e.clamped_from_m.is_some() {
            self.ceiling_clamped += 1;
        }
        if planning_high_m(e, self.ceiling_m) < e.high_m {
            self.high_bound_clamped += 1;
        }
    }
}

/// The height a `Bound::High` sweep is allowed to PLAN with.
///
/// `HeightEstimate::high_m` is deliberately left un-clamped so a report can
/// say "the evidence went to 44 m". `at_bound` is not a report: what it
/// returns goes straight into `SiteSpec::h_agl_m` and gets planned with, and
/// a 44 m antenna is precisely the broadcast-tower failure `DEFAULT_MAX_AGL_M`
/// exists to stop. Measured on a clutter neighbourhood holding two 60 m
/// towers: planning value 12.0 m (under the ceiling, so `ceiling_clamped`
/// stayed 0 and nothing in the summary said a word), `high_m` 44.0 m, and
/// `at_bound(High)` handed 44.0 m to the analysis. Applying the same policy to
/// the bound keeps the sensitivity sweep a household-planner sweep;
/// `high_bound_clamped` counts how often it fired so the cap is not silent
/// either.
///
/// Operator-supplied heights are exempt here for the same reason they are
/// exempt from the planning-value clamp: a surveyed 45 m mast is the only real
/// measurement in the data set.
fn planning_high_m(e: &HeightEstimate, ceiling_m: f64) -> f64 {
    if matches!(e.basis, HeightBasis::OperatorSupplied)
        || !ceiling_m.is_finite()
        || ceiling_m <= 0.0
    {
        return e.high_m;
    }
    // `.max(h_agl_m)` only matters for a hand-built `EstimatedSites` whose
    // summary ceiling disagrees with the estimates; it keeps low <= high.
    e.high_m.min(ceiling_m).max(e.h_agl_m)
}

impl fmt::Display for BasisCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total = self.total();
        writeln!(f, "height provenance ({total} sites):")?;
        for k in BasisKind::ALL {
            let n = self.count(k);
            let pct = if total == 0 { 0.0 } else { 100.0 * n as f64 / total as f64 };
            writeln!(
                f,
                "  {:<22} {:>6}  {:>5.1}%  mean band {:>5.1} m",
                k.label(),
                n,
                pct,
                self.mean_band_m(k)
            )?;
        }
        let share = |n: usize| if total == 0 { 0.0 } else { 100.0 * n as f64 / total as f64 };
        writeln!(
            f,
            "  {:<22} {:>6}  {:>5.1}%  (planning value capped at {:.1} m)",
            "ceiling-clamped",
            self.ceiling_clamped,
            share(self.ceiling_clamped),
            self.ceiling_m
        )?;
        writeln!(
            f,
            "  {:<22} {:>6}  {:>5.1}%  (high-bound sweep capped at {:.1} m)",
            "high-bound-capped",
            self.high_bound_clamped,
            share(self.high_bound_clamped),
            self.ceiling_m
        )?;
        // The line a coverage percentage must never be printed without. It is
        // NOT `guessed_share`: on Berlin every site has clutter under it, so
        // that one reads 0% for a data set with zero measured heights.
        write!(
            f,
            "  {:<22} {:>6}  {:>5.1}%  of heights are not measurements",
            "inferred",
            total - self.count(BasisKind::OperatorSupplied),
            100.0 * self.inferred_share()
        )
    }
}

/// Sites ready for `gaps::analyse`, with the provenance of every height kept
/// alongside them.
pub struct EstimatedSites {
    /// Planned at `HeightEstimate::h_agl_m`, index-aligned with `estimates`.
    pub sites: Vec<SiteSpec>,
    pub estimates: Vec<HeightEstimate>,
    pub summary: BasisCounts,
}

impl EstimatedSites {
    /// The same sites re-heighted to one end of their bands, for the
    /// sensitivity run that makes the uncertainty a number instead of a
    /// caveat: analyse at `low`, at the planning value, and at `high`, and
    /// report the three answers.
    ///
    /// Operator-supplied heights do not move -- their band is a point -- so
    /// this varies exactly the sites whose height was invented, which is what
    /// the sensitivity is supposed to measure.
    pub fn at_bound(&self, bound: Bound) -> Vec<SiteSpec> {
        self.sites
            .iter()
            .zip(&self.estimates)
            .map(|(s, e)| {
                let mut s = s.clone();
                s.h_agl_m = match bound {
                    // `low_m <= h_agl_m <= ceiling` already holds, so only the
                    // upper bound needs the policy applied. See
                    // `planning_high_m`.
                    Bound::Low => e.low_m,
                    Bound::Planned => e.h_agl_m,
                    Bound::High => planning_high_m(e, self.summary.ceiling_m),
                };
                s
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Low,
    Planned,
    High,
}

/// Estimate heights for a batch of nodes and build the `SiteSpec`s the gap
/// analysis consumes, together with the provenance histogram.
pub fn estimate_sites(
    nodes: &[NodeInput],
    ev: &SiteEvidence<'_>,
    p: &EstimateParams,
) -> EstimatedSites {
    let mut sites = Vec::with_capacity(nodes.len());
    let mut estimates = Vec::with_capacity(nodes.len());
    let mut summary = BasisCounts { ceiling_m: p.max_agl_m, ..Default::default() };
    for n in nodes {
        let e = estimate_height(n.xy, n.height_agl_m, ev, p);
        summary.record(&e);
        sites.push(SiteSpec {
            xy: n.xy,
            h_agl_m: e.h_agl_m,
            // An operator-recorded power always wins; otherwise the caller's
            // default (normally `allowed_max_tx_dbm`), and None means "the
            // plan's reference power" to gaps::analyse.
            tx_power_dbm: n.tx_power_dbm.or(p.default_tx_power_dbm),
            label: n.label.clone(),
        });
        estimates.push(e);
    }
    EstimatedSites { sites, estimates, summary }
}

#[cfg(test)]
mod tests {
    use super::*;
    use planner_core::preset::{GENERIC_SX1262, MC_EU868_DEFAULT};

    /// 10 m cells, north-up (dy negative), origin at (0, 0) -- the geometry of
    /// the Berlin pack.
    fn grid_10m(n: usize, data: Vec<f32>) -> Grid {
        Grid::with_axes(Xy { x: 0.0, y: 0.0 }, 10.0, -10.0, n, n, data).unwrap()
    }

    fn uniform(n: usize, v: f32) -> Grid {
        grid_10m(n, vec![v; n * n])
    }

    /// Roofs and courtyards resolved at the cell size: alternate cells 20 m
    /// and 0 m. The point of the fixture is that ANY single-cell reading of it
    /// is wrong -- on a roof cell it says 20, in a courtyard 0, and between
    /// them (which is where a bilinear sample of a real cell-mean layer lands)
    /// about 10.
    fn checkerboard(n: usize, tall: f32) -> Grid {
        let mut d = vec![0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                if (row + col) % 2 == 0 {
                    d[row * n + col] = tall;
                }
            }
        }
        grid_10m(n, d)
    }

    fn class_grid(n: usize, class: ClutterClass) -> Grid {
        grid_10m(n, vec![class.code() as f32; n * n])
    }

    fn node(x: f64, y: f64) -> Xy {
        Xy { x, y }
    }

    #[test]
    fn operator_supplied_height_beats_every_other_evidence_source_and_has_no_spread() {
        // Every other tier is available and all of them would say something
        // else: a 40 m LoD2 block, a 20 m clutter neighbourhood, DenseUrban.
        let clutter = uniform(21, 20.0);
        let classes = class_grid(21, ClutterClass::DenseUrban);
        let buildings = [BuildingHint {
            xy: node(100.0, -100.0),
            footprint_m2: 900.0,
            height_m: 40.0,
        }];
        let ev = SiteEvidence {
            clutter: Some(&clutter),
            classes: Some(&classes),
            buildings: &buildings,
        };
        let e = estimate_height(node(100.0, -100.0), Some(6.0), &ev, &EstimateParams::default());
        assert_eq!(e.basis, HeightBasis::OperatorSupplied);
        assert_eq!((e.h_agl_m, e.low_m, e.high_m), (6.0, 6.0, 6.0));
        assert_eq!(e.band_m(), 0.0);
    }

    /// The clamp is a policy against INFERRED landmark heights. A surveyed
    /// 45 m mast is a measurement, and the planner does not get to overrule it.
    #[test]
    fn the_ceiling_does_not_touch_an_operator_supplied_height() {
        let ev = SiteEvidence::none();
        let e = estimate_height(node(0.0, 0.0), Some(45.0), &ev, &EstimateParams::default());
        assert_eq!(e.h_agl_m, 45.0);
        assert!(e.clamped_from_m.is_none());
    }

    #[test]
    fn a_node_on_a_tall_lod2_building_is_estimated_from_that_roof_not_the_neighbourhood() {
        // A 24 m block (Berlin Traufhoehe, under the 30 m ceiling) standing in
        // a neighbourhood whose clutter mean is a low 8 m -- the clutter tier
        // would say ~10 m, the roof says ~26 m.
        let clutter = uniform(21, 8.0);
        let buildings = [BuildingHint {
            xy: node(103.0, -104.0),
            footprint_m2: 750.0,
            height_m: 24.0,
        }];
        let ev = SiteEvidence { clutter: Some(&clutter), classes: None, buildings: &buildings };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        match e.basis {
            HeightBasis::Lod2Building { building_h_m, .. } => assert_eq!(building_h_m, 24.0),
            other => panic!("expected the roof tier, got {other:?}"),
        }
        assert_eq!(e.h_agl_m, 24.0 + ROOF_MAST_TYPICAL_M);
        assert_eq!((e.low_m, e.high_m), (24.0, 24.0 + ROOF_MAST_MAX_M));
        // And emphatically not the 8 m clutter answer.
        assert!(e.h_agl_m > 20.0);
    }

    /// LoD2 carries every garden shed. A 6 m2 hut two metres from the advert
    /// must not out-rank the block the node is really on.
    #[test]
    fn a_tiny_shed_closer_than_a_large_building_does_not_win() {
        let buildings = [
            // 6 m2, right on top of the advert: rejected by the footprint
            // filter before ranking even starts.
            BuildingHint { xy: node(101.0, -101.0), footprint_m2: 6.0, height_m: 3.0 },
            // 45 m2 annexe, 4 m away: passes the filter, so it is the nearest
            // CANDIDATE and only the footprint tie-break keeps it from winning.
            BuildingHint { xy: node(104.0, -100.0), footprint_m2: 45.0, height_m: 4.0 },
            // The 900 m2 block, 12 m away: further, but within
            // ADVERT_POSITION_UNCERTAINTY_M of the nearest candidate, so the
            // two are indistinguishable in distance and footprint decides.
            BuildingHint { xy: node(112.0, -100.0), footprint_m2: 900.0, height_m: 21.0 },
        ];
        let ev = SiteEvidence { clutter: None, classes: None, buildings: &buildings };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        match e.basis {
            HeightBasis::Lod2Building { footprint_m2, building_h_m, .. } => {
                assert_eq!(footprint_m2, 900.0);
                assert_eq!(building_h_m, 21.0);
            }
            other => panic!("expected the block, got {other:?}"),
        }
    }

    /// A big building genuinely FURTHER than the advert's precision does not
    /// get to steal the node from the house it is standing on -- otherwise
    /// every node within 25 m of a supermarket would move onto its roof.
    #[test]
    fn a_large_building_beyond_the_advert_uncertainty_does_not_steal_the_node() {
        let buildings = [
            BuildingHint { xy: node(101.0, -100.0), footprint_m2: 120.0, height_m: 9.0 },
            // 24 m away: outside 1 + 11.1, so the window excludes it.
            BuildingHint { xy: node(124.0, -100.0), footprint_m2: 5000.0, height_m: 12.0 },
        ];
        let ev = SiteEvidence { clutter: None, classes: None, buildings: &buildings };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        match e.basis {
            HeightBasis::Lod2Building { footprint_m2, .. } => assert_eq!(footprint_m2, 120.0),
            other => panic!("expected the near house, got {other:?}"),
        }
    }

    /// Only sub-threshold buildings nearby is NOT roof evidence, so the
    /// estimate has to fall through rather than mount a node on a shed.
    #[test]
    fn only_outbuildings_nearby_falls_through_to_the_clutter_tier() {
        let clutter = uniform(21, 12.0);
        let buildings = [BuildingHint {
            xy: node(101.0, -100.0),
            footprint_m2: 12.0,
            height_m: 3.0,
        }];
        let ev = SiteEvidence { clutter: Some(&clutter), classes: None, buildings: &buildings };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        assert!(matches!(e.basis, HeightBasis::ClutterNeighbourhood { .. }), "{:?}", e.basis);
    }

    /// THE UNDERESTIMATE THIS RULE EXISTS FOR.
    ///
    /// Clutter is a cell MEAN, so on ground where roofs and courtyards
    /// alternate, the value under the node is diluted by the gaps. A rooftop
    /// antenna sits at the top of the local built form, not at its mean.
    /// Measured on this fixture: the bilinear sample at the node is 10.0 m,
    /// the p80 of the 32-cell neighbourhood (16 roof cells, 16 courtyard) is
    /// 20.0 m, so the estimate is 22.0 m against a single-cell answer of
    /// 12.0 m -- 10 m of antenna height, which is the whole 71.5%-vs-94.2%
    /// argument.
    #[test]
    fn the_clutter_neighbourhood_beats_the_single_cell_mean_where_buildings_and_gaps_alternate() {
        let clutter = checkerboard(41, 20.0);
        // Halfway between four cell centres, where a bilinear read of a
        // cell-mean layer gives the diluted 10 m.
        let at = node(205.0, -205.0);
        let single = clutter.sample_bilinear(at).unwrap() as f64;
        assert!((single - 10.0).abs() < 1e-3, "fixture drifted: single-cell read {single}");

        let ev = SiteEvidence { clutter: Some(&clutter), classes: None, buildings: &[] };
        let e = estimate_height(at, None, &ev, &EstimateParams::default());
        match e.basis {
            HeightBasis::ClutterNeighbourhood { radius_m, percentile, sampled_cells } => {
                assert_eq!(radius_m, CLUTTER_NEIGHBOURHOOD_RADIUS_M);
                assert_eq!(percentile, CLUTTER_PERCENTILE);
                // Pinned so the measured numbers quoted above stay honest.
                assert_eq!(sampled_cells, 32);
            }
            other => panic!("expected the clutter tier, got {other:?}"),
        }
        assert!(
            e.h_agl_m > single + ROOF_MAST_TYPICAL_M + 5.0,
            "neighbourhood {} did not beat the single-cell {single}",
            e.h_agl_m
        );
    }

    #[test]
    fn a_heterogeneous_neighbourhood_yields_a_wider_band_than_a_uniform_one() {
        let mixed = checkerboard(41, 20.0);
        let flat = uniform(41, 20.0);
        let at = node(205.0, -205.0);
        let p = EstimateParams::default();
        let e_mixed = estimate_height(
            at,
            None,
            &SiteEvidence { clutter: Some(&mixed), classes: None, buildings: &[] },
            &p,
        );
        let e_flat = estimate_height(
            at,
            None,
            &SiteEvidence { clutter: Some(&flat), classes: None, buildings: &[] },
            &p,
        );
        // Same planning value (both p80s land on a 20 m roof); the difference
        // is entirely in how much the estimator admits it does not know.
        assert_eq!(e_mixed.h_agl_m, e_flat.h_agl_m);
        assert!(
            e_mixed.band_m() > e_flat.band_m() + 5.0,
            "mixed band {:.1} m vs uniform {:.1} m",
            e_mixed.band_m(),
            e_flat.band_m()
        );
        // The uniform case still reports a band: CLUTTER_MIN_HALF_BAND_M.
        assert!(e_flat.band_m() >= CLUTTER_MIN_HALF_BAND_M);
    }

    /// Open ground reads as zero clutter, and a node on open ground is on a
    /// pole rather than lying in the grass.
    #[test]
    fn an_all_zero_clutter_neighbourhood_still_plans_a_pole_not_ground_level() {
        let clutter = uniform(21, 0.0);
        let ev = SiteEvidence { clutter: Some(&clutter), classes: None, buildings: &[] };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        assert_eq!(e.h_agl_m, OPEN_GROUND_MAST_M);
        assert!(e.low_m >= MIN_ANTENNA_AGL_M);
    }

    #[test]
    fn a_class_only_site_gets_the_class_convention_with_a_wide_band() {
        let classes = class_grid(21, ClutterClass::Suburban);
        let ev = SiteEvidence { clutter: None, classes: Some(&classes), buildings: &[] };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        assert_eq!(e.basis, HeightBasis::ClassTypical(ClutterClass::Suburban));
        assert_eq!((e.h_agl_m, e.low_m, e.high_m), (9.0, 4.0, 15.0));
        assert!(e.basis.is_guess());
        // Forest is the widest class band on purpose: under the canopy or
        // above it are 20 dB apart and the class cannot tell them apart.
        let forest = class_grid(21, ClutterClass::Forest);
        let f = estimate_height(
            node(100.0, -100.0),
            None,
            &SiteEvidence { clutter: None, classes: Some(&forest), buildings: &[] },
            &EstimateParams::default(),
        );
        assert!(f.band_m() > e.band_m());
    }

    /// A corrupt class pixel must be COUNTED as no evidence, not rounded into
    /// Open -- the histogram is the product here.
    #[test]
    fn a_corrupt_class_code_becomes_no_evidence_rather_than_a_silent_open() {
        let classes = grid_10m(21, vec![3.5f32; 21 * 21]);
        let ev = SiteEvidence { clutter: None, classes: Some(&classes), buildings: &[] };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        assert_eq!(e.basis, HeightBasis::NoEvidence);
    }

    #[test]
    fn no_evidence_spans_the_whole_census_sweep_and_takes_the_pessimistic_point() {
        let ev = SiteEvidence::none();
        let e = estimate_height(node(0.0, 0.0), None, &ev, &EstimateParams::default());
        assert_eq!(e.basis, HeightBasis::NoEvidence);
        // 8 m is the assumption under which the census reported 71.5% and 176
        // components; 25 m is the one that reported 94.2% and a single
        // component. Both live inside this one node's band.
        assert_eq!((e.h_agl_m, e.low_m, e.high_m), (8.0, 2.0, 25.0));
    }

    #[test]
    fn the_ceiling_clamp_caps_the_planning_value_and_says_so_in_the_result() {
        // A 47 m LoD2 record -- a church, a Plattenbau end block, or a chimney
        // broken out as its own building.
        let buildings = [BuildingHint {
            xy: node(100.0, -100.0),
            footprint_m2: 600.0,
            height_m: 47.0,
        }];
        let ev = SiteEvidence { clutter: None, classes: None, buildings: &buildings };
        let e = estimate_height(node(100.0, -100.0), None, &ev, &EstimateParams::default());
        assert_eq!(e.h_agl_m, DEFAULT_MAX_AGL_M);
        assert_eq!(e.clamped_from_m, Some(47.0 + ROOF_MAST_TYPICAL_M));
        // The evidence's own upper bound stays visible: the clamp is a policy,
        // and a report must be able to say what it overrode.
        assert_eq!(e.high_m, 47.0 + ROOF_MAST_MAX_M);
        assert!(e.high_m > DEFAULT_MAX_AGL_M);
        // The invariant survives the clamp even though the whole band was
        // above the ceiling.
        assert!(e.low_m <= e.h_agl_m && e.h_agl_m <= e.high_m);

        // Raising the ceiling deliberately (a real mast) removes the clamp.
        let p = EstimateParams { max_agl_m: 80.0, ..EstimateParams::default() };
        let tall = estimate_height(node(100.0, -100.0), None, &ev, &p);
        assert_eq!(tall.h_agl_m, 49.0);
        assert!(tall.clamped_from_m.is_none());
    }

    /// The one invariant every consumer relies on, over generated inputs
    /// covering all five tiers, every clutter class, empty and populated
    /// building lists, coarse and fine rasters, and ceilings from absurdly low
    /// to effectively absent.
    #[test]
    fn the_low_planned_high_ordering_holds_for_every_basis_on_generated_inputs() {
        // Deterministic LCG (Numerical Recipes constants) -- a fixed seed keeps
        // a failure reproducible, which a thread-seeded RNG would not.
        let mut s: u64 = 0x5eed_1812;
        let mut next = || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((s >> 16) & 0xffff) as f64 / 65535.0
        };

        let classes: Vec<ClutterClass> = (0..8).filter_map(ClutterClass::from_code).collect();
        let mut seen = [0usize; 5];
        for i in 0..4000 {
            let n = 25usize;
            // Clutter: sometimes absent, sometimes flat, sometimes rough, and
            // sometimes carrying a nodata hole and a negative artefact.
            let clutter = match i % 4 {
                0 => None,
                _ => {
                    let mut d = vec![0f32; n * n];
                    let amp = next() * 40.0;
                    for (j, v) in d.iter_mut().enumerate() {
                        *v = (next() * amp) as f32;
                        if j % 37 == 0 {
                            *v = f32::NAN;
                        }
                        if j % 53 == 0 {
                            *v = -3.0;
                        }
                    }
                    Some(Grid::with_axes(
                        Xy { x: 0.0, y: 0.0 },
                        if i % 8 == 1 { 30.0 } else { 10.0 },
                        if i % 8 == 1 { -30.0 } else { -10.0 },
                        n,
                        n,
                        d,
                    )
                    .unwrap())
                }
            };
            let class = classes[i % classes.len()];
            let cg = if i % 3 == 0 { Some(class_grid(n, class)) } else { None };
            let mut blds = Vec::new();
            for _ in 0..(i % 4) {
                blds.push(BuildingHint {
                    xy: node(next() * 200.0, -(next() * 200.0)),
                    // Straddles MIN_REPEATER_FOOTPRINT_M2 so both the accepted
                    // and the rejected branch are exercised.
                    footprint_m2: (next() * 1200.0) as f32,
                    height_m: (next() * 60.0) as f32,
                });
            }
            let ev = SiteEvidence {
                clutter: clutter.as_ref(),
                classes: cg.as_ref(),
                buildings: &blds,
            };
            let p = EstimateParams {
                // 1.0 m is below MIN_ANTENNA_AGL_M on purpose: a caller may
                // pass anything, and the invariant must still hold.
                max_agl_m: [1.0, 12.0, 30.0, 1e9][i % 4],
                ..EstimateParams::default()
            };
            let op = if i % 11 == 0 { Some(next() * 60.0) } else { None };
            let at = node(next() * 200.0, -(next() * 200.0));
            let e = estimate_height(at, op, &ev, &p);
            assert!(
                e.low_m <= e.h_agl_m && e.h_agl_m <= e.high_m,
                "iteration {i}: {:?} out of order for {:?}",
                (e.low_m, e.h_agl_m, e.high_m),
                e.basis
            );
            assert!(
                e.low_m.is_finite() && e.h_agl_m.is_finite() && e.high_m.is_finite(),
                "iteration {i}: non-finite {e:?}"
            );
            if !matches!(e.basis, HeightBasis::OperatorSupplied) {
                assert!(e.h_agl_m <= p.max_agl_m, "iteration {i}: ceiling not applied: {e:?}");
            }
            // When the clamp fired it must be VISIBLE and honest: the planning
            // value sits exactly on the ceiling, and the recorded pre-clamp
            // value is genuinely above it.
            if let Some(from) = e.clamped_from_m {
                assert_eq!(e.h_agl_m, p.max_agl_m, "iteration {i}: {e:?}");
                assert!(from > p.max_agl_m, "iteration {i}: {e:?}");
            }
            seen[e.basis.kind().index()] += 1;
        }
        // The generator has to have reached every tier, or the invariant was
        // only tested on the easy ones.
        for k in BasisKind::ALL {
            assert!(seen[k.index()] > 0, "generator never produced {}", k.label());
        }
    }

    #[test]
    fn allowed_max_tx_is_the_hardware_limit_in_berlin_and_the_erp_ceiling_with_gain() {
        // 869.525 MHz sits in the same 869.4-869.65 slot as the community's
        // 869.618: 27 dBm ERP. min(22, 27 - 2 + 2.15) = 22 -> the SX1262 binds.
        assert_eq!(allowed_max_tx_dbm(&MC_EU868_DEFAULT, &GENERIC_SX1262), 22.0);

        // Bolt a 9 dBi colinear on the same radio and the regulation binds
        // instead: min(22, 27 - 9 + 2.15) = 20.15 dBm conducted, which is
        // 27 dBm ERP exactly.
        let colinear = DevicePreset { antenna_gain_dbi: 9.0, ..GENERIC_SX1262 };
        let hi_gain = allowed_max_tx_dbm(&MC_EU868_DEFAULT, &colinear);
        assert!((hi_gain - 20.15).abs() < 1e-9, "{hi_gain}");

        // And in a 25 mW slot the regulation binds even on the stock whip:
        // min(22, 14 - 2 + 2.15) = 14.15.
        let low_slot = RadioPreset { freq_mhz: 868.1, ..MC_EU868_DEFAULT };
        let low = allowed_max_tx_dbm(&low_slot, &GENERIC_SX1262);
        assert!((low - 14.15).abs() < 1e-9, "{low}");
    }

    #[test]
    fn the_batch_reports_a_provenance_histogram_instead_of_one_confident_number() {
        let clutter = checkerboard(41, 20.0);
        let buildings = [BuildingHint {
            xy: node(100.0, -100.0),
            footprint_m2: 800.0,
            height_m: 18.0,
        }];
        let ev = SiteEvidence {
            clutter: Some(&clutter),
            classes: None,
            buildings: &buildings,
        };
        let nodes = vec![
            // Operator-supplied.
            NodeInput {
                xy: node(100.0, -100.0),
                label: "surveyed".into(),
                height_agl_m: Some(12.0),
                tx_power_dbm: Some(14.0),
            },
            // On the LoD2 block.
            NodeInput {
                xy: node(102.0, -101.0),
                label: "roof".into(),
                height_agl_m: None,
                tx_power_dbm: None,
            },
            // Far from the block, still on the clutter raster.
            NodeInput {
                xy: node(300.0, -300.0),
                label: "clutter".into(),
                height_agl_m: None,
                tx_power_dbm: None,
            },
            // Off the raster entirely.
            NodeInput {
                xy: node(90_000.0, -90_000.0),
                label: "nowhere".into(),
                height_agl_m: None,
                tx_power_dbm: None,
            },
        ];
        let p = EstimateParams {
            default_tx_power_dbm: Some(allowed_max_tx_dbm(&MC_EU868_DEFAULT, &GENERIC_SX1262)),
            ..EstimateParams::default()
        };
        let out = estimate_sites(&nodes, &ev, &p);

        assert_eq!(out.sites.len(), 4);
        assert_eq!(out.summary.total(), 4);
        assert_eq!(out.summary.count(BasisKind::OperatorSupplied), 1);
        assert_eq!(out.summary.count(BasisKind::Lod2Building), 1);
        assert_eq!(out.summary.count(BasisKind::ClutterNeighbourhood), 1);
        assert_eq!(out.summary.count(BasisKind::NoEvidence), 1);
        assert_eq!(out.summary.ceiling_clamped, 0);
        assert_eq!(out.summary.guessed_share(), 0.25);
        // Operator-supplied has no spread; every inferred tier does.
        assert_eq!(out.summary.mean_band_m(BasisKind::OperatorSupplied), 0.0);
        assert!(out.summary.mean_band_m(BasisKind::NoEvidence) > 20.0);

        // Powers: the operator's own value survives, everyone else gets the
        // regulatory/hardware maximum.
        assert_eq!(out.sites[0].tx_power_dbm, Some(14.0));
        assert_eq!(out.sites[1].tx_power_dbm, Some(22.0));
        assert_eq!(out.sites[0].h_agl_m, 12.0);
        assert_eq!(out.sites[1].h_agl_m, 20.0); // 18 m roof + 2 m mast

        // The sensitivity handles move exactly the invented heights.
        let lo = out.at_bound(Bound::Low);
        let hi = out.at_bound(Bound::High);
        assert_eq!(lo[0].h_agl_m, 12.0, "a surveyed height must not move");
        assert_eq!(hi[0].h_agl_m, 12.0, "a surveyed height must not move");
        assert!(hi[3].h_agl_m > lo[3].h_agl_m, "the no-evidence site must move");

        let text = out.summary.to_string();
        assert!(text.contains("height provenance (4 sites)"), "{text}");
        assert!(text.contains("clutter-neighbourhood"), "{text}");
        assert!(text.contains("ceiling-clamped"), "{text}");
    }

    #[test]
    fn a_coarse_pack_grows_the_neighbourhood_once_and_reports_the_radius_it_used() {
        // On a 30 m pack the nominal 30 m radius reaches only the 4-neighbour
        // cross plus the centre: a p80 of five samples is nearly the maximum,
        // which is the landmark failure the percentile exists to avoid.
        let n = 21;
        let g = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, 30.0, -30.0, n, n, vec![14.0; n * n])
            .unwrap();
        let ev = SiteEvidence { clutter: Some(&g), classes: None, buildings: &[] };
        let e = estimate_height(node(300.0, -300.0), None, &ev, &EstimateParams::default());
        match e.basis {
            HeightBasis::ClutterNeighbourhood { radius_m, sampled_cells, .. } => {
                assert_eq!(radius_m, 2.0 * CLUTTER_NEIGHBOURHOOD_RADIUS_M);
                assert!(sampled_cells >= MIN_NEIGHBOURHOOD_CELLS, "{sampled_cells}");
            }
            other => panic!("expected the clutter tier, got {other:?}"),
        }
    }

    /// REGRESSION. The ceiling was applied to the planning value only, so the
    /// optimistic leg of the sensitivity sweep planned whatever the evidence's
    /// upper bound happened to be -- and did it invisibly, because the
    /// planning value itself was never clamped and `ceiling_clamped` therefore
    /// stayed at 0.
    ///
    /// Fixture: a 10 m neighbourhood with two 60 m towers in it. Measured
    /// before the fix: h_agl_m 12.0 m (p80 = 10, plus the mast), high_m 44.0 m
    /// (p95 = 40, plus the max mast), `ceiling_clamped` 0, and
    /// `at_bound(High)` handing 44.0 m AGL to `gaps::analyse` -- the
    /// broadcast-tower answer `DEFAULT_MAX_AGL_M` exists to refuse, arriving
    /// through the back door with nothing in the summary to show for it.
    #[test]
    fn the_high_bound_sweep_is_capped_by_the_same_ceiling_as_the_planning_value() {
        let n = 41usize;
        let mut d = vec![10.0f32; n * n];
        // Two towers inside the 30 m neighbourhood of (200, -200): enough to
        // drag p95 up, not enough to move p80.
        d[20 * n + 21] = 60.0;
        d[21 * n + 20] = 60.0;
        let g = grid_10m(n, d);
        let ev = SiteEvidence { clutter: Some(&g), classes: None, buildings: &[] };
        let nodes = vec![
            NodeInput {
                xy: node(200.0, -200.0),
                label: "beside two towers".into(),
                height_agl_m: None,
                tx_power_dbm: None,
            },
            // A surveyed 45 m mast: exempt from the ceiling on every bound,
            // exactly as it is exempt from the planning-value clamp.
            NodeInput {
                xy: node(200.0, -200.0),
                label: "surveyed mast".into(),
                height_agl_m: Some(45.0),
                tx_power_dbm: None,
            },
        ];
        let out = estimate_sites(&nodes, &ev, &EstimateParams::default());

        // The evidence itself is untouched: a report must still be able to say
        // "capped at 30 m, the clutter went to 44 m".
        assert!(
            out.estimates[0].high_m > DEFAULT_MAX_AGL_M,
            "fixture drifted: high_m {}",
            out.estimates[0].high_m
        );
        assert!(out.estimates[0].h_agl_m < DEFAULT_MAX_AGL_M, "fixture drifted");
        assert_eq!(out.summary.ceiling_clamped, 0, "the planning value was never clamped");

        let hi = out.at_bound(Bound::High);
        assert_eq!(hi[0].h_agl_m, DEFAULT_MAX_AGL_M, "the high sweep escaped the ceiling");
        assert_eq!(hi[1].h_agl_m, 45.0, "a surveyed mast must not be capped");
        // And the cap is not silent either.
        assert_eq!(out.summary.high_bound_clamped, 1);
        assert!(out.summary.to_string().contains("high-bound-capped"), "{}", out.summary);
    }

    /// REGRESSION. `f64::max` returns the OTHER operand when one side is NaN
    /// -- the same swallow this crate already documents for P.1812's internal
    /// clamps in `coverage()`. A non-finite `clutter_percentile` made
    /// `quantile` return NaN, and `NaN.max(OPEN_GROUND_MAST_M)` turned it into
    /// a confident 3.0 m "pole on open ground" over a uniform 20 m block,
    /// labelled ClutterNeighbourhood with a band of 2..24 m. An out-of-range
    /// percentile was quietly clamped to the MAXIMUM -- the single-tallest-
    /// object answer this tier exists to avoid -- while the basis went on
    /// reporting the value the caller asked for.
    #[test]
    fn a_nonsense_clutter_percentile_is_refused_instead_of_becoming_open_ground() {
        let clutter = uniform(21, 20.0);
        let ev = SiteEvidence { clutter: Some(&clutter), classes: None, buildings: &[] };
        let at = node(100.0, -100.0);
        for bad in [f64::NAN, 1.5, -0.2] {
            let p = EstimateParams { clutter_percentile: bad, ..EstimateParams::default() };
            let e = estimate_height(at, None, &ev, &p);
            assert_eq!(e.basis, HeightBasis::NoEvidence, "percentile {bad} produced {e:?}");
        }
        // The sane value on the same fixture still reaches the clutter tier,
        // so the guard is not simply switching the tier off.
        let good = estimate_height(at, None, &ev, &EstimateParams::default());
        assert!(matches!(good.basis, HeightBasis::ClutterNeighbourhood { .. }));
        assert_eq!(good.h_agl_m, 20.0 + ROOF_MAST_TYPICAL_M);
    }

    /// REGRESSION. A negative height is not a report, it is a leaked sentinel,
    /// and `gaps::analyse` repairs whatever it is handed
    /// (`h_agl_m.clamp(1.0, 3000.0)`) -- so believing it would have put a
    /// -9999 through to the model as a 1.0 m antenna wearing an
    /// "operator-supplied" label and a zero-width band, i.e. the most
    /// confident row in the histogram. Zero, on the other hand, is a real
    /// deployment and must survive.
    #[test]
    fn a_negative_operator_height_is_treated_as_missing_but_zero_is_believed() {
        let clutter = uniform(21, 20.0);
        let ev = SiteEvidence { clutter: Some(&clutter), classes: None, buildings: &[] };
        let at = node(100.0, -100.0);
        let p = EstimateParams::default();
        for bad in [-9999.0, -1.0, f64::NAN] {
            let e = estimate_height(at, Some(bad), &ev, &p);
            assert_ne!(
                e.basis.kind(),
                BasisKind::OperatorSupplied,
                "{bad} was believed as a surveyed height: {e:?}"
            );
            assert!(e.h_agl_m > 0.0 && e.h_agl_m.is_finite(), "{e:?}");
        }
        let zero = estimate_height(at, Some(0.0), &ev, &p);
        assert_eq!(zero.basis, HeightBasis::OperatorSupplied);
        assert_eq!((zero.h_agl_m, zero.low_m, zero.high_m), (0.0, 0.0, 0.0));
    }

    /// REGRESSION. `guessed_share` counts only the two convention tiers, so on
    /// the Berlin import -- where every site has a clutter raster under it and
    /// NOT ONE height was measured -- it returns 0.00. A report that prints
    /// "0% guessed" beside "87.9% of residents reached" has laundered exactly
    /// the uncertainty this module exists to expose.
    #[test]
    fn the_share_of_invented_heights_is_reported_even_when_every_site_has_clutter() {
        let clutter = uniform(41, 20.0);
        let ev = SiteEvidence { clutter: Some(&clutter), classes: None, buildings: &[] };
        let nodes: Vec<NodeInput> = (0..4)
            .map(|i| NodeInput {
                xy: node(100.0 + 40.0 * i as f64, -100.0),
                label: format!("n{i}"),
                height_agl_m: None,
                tx_power_dbm: None,
            })
            .collect();
        let out = estimate_sites(&nodes, &ev, &EstimateParams::default());
        assert_eq!(out.summary.count(BasisKind::ClutterNeighbourhood), 4);
        assert_eq!(out.summary.guessed_share(), 0.0, "the misleading number, kept for its own tier");
        assert_eq!(out.summary.inferred_share(), 1.0, "not one of these heights was measured");
        let text = out.summary.to_string();
        assert!(text.contains("of heights are not measurements"), "{text}");
        assert!(text.contains("100.0%"), "{text}");
    }

    /// REGRESSION. ERP is referenced to a HALF-WAVE DIPOLE and
    /// `antenna_gain_dbi` to an isotropic radiator, so
    /// `ceiling_erp - gain_dbi` is 2.15 dB pessimistic. It told a 9 dBi
    /// colinear to run at 18.0 dBm conducted when 20.15 dBm is still inside
    /// 500 mW ERP -- and it is only the high-gain case where the regulation
    /// binds at all, i.e. exactly where the number matters.
    #[test]
    fn the_erp_ceiling_is_referenced_to_a_dipole_not_an_isotropic_radiator() {
        let colinear = DevicePreset { antenna_gain_dbi: 9.0, ..GENERIC_SX1262 };
        let got = allowed_max_tx_dbm(&MC_EU868_DEFAULT, &colinear);
        assert!((got - (27.0 - 9.0 + DBI_TO_DBD_DB)).abs() < 1e-9, "{got}");
        assert!(got > 18.0 + 2.0, "the dBi/dBd conversion is missing: {got}");
        // The resulting ERP is the ceiling itself, which is the property the
        // whole calculation is for.
        assert!((got + 9.0 - DBI_TO_DBD_DB - 27.0).abs() < 1e-9);
    }

    /// A class raster must be read nearest-neighbour. Interpolating class
    /// CODES is arithmetic on labels: Open (0) beside DenseUrban (6) averages
    /// to 3, which decodes as Forest -- a different land cover, not a rounding
    /// error.
    #[test]
    fn class_codes_are_sampled_nearest_neighbour_not_interpolated() {
        let n = 4usize;
        let mut d = vec![ClutterClass::Open.code() as f32; n * n];
        for row in 0..n {
            d[row * n + 2] = ClutterClass::DenseUrban.code() as f32;
            d[row * n + 3] = ClutterClass::DenseUrban.code() as f32;
        }
        let g = grid_10m(n, d);
        let ev = SiteEvidence { clutter: None, classes: Some(&g), buildings: &[] };
        // On the boundary between an Open column and a DenseUrban one: the
        // bilinear read here is 3.0 (Forest).
        let at = node(15.0, -15.0);
        assert!((g.sample_bilinear(at).unwrap() - 3.0).abs() < 1e-3, "fixture drifted");
        let e = estimate_height(at, None, &ev, &EstimateParams::default());
        assert!(
            matches!(
                e.basis,
                HeightBasis::ClassTypical(ClutterClass::Open)
                    | HeightBasis::ClassTypical(ClutterClass::DenseUrban)
            ),
            "interpolated a class code into {:?}",
            e.basis
        );
    }
}

