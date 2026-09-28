//! MeshCore advert-map importer: the global node map served by
//! `map.meshcore.io/api/v1/nodes` as deployed-site records.
//!
//! This answers "what infrastructure already exists here", not "what did who
//! hear" -- an advert is a self-reported existence plus a self-reported
//! position, with no rx/tx pair and no SNR, so it deliberately does NOT become
//! an [`Observation`](crate::Observation): it cannot calibrate propagation, it
//! seeds the already-covered set that siting must not re-cover.
//! [`PositionQuality`] is reused because the one thing an advert does tell us
//! is how much to trust its coordinates.
//!
//! The front fetches the JSON and `planner-job nodes-import` turns it into a
//! nodeset. Nothing here touches the network -- the caller hands over
//! already-downloaded text, same contract as `potatomesh`, which keeps the
//! importer offline-testable.
//!
//! Shapes below are verified against the 2026-08-31 snapshot of the map
//! (59525 entries, 46 MB). What that snapshot
//! proves about the source, and why this file is defensive about it:
//! - Advert type histogram: 45263 repeaters (2), 11297 companions (1),
//!   2938 room servers (3), 27 sensors (4). No other code appeared, but an
//!   unknown code is preserved as [`MeshCoreKind::Other`] instead of being
//!   dropped in silence.
//! - `last_advert` is ALWAYS "YYYY-MM-DDTHH:MM:SS.sssZ" in the snapshot, and
//!   ranges from 1969-12-31 to 2105-02-12. Both ends are node clocks that
//!   never got set. 4399 entries are dated after the fetch itself, 192 of them
//!   by more than a week and 175 by more than a year. A pure "older than N
//!   days" test therefore lets a broken clock look eternally fresh, which is
//!   why there is a separate future-skew guard.
//! - 8324 of 59525 `adv_name` values contain non-ASCII (mostly emoji), one
//!   Berlin name is blank, and names are not unique.
//! - `params` is present but sometimes empty (112 entries carry four nulls),
//!   and its numbers arrive as either ints or floats WITHIN THE SAME FIELD
//!   (`bw` is int in 2005 entries and float in 57408, `freq` int in 277) --
//!   hence f64 for all four, with sf/cr range-checked afterwards rather than
//!   decoded as integers.
//! - Inside the Berlin pack bbox (lon 13.033..13.810, lat 52.310..52.710) the
//!   snapshot holds 500 nodes: 395 repeaters, 69 companions, 36 room servers.

use crate::PositionQuality;
use planner_core::geo::GeoPos;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MeshCoreMapError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// MeshCore advert type. The wire codes are the protocol's own
/// (1 companion/chat client, 2 repeater, 3 room server, 4 sensor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeshCoreKind {
    Companion,
    Repeater,
    RoomServer,
    Sensor,
    /// A code this build does not know, or a missing `type` field (code 0).
    /// Kept rather than dropped at parse time so that an unrecognised advert
    /// shows up in the report as filtered-out infrastructure instead of
    /// vanishing between the API and the operator.
    Other(i64),
}

impl MeshCoreKind {
    pub fn from_code(code: i64) -> Self {
        match code {
            1 => MeshCoreKind::Companion,
            2 => MeshCoreKind::Repeater,
            3 => MeshCoreKind::RoomServer,
            4 => MeshCoreKind::Sensor,
            other => MeshCoreKind::Other(other),
        }
    }

    pub fn code(self) -> i64 {
        match self {
            MeshCoreKind::Companion => 1,
            MeshCoreKind::Repeater => 2,
            MeshCoreKind::RoomServer => 3,
            MeshCoreKind::Sensor => 4,
            MeshCoreKind::Other(code) => code,
        }
    }
}

/// One deployed node as the advert map describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshCoreNode {
    pub public_key: String,
    pub name: String,
    pub kind: MeshCoreKind,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// `last_advert` as unix seconds, or None when the API sent nothing
    /// parseable. None is NOT a zero: real 1970 timestamps exist in the feed,
    /// so a sentinel would be indistinguishable from data (see
    /// [`parse_rfc3339_utc`]).
    pub last_advert_unix: Option<i64>,
    pub freq_mhz: Option<f64>,
    pub bw_khz: Option<f64>,
    pub sf: Option<u8>,
    pub cr: Option<u8>,
    pub pos_quality: PositionQuality,
}

impl MeshCoreNode {
    /// The position in the crate-wide type, when there is one.
    pub fn pos(&self) -> Option<GeoPos> {
        match (self.lat, self.lon) {
            (Some(lat_deg), Some(lon_deg)) => Some(GeoPos { lat_deg, lon_deg }),
            _ => None,
        }
    }
}

/// Lon/lat window. A named struct, not a bare 4-tuple: every bbox bug in this
/// repo so far has been a lat/lon swap at a call site, and `(min_lon, min_lat)`
/// order is exactly the trap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MapBbox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

impl MapBbox {
    pub fn new(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        MapBbox { min_lon, min_lat, max_lon, max_lat }
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        lon >= self.min_lon && lon <= self.max_lon && lat >= self.min_lat && lat <= self.max_lat
    }
}

/// What to keep out of the map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapFilter {
    pub bbox: Option<MapBbox>,
    /// Advert types to keep. Defaults to repeaters only: those are the fixed
    /// sites a plan has to route around; companions are people carrying
    /// phones, and their advert position is wherever they last stood.
    pub kinds: Vec<MeshCoreKind>,
    /// Drop adverts older than this many days. Roughly one quarter, chosen
    /// because MeshCore repeaters re-advertise on the order of hours: three
    /// months of silence means the site is gone, not quiet. None disables the
    /// age test entirely (the map then reads as a historical inventory).
    pub max_age_days: Option<u32>,
    /// Drop adverts dated more than this far into the future. Node clocks in
    /// the live feed run ahead by hours to days (all 395 Berlin repeaters in
    /// the 2026-08-31 snapshot are within 3 days of the fetch), while the
    /// broken ones sit in 2105: only 192 of 59525 entries are more than a week
    /// ahead, so a week is where skew stops and garbage starts. Without this
    /// guard a dead node with a runaway clock stays "fresh" forever.
    pub max_future_skew_days: Option<u32>,
    /// Drop entries with no usable position. The planner cannot place a site
    /// it cannot locate, but a caller counting deployed hardware may want them.
    pub require_position: bool,
}

impl Default for MapFilter {
    fn default() -> Self {
        MapFilter {
            bbox: None,
            kinds: vec![MeshCoreKind::Repeater],
            max_age_days: Some(90),
            max_future_skew_days: Some(7),
            require_position: true,
        }
    }
}

/// Why nodes did not survive the filter. Silently shrinking real
/// infrastructure data from 59525 to a few hundred is not acceptable: the
/// import reports this so the operator can see the cut and challenge it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapReport {
    pub total_parsed: usize,
    pub dropped_wrong_kind: usize,
    pub dropped_no_position: usize,
    pub dropped_out_of_bbox: usize,
    pub dropped_stale: usize,
    /// Dropped because `last_advert` could not be read at all, kept apart from
    /// `dropped_stale` on purpose. Both buckets mean "this node is not in the
    /// plan", but they mean opposite things to the operator: a big stale count
    /// says the mesh has died back, whereas a big unreadable count says the
    /// upstream timestamp format moved and THIS PARSER is broken. Folded
    /// together, an API that stops emitting ".sss" milliseconds turns the whole
    /// map into "everything is old" -- an empty plan that looks like a truthful
    /// answer instead of a build failure.
    pub dropped_no_timestamp: usize,
    pub dropped_future: usize,
    pub deduped: usize,
    pub kept: usize,
}

impl MapReport {
    /// Every parsed record lands in exactly one bucket, so this must equal
    /// `total_parsed`. Cheap invariant that catches a filter arm that forgets
    /// to count.
    pub fn total_accounted(&self) -> usize {
        self.dropped_wrong_kind
            + self.dropped_no_position
            + self.dropped_out_of_bbox
            + self.dropped_stale
            + self.dropped_no_timestamp
            + self.dropped_future
            + self.deduped
            + self.kept
    }
}

impl fmt::Display for MapReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "meshcore map: {} parsed -> {} kept (dropped {} wrong-kind, {} no-position, \
             {} out-of-bbox, {} stale, {} unreadable-timestamp, {} future-dated, \
             {} duplicate)",
            self.total_parsed,
            self.kept,
            self.dropped_wrong_kind,
            self.dropped_no_position,
            self.dropped_out_of_bbox,
            self.dropped_stale,
            self.dropped_no_timestamp,
            self.dropped_future,
            self.deduped,
        )
    }
}

/// Parse the exact timestamp shape this API emits, "YYYY-MM-DDTHH:MM:SS.sssZ",
/// into unix seconds. Anything else returns None.
///
/// Hand-rolled on purpose: a date crate would be a dependency for one fixed
/// 24-character layout. Rejecting instead of guessing matters more than usual
/// here, because the obvious fallback -- 0 -- is a legal value in this feed: the
/// snapshot really does contain 1970-01-01 and 1969-12-31 adverts from nodes
/// whose clock never got set. A 0 fallback would read as an advert 56 years in
/// the past, i.e. `now - t` far exceeds any `max_age_days` and the node is
/// dropped as dead -- live infrastructure would disappear from the plan with no
/// way to tell it apart from genuinely ancient entries.
pub fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 24 {
        return None;
    }
    if b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let field = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse::<i64>().ok()
    };
    let year = field(0, 4)?;
    let month = field(5, 7)?;
    let day = field(8, 10)?;
    let hour = field(11, 13)?;
    let minute = field(14, 16)?;
    let second = field(17, 19)?;
    // Milliseconds must be digits to accept the record, but are then dropped:
    // the record keeps whole seconds, and sub-second precision is meaningless
    // against a freshness window measured in days.
    field(20, 23)?;

    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    // Second 60 is a legal RFC3339 leap second; unix time has no such value,
    // so it folds onto :59 rather than rejecting a valid timestamp.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let second = second.min(59);
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`). Shifting the year to start in March puts the leap day
/// last, which is what removes every special case from the arithmetic. Valid
/// for negative results too, which the 1969 adverts in the feed need.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // 0..=399
    let shifted_month = (month + 9) % 12; // March = 0
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[derive(Debug, Deserialize)]
struct RawNode {
    public_key: Option<String>,
    #[serde(rename = "type")]
    kind: Option<i64>,
    adv_name: Option<String>,
    last_advert: Option<String>,
    adv_lat: Option<f64>,
    adv_lon: Option<f64>,
    params: Option<RawParams>,
}

#[derive(Debug, Deserialize)]
struct RawParams {
    freq: Option<f64>,
    bw: Option<f64>,
    // Every one of these is f64, sf/cr included, because this feed's JSON number
    // type is NOT stable per field: in the one 2026-08-31 snapshot `bw` arrives
    // as an int in 2005 entries and as a float in 57408, and `freq` as an int in
    // 277. Nothing says sf/cr are exempt from the same gateway-by-gateway
    // encoding, and serde's i64 decoder rejects `8.0` outright -- which would
    // abort the entire 46 MB parse over one node's radio settings. Range and
    // integrality are enforced by `small_uint` below instead, where a junk value
    // costs that node its params and nothing else.
    sf: Option<f64>,
    cr: Option<f64>,
}

/// A radio parameter that has to be a small whole number. Fractional, negative,
/// out-of-range or non-finite values return None rather than being rounded into
/// something plausible: a wrong spreading factor silently invents a link budget.
fn small_uint(v: f64) -> Option<u8> {
    (v.is_finite() && v.fract() == 0.0 && (0.0..=255.0).contains(&v)).then_some(v as u8)
}

/// Parse the whole advert map. Records are normalized, never rejected: a bad
/// timestamp, a missing position or an unknown advert type survives to the
/// filter, which is the layer that counts what it drops.
pub fn parse_map_json(json: &str) -> Result<Vec<MeshCoreNode>, MeshCoreMapError> {
    let raw: Vec<RawNode> = serde_json::from_str(json)?;
    Ok(raw
        .into_iter()
        .map(|r| {
            let (lat, lon) = clean_position(r.adv_lat, r.adv_lon);
            let kind = MeshCoreKind::from_code(r.kind.unwrap_or(0));
            let params = r.params;
            MeshCoreNode {
                public_key: r.public_key.unwrap_or_default(),
                name: r.adv_name.unwrap_or_default(),
                kind,
                lat,
                lon,
                last_advert_unix: r.last_advert.as_deref().and_then(parse_rfc3339_utc),
                freq_mhz: params.as_ref().and_then(|p| p.freq),
                bw_khz: params.as_ref().and_then(|p| p.bw),
                sf: params.as_ref().and_then(|p| p.sf).and_then(small_uint),
                cr: params.as_ref().and_then(|p| p.cr).and_then(small_uint),
                pos_quality: position_quality(kind, lat, lon),
            }
        })
        .collect())
}

/// (0,0) is Null Island -- the default a node reports before it knows where it
/// is -- and out-of-range values are corruption. Both become "no position"
/// here, matching how `rangetest` and `potatomesh` treat the same sentinel.
fn clean_position(lat: Option<f64>, lon: Option<f64>) -> (Option<f64>, Option<f64>) {
    match (lat, lon) {
        (Some(lat), Some(lon))
            if (lat, lon) != (0.0, 0.0)
                && (-90.0..=90.0).contains(&lat)
                && (-180.0..=180.0).contains(&lon) =>
        {
            (Some(lat), Some(lon))
        }
        _ => (None, None),
    }
}

fn position_quality(kind: MeshCoreKind, lat: Option<f64>, lon: Option<f64>) -> PositionQuality {
    let (Some(lat), Some(lon)) = (lat, lon) else {
        return PositionQuality::Unknown;
    };
    // Coordinates rounded to two decimals sit on a ~1.1 km grid, a hundred
    // times coarser than the 10 m pack raster, so they cannot site anything.
    // BOTH axes must be on the grid, because truncation is applied to a whole
    // fix: 8 of the 500 Berlin entries have a latitude that lands on it but only
    // 5 have both, and the other 3 are ordinary 4-decimal fixes whose latitude
    // happened to come out round. Requiring both is also what keeps the false
    // positive rate at ~1 in 10 000 precise fixes rather than ~1 in 100 (271 of
    // the 59525 nodes in the 2026-08-31 snapshot are double-coarse, against 2190
    // that are coarse on exactly one axis) -- worth it to avoid reading a 1 km
    // guess as a surveyed mast.
    let coarse = |v: f64| ((v * 100.0).round() - v * 100.0).abs() < 1e-6;
    if coarse(lat) && coarse(lon) {
        return PositionQuality::Truncated;
    }
    match kind {
        // Repeaters and room servers are installed hardware; their advert
        // position is where the operator put them and it does not move.
        MeshCoreKind::Repeater | MeshCoreKind::RoomServer => PositionQuality::FixedSite,
        // A companion advert may be a live GPS fix or a hand-typed guess, and
        // the advert does not say which.
        _ => PositionQuality::Unknown,
    }
}

impl MapFilter {
    /// Apply the filter at a caller-supplied `now`.
    ///
    /// `now_unix` is a parameter, never the system clock: an import has to be
    /// reproducible, and a test that reads the wall clock only fails once
    /// the fixture ages past the window.
    ///
    /// Tests run in a fixed order so every record lands in exactly one bucket
    /// (kind, then position, then bbox, then time), cheapest and broadest cut
    /// first. Dedup runs last, on survivors only, so a stale copy of a key
    /// cannot shadow the fresh one.
    pub fn apply(&self, nodes: &[MeshCoreNode], now_unix: i64) -> (Vec<MeshCoreNode>, MapReport) {
        let mut report = MapReport { total_parsed: nodes.len(), ..MapReport::default() };
        let max_age = self.max_age_days.map(|d| i64::from(d) * 86_400);
        let max_skew = self.max_future_skew_days.map(|d| i64::from(d) * 86_400);

        let mut kept: Vec<MeshCoreNode> = Vec::new();
        // public_key -> index into `kept`. Insertion order is preserved on
        // output so two imports of the same snapshot produce the same nodeset.
        let mut seen: HashMap<String, usize> = HashMap::new();

        for node in nodes {
            if !self.kinds.contains(&node.kind) {
                report.dropped_wrong_kind += 1;
                continue;
            }
            let pos = node.pos();
            if pos.is_none() && self.require_position {
                report.dropped_no_position += 1;
                continue;
            }
            if let Some(bbox) = self.bbox {
                match pos {
                    Some(p) if bbox.contains(p.lat_deg, p.lon_deg) => {}
                    // A node with no position cannot be shown to be inside the
                    // window, so a bbox excludes it even when positions are
                    // otherwise optional.
                    _ => {
                        report.dropped_out_of_bbox += 1;
                        continue;
                    }
                }
            }
            if max_age.is_some() || max_skew.is_some() {
                match node.last_advert_unix {
                    Some(t) => {
                        let age = now_unix - t;
                        if max_skew.is_some_and(|skew| -age > skew) {
                            report.dropped_future += 1;
                            continue;
                        }
                        if max_age.is_some_and(|limit| age > limit) {
                            report.dropped_stale += 1;
                            continue;
                        }
                    }
                    // Unknown age with an AGE rule in force: dropped, because an
                    // unreadable timestamp is exactly the case where liveness
                    // cannot be vouched for -- but into its own bucket, so a
                    // parser that has fallen behind the API cannot masquerade as
                    // a mesh that has died.
                    None if max_age.is_some() => {
                        report.dropped_no_timestamp += 1;
                        continue;
                    }
                    // Only the future-skew guard is armed. A node with no
                    // readable timestamp cannot be future-dated, so there is
                    // nothing here to fail: dropping it would delete live
                    // infrastructure under a config that switched the age test
                    // off on purpose.
                    None => {}
                }
            }
            // An advert whose `public_key` was missing or null carries the empty
            // string, which is a hole in the data and not a key: two anonymous
            // adverts are not the same node. Feeding them to the dedup map would
            // collapse every one of them onto the first and charge the loss to
            // the `deduped` counter, i.e. real infrastructure would disappear
            // labelled "duplicate". They pass through as distinct records.
            if node.public_key.is_empty() {
                kept.push(node.clone());
                continue;
            }
            match seen.get(&node.public_key).copied() {
                Some(idx) => {
                    report.deduped += 1;
                    // Most recent advert wins; a node with no timestamp loses
                    // to any node that has one.
                    if node.last_advert_unix > kept[idx].last_advert_unix {
                        kept[idx] = node.clone();
                    }
                }
                None => {
                    seen.insert(node.public_key.clone(), kept.len());
                    kept.push(node.clone());
                }
            }
        }
        report.kept = kept.len();
        (kept, report)
    }
}

/// Parse and filter in one call -- what an import wants.
pub fn load_map(
    json: &str,
    filter: &MapFilter,
    now_unix: i64,
) -> Result<(Vec<MeshCoreNode>, MapReport), MeshCoreMapError> {
    let nodes = parse_map_json(json)?;
    Ok(filter.apply(&nodes, now_unix))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed clock for the fixture: 2026-08-31T00:00:00Z. Never the system
    /// clock, or this suite would start failing on its own.
    const NOW: i64 = 1_788_134_400;

    /// Berlin window.
    const BERLIN: MapBbox = MapBbox {
        min_lon: 13.033,
        min_lat: 52.310,
        max_lon: 13.810,
        max_lat: 52.710,
    };

    /// Every hazard the live feed actually contains, in one array: a null
    /// adv_lat, a two-year-old advert, a companion among repeaters, a node in
    /// Munich, the same public_key twice with different last_advert, an
    /// emoji/umlaut name, a 2105 clock, an unparseable timestamp and an
    /// unknown advert type. Names use \u escapes so this file stays ASCII.
    const FIXTURE: &str = r#"[
      {"public_key":"01aa","type":2,"adv_name":"BLN Waldeckpark","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.506,"adv_lon":13.402,"params":{"freq":869.618,"bw":62.5,"cr":8,"sf":8},"link":"meshcore://..","source":"uploader"},
      {"public_key":"01bb","type":2,"adv_name":"No Fix","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":null,"adv_lon":13.402,"params":{}},
      {"public_key":"01cc","type":2,"adv_name":"Long Gone","last_advert":"2024-05-01T12:00:00.000Z","adv_lat":52.52,"adv_lon":13.405,"params":{"freq":869.618,"bw":62,"cr":5,"sf":11}},
      {"public_key":"01dd","type":1,"adv_name":"Somebody's Phone","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.5001,"adv_lon":13.4001},
      {"public_key":"01ee","type":2,"adv_name":"Munich Mast","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":48.0733,"adv_lon":11.5065},
      {"public_key":"01ff","type":2,"adv_name":"Twice Old","last_advert":"2026-08-01T10:00:00.000Z","adv_lat":52.4801,"adv_lon":13.3502},
      {"public_key":"01ff","type":2,"adv_name":"Twice New","last_advert":"2026-08-25T10:00:00.000Z","adv_lat":52.4802,"adv_lon":13.3503},
      {"public_key":"01a1","type":2,"adv_name":"B\u00fcrgeramt \u2600\ufe0f","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.4903,"adv_lon":13.4204},
      {"public_key":"01b2","type":2,"adv_name":"Runaway Clock","last_advert":"2105-02-12T05:48:08.000Z","adv_lat":52.4704,"adv_lon":13.3805},
      {"public_key":"01c3","type":2,"adv_name":"Bad Time","last_advert":"2026-08-20 08:15:00","adv_lat":52.4605,"adv_lon":13.3906},
      {"public_key":"01d4","type":7,"adv_name":"Future Protocol","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.4506,"adv_lon":13.3707}
    ]"#;

    #[test]
    fn the_default_filter_keeps_fresh_positioned_repeaters_and_accounts_for_every_drop() {
        let nodes = parse_map_json(FIXTURE).unwrap();
        assert_eq!(nodes.len(), 11, "nothing is dropped at parse time");
        let filter = MapFilter { bbox: Some(BERLIN), ..MapFilter::default() };
        let (kept, report) = filter.apply(&nodes, NOW);

        assert_eq!(report.total_parsed, 11);
        assert_eq!(report.dropped_wrong_kind, 2, "the companion and advert type 7");
        assert_eq!(report.dropped_no_position, 1, "null adv_lat");
        assert_eq!(report.dropped_out_of_bbox, 1, "Munich");
        assert_eq!(report.dropped_stale, 1, "the 2024 advert");
        assert_eq!(report.dropped_no_timestamp, 1, "the unparseable one, counted apart");
        assert_eq!(report.dropped_future, 1, "the 2105 clock");
        assert_eq!(report.deduped, 1);
        assert_eq!(report.kept, 3);
        assert_eq!(
            report.total_accounted(),
            report.total_parsed,
            "every record must land in exactly one bucket"
        );

        let names: Vec<&str> = kept.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["BLN Waldeckpark", "Twice New", "B\u{fc}rgeramt \u{2600}\u{fe0f}"]);
        assert_eq!(kept[0].kind, MeshCoreKind::Repeater);
        assert_eq!(kept[0].freq_mhz, Some(869.618));
        assert_eq!(kept[0].bw_khz, Some(62.5));
        assert_eq!(kept[0].sf, Some(8));
        assert_eq!(kept[0].cr, Some(8));
        assert_eq!(kept[0].pos_quality, PositionQuality::FixedSite);
        assert_eq!(kept[0].last_advert_unix, Some(1_787_213_700));

        // Dedup keeps the newer advert, and the newer position with it.
        assert_eq!(kept[1].public_key, "01ff");
        assert_eq!(kept[1].last_advert_unix, Some(1_787_652_000));
        assert_eq!(kept[1].pos().unwrap().lat_deg, 52.4802);
    }

    #[test]
    fn a_positionless_node_survives_when_the_caller_does_not_need_coordinates() {
        let nodes = parse_map_json(FIXTURE).unwrap();
        let filter = MapFilter { require_position: false, ..MapFilter::default() };
        let (kept, report) = filter.apply(&nodes, NOW);
        assert_eq!(report.dropped_no_position, 0);
        assert!(kept.iter().any(|n| n.public_key == "01bb" && n.pos().is_none()));
        assert_eq!(
            kept.iter().find(|n| n.public_key == "01bb").unwrap().pos_quality,
            PositionQuality::Unknown
        );
        // No bbox this time, so Munich stays.
        assert!(kept.iter().any(|n| n.name == "Munich Mast"));
        assert_eq!(report.total_accounted(), report.total_parsed);
    }

    #[test]
    fn a_positionless_node_cannot_be_proven_inside_a_bbox() {
        let nodes = parse_map_json(FIXTURE).unwrap();
        let filter = MapFilter {
            bbox: Some(BERLIN),
            require_position: false,
            ..MapFilter::default()
        };
        let (_, report) = filter.apply(&nodes, NOW);
        assert_eq!(report.dropped_no_position, 0);
        assert_eq!(report.dropped_out_of_bbox, 2, "Munich plus the unlocatable node");
    }

    #[test]
    fn disabling_the_age_tests_turns_the_map_into_a_historical_inventory() {
        let nodes = parse_map_json(FIXTURE).unwrap();
        let filter = MapFilter {
            bbox: Some(BERLIN),
            max_age_days: None,
            max_future_skew_days: None,
            ..MapFilter::default()
        };
        let (kept, report) = filter.apply(&nodes, NOW);
        assert_eq!(report.dropped_stale, 0);
        assert_eq!(report.dropped_future, 0);
        // 2024 advert, 2105 advert and the unparseable one all come back.
        assert_eq!(report.kept, 6);
        assert!(kept.iter().any(|n| n.name == "Long Gone"));
        assert!(kept.iter().any(|n| n.last_advert_unix.is_none()));
    }

    #[test]
    fn other_kinds_are_selectable_and_unknown_codes_round_trip() {
        let nodes = parse_map_json(FIXTURE).unwrap();
        let filter = MapFilter {
            kinds: vec![MeshCoreKind::Companion, MeshCoreKind::Other(7)],
            bbox: Some(BERLIN),
            ..MapFilter::default()
        };
        let (kept, _) = filter.apply(&nodes, NOW);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].kind, MeshCoreKind::Companion);
        assert_eq!(kept[1].kind.code(), 7);
        assert_eq!(MeshCoreKind::from_code(2), MeshCoreKind::Repeater);
        assert_eq!(MeshCoreKind::from_code(4).code(), 4);
        // A missing `type` becomes code 0, which no real advert uses, so it can
        // never be selected by accident.
        assert_eq!(MeshCoreKind::from_code(0), MeshCoreKind::Other(0));
    }

    #[test]
    fn the_timestamp_parser_rejects_malformed_input_instead_of_returning_zero() {
        // Zero is a legal value in this feed (the snapshot contains 1970 and
        // 1969 adverts), so a silent 0 fallback would read as an advert 56
        // years old: under `now - t > max_age_days` the node is dropped as
        // dead, and a live repeater vanishes from the plan indistinguishably
        // from a genuinely ancient one.
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00.000Z"), Some(0));
        for bad in [
            "",
            "2026-08-20 08:15:00",         // space instead of T
            "2026-08-20T08:15:00Z",        // no milliseconds
            "2026-08-20T08:15:00.000+02:00", // offset, not Z
            "2026-08-20T08:15:00.000z",    // lowercase zone
            "2026-13-20T08:15:00.000Z",    // month 13
            "2026-00-20T08:15:00.000Z",    // month 0
            "2026-02-30T08:15:00.000Z",    // day past February
            "2025-02-29T08:15:00.000Z",    // not a leap year
            "2026-08-20T24:15:00.000Z",    // hour 24
            "2026-08-20T08:60:00.000Z",    // minute 60
            "2026-08-20T08:15:61.000Z",    // second 61
            "2026-08-2xT08:15:00.000Z",    // non-digit
            "202-08-20T08:15:00.0000Z",    // right length, wrong layout
        ] {
            assert_eq!(parse_rfc3339_utc(bad), None, "must reject {bad:?}");
        }
        // Shapes that are correct, including the ones the calendar maths has
        // to get right.
        assert_eq!(parse_rfc3339_utc("2026-07-29T17:43:32.000Z"), Some(1_785_347_012));
        assert_eq!(parse_rfc3339_utc("2000-02-29T12:00:00.000Z"), Some(951_825_600));
        assert_eq!(parse_rfc3339_utc("1969-12-31T23:00:03.000Z"), Some(-3597));
        assert_eq!(parse_rfc3339_utc("2105-02-12T05:48:08.000Z"), Some(4_263_860_888));
        // Leap second folds onto :59 rather than being rejected.
        assert_eq!(
            parse_rfc3339_utc("2016-12-31T23:59:60.000Z"),
            parse_rfc3339_utc("2016-12-31T23:59:59.000Z")
        );
    }

    #[test]
    fn a_coarse_position_is_reported_as_truncated_not_as_a_surveyed_site() {
        let json = r#"[
          {"public_key":"01","type":2,"adv_name":"Rounded","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.5,"adv_lon":13.4},
          {"public_key":"02","type":2,"adv_name":"Precise","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.5063,"adv_lon":13.4021},
          {"public_key":"03","type":2,"adv_name":"Null Island","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":0,"adv_lon":0}
        ]"#;
        let nodes = parse_map_json(json).unwrap();
        assert_eq!(nodes[0].pos_quality, PositionQuality::Truncated);
        assert_eq!(nodes[1].pos_quality, PositionQuality::FixedSite);
        assert_eq!(nodes[2].pos_quality, PositionQuality::Unknown);
        assert!(nodes[2].lat.is_none(), "(0,0) is the unset default, not a place");
    }

    #[test]
    fn the_report_prints_one_line_naming_every_bucket() {
        let nodes = parse_map_json(FIXTURE).unwrap();
        let filter = MapFilter { bbox: Some(BERLIN), ..MapFilter::default() };
        let (_, report) = filter.apply(&nodes, NOW);
        let line = report.to_string();
        assert!(line.starts_with("meshcore map: 11 parsed -> 3 kept"), "{line}");
        assert!(line.contains("1 stale"), "{line}");
        assert!(line.contains("1 unreadable-timestamp"), "{line}");
        assert!(!line.contains('\n'), "a build log wants one line");
    }

    /// Regression: two adverts that arrived without a `public_key` are two
    /// nodes, not one. Before the fix both carried the empty-string sentinel,
    /// the dedup map keyed on it, and the second one was thrown away and
    /// counted as a duplicate -- a hole in the feed erasing real hardware under
    /// a counter that says nothing was lost.
    #[test]
    fn adverts_with_no_identity_are_never_deduped_against_each_other() {
        let json = r#"[
          {"type":2,"adv_name":"Anonymous A","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.5061,"adv_lon":13.4021},
          {"public_key":null,"type":2,"adv_name":"Anonymous B","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.4902,"adv_lon":13.4203},
          {"public_key":"01aa","type":2,"adv_name":"Named","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.4703,"adv_lon":13.3804}
        ]"#;
        let nodes = parse_map_json(json).unwrap();
        let filter = MapFilter { bbox: Some(BERLIN), ..MapFilter::default() };
        let (kept, report) = filter.apply(&nodes, NOW);
        assert_eq!(report.deduped, 0, "an absent key is a hole, not a shared identity");
        assert_eq!(report.kept, 3);
        let names: Vec<&str> = kept.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Anonymous A", "Anonymous B", "Named"]);
        assert_eq!(report.total_accounted(), report.total_parsed);
        // Real keys still collapse.
        let dupes = r#"[
          {"public_key":"01aa","type":2,"adv_name":"First","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.5061,"adv_lon":13.4021},
          {"public_key":"01aa","type":2,"adv_name":"Second","last_advert":"2026-08-21T08:15:00.000Z","adv_lat":52.5062,"adv_lon":13.4022}
        ]"#;
        let (kept, report) = filter.apply(&parse_map_json(dupes).unwrap(), NOW);
        assert_eq!((report.deduped, report.kept), (1, 1));
        assert_eq!(kept[0].name, "Second");
    }

    /// Regression: `max_age_days: None` says "do not test age". It used to drop
    /// every advert with an unreadable timestamp anyway, because the block was
    /// entered on the future-skew guard alone -- which is on by default, so the
    /// obvious "give me the historical inventory" call quietly lost nodes.
    #[test]
    fn switching_off_the_age_limit_keeps_adverts_whose_timestamp_cannot_be_read() {
        let nodes = parse_map_json(FIXTURE).unwrap();
        let filter = MapFilter {
            bbox: Some(BERLIN),
            max_age_days: None,
            // Left at the default Some(7): the skew guard alone must not
            // resurrect the age test.
            ..MapFilter::default()
        };
        let (kept, report) = filter.apply(&nodes, NOW);
        assert_eq!(report.dropped_stale, 0, "no age rule is in force");
        assert_eq!(report.dropped_no_timestamp, 0, "nothing to test it against");
        assert_eq!(report.dropped_future, 1, "the 2105 clock is still garbage");
        assert!(kept.iter().any(|n| n.name == "Bad Time" && n.last_advert_unix.is_none()));
        assert!(kept.iter().any(|n| n.name == "Long Gone"), "the 2024 advert too");
        assert_eq!(report.total_accounted(), report.total_parsed);
    }

    /// Regression: an upstream timestamp-format change must not read as "the
    /// mesh is dead". Both buckets end in an empty plan, but only one of them
    /// tells the operator the build is broken rather than the city.
    #[test]
    fn a_feed_whose_timestamp_format_moved_is_reported_as_unreadable_not_as_stale() {
        // The same adverts as the fixture's live one, minus the ".sss" the API
        // emits today.
        let json = r#"[
          {"public_key":"01aa","type":2,"adv_name":"A","last_advert":"2026-08-20T08:15:00Z","adv_lat":52.5061,"adv_lon":13.4021},
          {"public_key":"01bb","type":2,"adv_name":"B","last_advert":"2026-08-20T08:15:00+00:00","adv_lat":52.4902,"adv_lon":13.4203}
        ]"#;
        let filter = MapFilter { bbox: Some(BERLIN), ..MapFilter::default() };
        let (kept, report) = filter.apply(&parse_map_json(json).unwrap(), NOW);
        assert!(kept.is_empty());
        assert_eq!(report.dropped_stale, 0, "these adverts are not old, they are unread");
        assert_eq!(report.dropped_no_timestamp, 2);
        assert!(report.to_string().contains("2 unreadable-timestamp"), "{report}");
    }

    /// Regression: `bw` is an int in 2005 of the snapshot's entries and a float
    /// in 57408, so the feed's number encoding is per-entry, not per-field.
    /// Decoding sf/cr as i64 made a single `"sf": 8.0` abort the whole 46 MB
    /// parse -- an import failing outright over one node's radio
    /// settings.
    #[test]
    fn a_float_encoded_spreading_factor_costs_one_node_its_params_not_the_parse() {
        let json = r#"[
          {"public_key":"01aa","type":2,"adv_name":"Float","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.5061,"adv_lon":13.4021,"params":{"freq":869,"bw":62.5,"sf":8.0,"cr":5.0}},
          {"public_key":"01bb","type":2,"adv_name":"Junk","last_advert":"2026-08-20T08:15:00.000Z","adv_lat":52.4902,"adv_lon":13.4203,"params":{"freq":869.618,"bw":62.5,"sf":8.5,"cr":300}}
        ]"#;
        let nodes = parse_map_json(json).expect("one odd number must not fail the file");
        assert_eq!((nodes[0].sf, nodes[0].cr), (Some(8), Some(5)));
        assert_eq!(nodes[0].freq_mhz, Some(869.0), "an int freq is still a frequency");
        // Fractional and out-of-range values are dropped, not rounded: a wrong
        // spreading factor invents a link budget.
        assert_eq!((nodes[1].sf, nodes[1].cr), (None, None));
        assert_eq!(nodes[1].bw_khz, Some(62.5), "the readable params survive");
        assert_eq!(small_uint(-1.0), None);
        assert_eq!(small_uint(255.0), Some(255));
        assert_eq!(small_uint(f64::NAN), None);
    }

    /// A saved copy of the advert map: `SIMESH_MESHCORE_SNAPSHOT` when set,
    /// else the front's download cache.
    fn snapshot_path() -> std::path::PathBuf {
        std::env::var_os("SIMESH_MESHCORE_SNAPSHOT").map(Into::into).unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../packs/.cache/meshcore/nodes.json")
        })
    }

    /// Reads the real 46 MB advert map and skips when there is no saved copy,
    /// so the one test that touches real data runs in every `cargo test`
    /// where the copy exists. It costs ~1 s.
    #[test]
    fn the_real_advert_map_yields_the_expected_berlin_repeater_count() {
        let path = snapshot_path();
        if !path.exists() {
            eprintln!("SKIP: MeshCore advert map not saved ({})", path.display());
            return;
        }
        let json = std::fs::read_to_string(&path).expect("cached MeshCore map");
        let nodes = parse_map_json(&json).unwrap();
        assert!(nodes.len() > 50_000, "snapshot had 59525 entries, got {}", nodes.len());

        // Before any age test: ~500 nodes in the box, 395 repeaters, 69
        // companions and 36 room servers in the 2026-08-31 snapshot.
        let in_box = |k: MeshCoreKind| {
            nodes
                .iter()
                .filter(|n| n.kind == k)
                .filter(|n| n.pos().is_some_and(|p| BERLIN.contains(p.lat_deg, p.lon_deg)))
                .count()
        };
        let (rep, comp, room) = (
            in_box(MeshCoreKind::Repeater),
            in_box(MeshCoreKind::Companion),
            in_box(MeshCoreKind::RoomServer),
        );
        println!("berlin box: {rep} repeaters, {comp} companions, {room} room servers");
        assert!((300..500).contains(&rep), "{rep} repeaters");
        assert!((30..150).contains(&comp), "{comp} companions");
        assert!((15..80).contains(&room), "{room} room servers");

        // "Now" is taken from the snapshot rather than the wall clock, so the
        // freshness assertions keep their meaning after the cache is refetched.
        // It is the 95th percentile of advert times, NOT their max: the newest
        // advert in the real file is dated 2105 by a node whose clock never got
        // set, and using it would date every live repeater 79 years into the
        // past. The far-future tail only starts above the 99.9th percentile, so
        // p95 lands within a day of the actual fetch time.
        let mut times: Vec<i64> = nodes.iter().filter_map(|n| n.last_advert_unix).collect();
        times.sort_unstable();
        let now = times[times.len() * 95 / 100];

        let all_ages = MapFilter {
            bbox: Some(BERLIN),
            max_age_days: None,
            max_future_skew_days: None,
            ..MapFilter::default()
        };
        let (_, report) = all_ages.apply(&nodes, now);
        // 395 repeaters in the 2026-08-31 snapshot; the live map drifts, so the
        // assertion is a range.
        assert!(
            (300..500).contains(&report.kept),
            "expected ~395 Berlin repeaters, got {report}"
        );
        assert_eq!(report.total_accounted(), report.total_parsed);

        let fresh = MapFilter { bbox: Some(BERLIN), ..MapFilter::default() };
        let (kept, fresh_report) = fresh.apply(&nodes, now);
        // 266 of the 395 were fresh within 90 days of the snapshot: most of
        // what the map shows is already dead, which is the whole reason
        // freshness filtering is mandatory.
        assert!(
            fresh_report.kept > 100 && fresh_report.kept < report.kept,
            "{fresh_report}"
        );
        assert!(kept.iter().all(|n| n.kind == MeshCoreKind::Repeater));
        assert!(kept.iter().all(|n| n.pos().is_some()));
        assert_eq!(fresh_report.total_accounted(), fresh_report.total_parsed);
        // Every timestamp in the snapshot is "YYYY-MM-DDTHH:MM:SS.sssZ", so this
        // bucket must be empty. A nonzero count here means the API changed shape
        // and the plan is about to go dark for a reason that has nothing to do
        // with the mesh.
        assert_eq!(fresh_report.dropped_no_timestamp, 0, "{fresh_report}");
        // Public keys are 64 hex characters and unique in this snapshot, so
        // nothing may be lost to dedup and nothing may fall through the
        // no-identity path.
        assert_eq!(fresh_report.deduped, 0, "{fresh_report}");
        assert!(nodes.iter().all(|n| n.public_key.len() == 64));
        // Coarse fixes are rare but real, and only count when BOTH axes are on
        // the ~1.1 km grid: 5 of the 500 Berlin-bbox nodes in the 2026-08-31
        // snapshot, against 8 whose latitude alone lands there.
        let truncated = kept.iter().filter(|n| n.pos_quality == PositionQuality::Truncated).count();
        assert!(truncated < kept.len() / 10, "{truncated} truncated of {}", kept.len());
        println!("{fresh_report}");
    }
}
