//! PotatoMesh REST API importer: its instances aggregate Meshtastic AND
//! MeshCore via read-only ingestors, and GET endpoints are public.
//!
//! Field shapes verified 2026-08-30 against the repo's SQL schemas
//! (`data/{nodes,neighbors,traces}.sql` — the Sinatra layer serves rows with
//! these column names), and the `/api/nodes` row against potatomesh.net on
//! 2026-09-27. The importer takes JSON text, not URLs: transport stays with
//! the caller, keeping this crate offline-testable.
//!
//! Caveats, by design of the source data:
//! - `neighbors` rows are single-direction link reports: `node_id` REPORTED
//!   hearing `neighbor_id` at `snr` → rx = node, tx = neighbor.
//! - `traces` rows carry the requester-side snr/rssi of the final hop
//!   (per-hop SNR lists are not in the schema); src/dest are numeric node
//!   `num`s that must be joined through /api/nodes.
//! - Node `precision_bits` < 30 marks a position truncated on purpose
//!   (Meshtastic's position precision setting) → PositionQuality::Truncated,
//!   whatever its `location_source`: a hand-entered position is truncated just
//!   the same before it is broadcast. Otherwise `location_source` containing
//!   "MANUAL" → FixedSite, a full-precision fix → GpsFix. MeshCore rows carry
//!   neither field; a MeshCore repeater or room server is installed hardware
//!   and reads FixedSite, as it does on the MeshCore map.
//! - `lora_freq` is whole MHz. `modem_preset` is a Meshtastic preset name
//!   (`MediumFast`, `LONG_FAST`) or MeshCore's `SF8/BW62/CR8`.

use crate::{Observation, PositionQuality, SourceTag};
use planner_core::geo::GeoPos;
use serde::Deserialize;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PotatoMeshError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeRow {
    pub node_id: String,
    pub num: Option<i64>,
    #[serde(default)]
    pub short_name: Option<String>,
    #[serde(default)]
    pub long_name: Option<String>,
    /// Meshtastic `CLIENT`, `ROUTER`, … or MeshCore `REPEATER`,
    /// `COMPANION`, `ROOM_SERVER`.
    #[serde(default)]
    pub role: Option<String>,
    /// Unix seconds the instance last heard the node.
    #[serde(default)]
    pub last_heard: Option<i64>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub precision_bits: Option<i64>,
    pub location_source: Option<String>,
    #[serde(default)]
    pub lora_freq: Option<f64>,
    #[serde(default)]
    pub modem_preset: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
}

/// LoRa settings a node list reports: frequency in MHz, spreading factor,
/// bandwidth in kHz, coding rate as the denominator of 4/x.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Radio {
    pub freq_mhz: f64,
    pub sf: u8,
    pub bw_khz: f64,
    pub cr: u8,
}

impl NodeRow {
    /// The position, when there is one: (0,0) and out-of-range values are none.
    pub fn pos(&self) -> Option<GeoPos> {
        match (self.latitude, self.longitude) {
            (Some(lat), Some(lon))
                if (lat, lon) != (0.0, 0.0)
                    && (-90.0..=90.0).contains(&lat)
                    && (-180.0..=180.0).contains(&lon) =>
            {
                Some(GeoPos { lat_deg: lat, lon_deg: lon })
            }
            _ => None,
        }
    }

    pub fn quality(&self) -> PositionQuality {
        if self.pos().is_none() {
            return PositionQuality::Unknown;
        }
        if self.precision_bits.is_some_and(|bits| bits < 30) {
            return PositionQuality::Truncated;
        }
        if self
            .location_source
            .as_deref()
            .is_some_and(|s| s.to_ascii_uppercase().contains("MANUAL"))
        {
            return PositionQuality::FixedSite;
        }
        if self.precision_bits.is_some() {
            return PositionQuality::GpsFix;
        }
        let meshcore = self.protocol.as_deref().is_some_and(|p| p.eq_ignore_ascii_case("meshcore"));
        let installed = self
            .role
            .as_deref()
            .is_some_and(|r| matches!(r.to_ascii_uppercase().as_str(), "REPEATER" | "ROOM_SERVER"));
        if meshcore && installed {
            PositionQuality::FixedSite
        } else {
            PositionQuality::Unknown
        }
    }

    /// The node's name: the long name, else the short one, else empty.
    pub fn label(&self) -> String {
        [&self.long_name, &self.short_name]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_string()
    }

    /// Frequency and modem settings when both are reported and the preset is
    /// one this parser knows.
    pub fn radio(&self) -> Option<Radio> {
        let freq_mhz = self.lora_freq.filter(|f| f.is_finite() && *f > 0.0)?;
        let (sf, bw_khz, cr) = modem_preset(self.modem_preset.as_deref()?)?;
        Some(Radio { freq_mhz, sf, bw_khz, cr })
    }
}

/// (spreading factor, bandwidth kHz, coding rate 4/x) of a modem preset.
///
/// Meshtastic names come in CamelCase (`MediumFast`) or SCREAMING_SNAKE
/// (`MEDIUM_FAST`); both fold to the same key. MeshCore's `SF8/BW62/CR8`
/// carries the numbers, with bandwidth truncated to whole kHz, so the LoRa
/// bandwidths below 125 kHz are restored from their truncation.
pub fn modem_preset(p: &str) -> Option<(u8, f64, u8)> {
    if p.to_ascii_uppercase().starts_with("SF") && p.contains('/') {
        let mut sf = None;
        let mut bw = None;
        let mut cr = None;
        for part in p.split('/') {
            let up = part.trim().to_ascii_uppercase();
            if let Some(v) = up.strip_prefix("SF") {
                sf = v.parse::<u8>().ok();
            } else if let Some(v) = up.strip_prefix("BW") {
                bw = v.parse::<f64>().ok().map(|b| match b as u32 {
                    7 => 7.8,
                    10 => 10.4,
                    15 => 15.6,
                    20 => 20.8,
                    31 => 31.25,
                    41 => 41.7,
                    62 => 62.5,
                    _ => b,
                });
            } else if let Some(v) = up.strip_prefix("CR") {
                cr = v.parse::<u8>().ok();
            }
        }
        let (sf, bw, cr) = (sf?, bw?, cr?);
        return ((5..=12).contains(&sf) && bw > 0.0 && (5..=8).contains(&cr)).then_some((sf, bw, cr));
    }
    let key: String = p.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    Some(match key.as_str() {
        "SHORTTURBO" => (7, 500.0, 5),
        "SHORTFAST" => (7, 250.0, 5),
        "SHORTSLOW" => (8, 250.0, 5),
        "MEDIUMFAST" => (9, 250.0, 5),
        "MEDIUMSLOW" => (10, 250.0, 5),
        "LONGFAST" => (11, 250.0, 5),
        "LONGMODERATE" => (11, 125.0, 8),
        "LONGSLOW" => (12, 125.0, 8),
        "VERYLONGSLOW" => (12, 62.5, 8),
        _ => return None,
    })
}

/// Every row of an `/api/nodes` response, unfiltered.
pub fn parse_node_rows(json: &str) -> Result<Vec<NodeRow>, PotatoMeshError> {
    Ok(serde_json::from_str(json)?)
}

/// The kind a node list reports, as the nodeset tag writes it: MeshCore's
/// `repeater`, `room-server`, `companion`; Meshtastic's `router`, `client`,
/// `tracker`, `sensor`; any other role lower-cased with `-` for `_`, and
/// `unknown` when there is none.
pub fn node_kind(role: Option<&str>) -> String {
    let Some(role) = role.map(str::trim).filter(|r| !r.is_empty()) else {
        return "unknown".into();
    };
    match role.to_ascii_uppercase().as_str() {
        "REPEATER" => "repeater",
        "ROOM_SERVER" | "ROOM" => "room-server",
        "COMPANION" | "CHAT" => "companion",
        "ROUTER" | "ROUTER_LATE" | "ROUTER_CLIENT" => "router",
        "CLIENT" | "CLIENT_MUTE" | "CLIENT_HIDDEN" | "CLIENT_BASE" | "TAK" => "client",
        "TRACKER" | "TAK_TRACKER" | "LOST_AND_FOUND" => "tracker",
        "SENSOR" => "sensor",
        other => return other.to_ascii_lowercase().replace('_', "-"),
    }
    .into()
}

/// What to keep of an `/api/nodes` list.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeFilter {
    pub bbox: Option<crate::meshcore_map::MapBbox>,
    /// Keep MeshCore companions (people's handsets) as well.
    pub companions: bool,
    /// Drop nodes last heard more than this many days before `now`.
    pub max_age_days: Option<u32>,
    /// Drop nodes last heard more than this far after `now`: a clock that was
    /// never set, not a node.
    pub max_future_skew_days: Option<u32>,
}

impl Default for NodeFilter {
    fn default() -> Self {
        NodeFilter { bbox: None, companions: false, max_age_days: Some(90), max_future_skew_days: Some(7) }
    }
}

/// Where every row of the list went. Each row lands in exactly one bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct NodeReport {
    pub total_parsed: usize,
    pub dropped_wrong_kind: usize,
    pub dropped_no_position: usize,
    /// Position truncated on purpose (`precision_bits` < 30).
    pub dropped_truncated: usize,
    pub dropped_out_of_bbox: usize,
    pub dropped_stale: usize,
    pub dropped_no_timestamp: usize,
    pub dropped_future: usize,
    pub deduped: usize,
    pub kept: usize,
}

impl NodeReport {
    pub fn total_accounted(&self) -> usize {
        self.dropped_wrong_kind
            + self.dropped_no_position
            + self.dropped_truncated
            + self.dropped_out_of_bbox
            + self.dropped_stale
            + self.dropped_no_timestamp
            + self.dropped_future
            + self.deduped
            + self.kept
    }
}

impl NodeFilter {
    /// Apply the filter at a caller-supplied `now` (unix seconds), in the
    /// order kind, position, truncation, bbox, age; dedup by `node_id` last,
    /// the most recently heard row winning, output in first-seen order.
    pub fn apply(&self, rows: &[NodeRow], now_unix: i64) -> (Vec<NodeRow>, NodeReport) {
        let mut report = NodeReport { total_parsed: rows.len(), ..NodeReport::default() };
        let max_age = self.max_age_days.map(|d| i64::from(d) * 86_400);
        let max_skew = self.max_future_skew_days.map(|d| i64::from(d) * 86_400);
        let mut kept: Vec<NodeRow> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for r in rows {
            if !self.companions && node_kind(r.role.as_deref()) == "companion" {
                report.dropped_wrong_kind += 1;
                continue;
            }
            let Some(pos) = r.pos() else {
                report.dropped_no_position += 1;
                continue;
            };
            if r.quality() == PositionQuality::Truncated {
                report.dropped_truncated += 1;
                continue;
            }
            if self.bbox.is_some_and(|b| !b.contains(pos.lat_deg, pos.lon_deg)) {
                report.dropped_out_of_bbox += 1;
                continue;
            }
            match r.last_heard {
                Some(t) => {
                    let age = now_unix - t;
                    if max_skew.is_some_and(|s| -age > s) {
                        report.dropped_future += 1;
                        continue;
                    }
                    if max_age.is_some_and(|l| age > l) {
                        report.dropped_stale += 1;
                        continue;
                    }
                }
                None if max_age.is_some() => {
                    report.dropped_no_timestamp += 1;
                    continue;
                }
                None => {}
            }
            match seen.get(&r.node_id).copied() {
                Some(i) => {
                    report.deduped += 1;
                    if r.last_heard > kept[i].last_heard {
                        kept[i] = r.clone();
                    }
                }
                None => {
                    seen.insert(r.node_id.clone(), kept.len());
                    kept.push(r.clone());
                }
            }
        }
        report.kept = kept.len();
        (kept, report)
    }
}

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub pos: Option<GeoPos>,
    pub quality: PositionQuality,
}

/// Index /api/nodes by node_id, and numeric `num` → node_id for trace joins.
pub struct NodeIndex {
    pub by_id: HashMap<String, NodeInfo>,
    pub by_num: HashMap<i64, String>,
}

pub fn parse_nodes(json: &str) -> Result<NodeIndex, PotatoMeshError> {
    let rows = parse_node_rows(json)?;
    let mut by_id = HashMap::new();
    let mut by_num = HashMap::new();
    for r in rows {
        let info = NodeInfo { pos: r.pos(), quality: r.quality() };
        if let Some(num) = r.num {
            by_num.insert(num, r.node_id.clone());
        }
        by_id.insert(r.node_id, info);
    }
    Ok(NodeIndex { by_id, by_num })
}

#[derive(Debug, Deserialize)]
struct NeighborRow {
    node_id: String,
    neighbor_id: String,
    snr: Option<f32>,
    rx_time: Option<i64>,
}

/// /api/neighbors → observations (rx = reporter, tx = heard neighbor).
pub fn neighbors_to_observations(
    json: &str,
    nodes: &NodeIndex,
) -> Result<Vec<Observation>, PotatoMeshError> {
    let rows: Vec<NeighborRow> = serde_json::from_str(json)?;
    let look = |id: &str| nodes.by_id.get(id).cloned();
    Ok(rows
        .into_iter()
        .map(|r| {
            let rx = look(&r.node_id);
            let tx = look(&r.neighbor_id);
            Observation {
                time_unix: r.rx_time,
                tx_id: r.neighbor_id,
                rx_id: r.node_id,
                tx_pos: tx.as_ref().and_then(|n| n.pos),
                rx_pos: rx.as_ref().and_then(|n| n.pos),
                pos_quality: rx
                    .map(|n| n.quality)
                    .unwrap_or(PositionQuality::Unknown),
                snr_db: r.snr,
                rssi_dbm: None,
                freq_mhz: None,
                source: SourceTag::PotatoMesh,
            }
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct TraceRow {
    src: Option<i64>,
    dest: Option<i64>,
    rx_time: Option<i64>,
    rssi: Option<f32>,
    snr: Option<f32>,
}

/// /api/traces → observations. Row-level snr/rssi belong to the final hop as
/// heard requester-side (schema has no per-hop values) — flagged by keeping
/// tx/dest at trace endpoints; calibration should weight these lower.
pub fn traces_to_observations(
    json: &str,
    nodes: &NodeIndex,
) -> Result<Vec<Observation>, PotatoMeshError> {
    let rows: Vec<TraceRow> = serde_json::from_str(json)?;
    let id_of = |num: Option<i64>| -> Option<&String> { num.and_then(|n| nodes.by_num.get(&n)) };
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let src_id = id_of(r.src)?.clone();
            let dest_id = id_of(r.dest)?.clone();
            let src = nodes.by_id.get(&src_id);
            let dest = nodes.by_id.get(&dest_id);
            Some(Observation {
                time_unix: r.rx_time,
                tx_id: src_id.clone(),
                rx_id: dest_id,
                tx_pos: src.and_then(|n| n.pos),
                rx_pos: dest.and_then(|n| n.pos),
                pos_quality: dest
                    .map(|n| n.quality)
                    .unwrap_or(PositionQuality::Unknown),
                snr_db: r.snr,
                rssi_dbm: r.rssi,
                freq_mhz: None,
                source: SourceTag::PotatoMesh,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODES: &str = r#"[
      {"node_id":"!aa","num":11,"latitude":52.52,"longitude":13.40,"precision_bits":32,"location_source":"LOC_INTERNAL","protocol":"meshtastic"},
      {"node_id":"!bb","num":22,"latitude":52.53,"longitude":13.42,"precision_bits":13,"protocol":"meshcore"},
      {"node_id":"!cc","num":33,"latitude":52.50,"longitude":13.38,"location_source":"LOC_MANUAL"},
      {"node_id":"!dd","num":44}
    ]"#;

    /// potatomesh.net's `/api/nodes` shape: Meshtastic and MeshCore rows,
    /// a truncated manual position, a positionless companion, Munich, a
    /// stale repeater, Null Island.
    const FIXTURE: &str = include_str!("../tests/fixtures/potatomesh-nodes.json");
    /// 2026-09-27T12:38:44Z, the newest `last_heard` in the fixture.
    const FIXTURE_NOW: i64 = 1_790_512_724;
    const BERLIN: crate::meshcore_map::MapBbox = crate::meshcore_map::MapBbox {
        min_lon: 13.033,
        min_lat: 52.310,
        max_lon: 13.810,
        max_lat: 52.710,
    };

    #[test]
    fn the_node_filter_drops_truncated_positions_and_accounts_for_every_row() {
        let rows = parse_node_rows(FIXTURE).unwrap();
        assert_eq!(rows.len(), 11);
        let f = NodeFilter { bbox: Some(BERLIN), max_age_days: Some(365), ..NodeFilter::default() };
        let (kept, r) = f.apply(&rows, FIXTURE_NOW);
        assert_eq!(r.dropped_wrong_kind, 2, "two companions: {r:?}");
        assert_eq!(r.dropped_no_position, 1, "Null Island (the positionless one is a companion)");
        assert_eq!(r.dropped_truncated, 1, "precision_bits 16, even though hand-entered");
        assert_eq!(r.dropped_out_of_bbox, 1, "Munich");
        assert_eq!(r.dropped_stale, 1, "last heard in February 2025");
        assert_eq!(r.kept, 5);
        assert_eq!(r.total_accounted(), r.total_parsed);
        let ids: Vec<&str> = kept.iter().map(|n| n.node_id.as_str()).collect();
        assert_eq!(ids, ["!17dc0fa6", "!a1b2c3d4", "!31549ae1", "!0badf00d", "!27716218"]);
        let kinds: Vec<String> = kept.iter().map(|n| node_kind(n.role.as_deref())).collect();
        assert_eq!(kinds, ["router", "client", "repeater", "room-server", "node"]);
        let q: Vec<PositionQuality> = kept.iter().map(NodeRow::quality).collect();
        use PositionQuality::*;
        assert_eq!(q, [FixedSite, GpsFix, FixedSite, FixedSite, Unknown]);
        // The long name, the short one when the long is blank, emoji intact.
        assert_eq!(kept[0].label(), "Teehandlung Router");
        assert_eq!(kept[1].label(), "GPS1");
        assert_eq!(kept[2].label(), "Friedrichshain Repeater \u{2600}\u{fe0f}");

        let with_companions = NodeFilter { companions: true, ..f };
        let (_, r) = with_companions.apply(&rows, FIXTURE_NOW);
        assert_eq!((r.dropped_wrong_kind, r.dropped_no_position, r.kept), (0, 2, 6));
    }

    #[test]
    fn modem_presets_of_both_protocols_read_as_radio_settings() {
        let rows = parse_node_rows(FIXTURE).unwrap();
        assert_eq!(rows[0].radio(), Some(Radio { freq_mhz: 869.0, sf: 9, bw_khz: 250.0, cr: 5 }));
        assert_eq!(rows[2].radio(), Some(Radio { freq_mhz: 869.0, sf: 11, bw_khz: 250.0, cr: 5 }));
        assert_eq!(rows[3].radio(), Some(Radio { freq_mhz: 869.0, sf: 8, bw_khz: 62.5, cr: 8 }));
        assert_eq!(modem_preset("SF7/BW125/CR5"), Some((7, 125.0, 5)));
        assert_eq!(modem_preset("LongSlow"), Some((12, 125.0, 8)));
        assert_eq!(modem_preset("SF13/BW125/CR5"), None);
        assert_eq!(modem_preset("Custom"), None);
    }

    #[test]
    fn node_index_quality_tiers() {
        let idx = parse_nodes(NODES).unwrap();
        assert_eq!(idx.by_id["!aa"].quality, PositionQuality::GpsFix);
        assert_eq!(idx.by_id["!bb"].quality, PositionQuality::Truncated);
        assert_eq!(idx.by_id["!cc"].quality, PositionQuality::FixedSite);
        assert_eq!(idx.by_id["!dd"].quality, PositionQuality::Unknown);
        assert!(idx.by_id["!dd"].pos.is_none());
        assert_eq!(idx.by_num[&22], "!bb");
    }

    #[test]
    fn neighbors_become_rx_tx_observations() {
        let idx = parse_nodes(NODES).unwrap();
        let neighbors = r#"[
          {"node_id":"!aa","neighbor_id":"!cc","snr":6.5,"rx_time":1756500000,"protocol":"meshtastic"},
          {"node_id":"!bb","neighbor_id":"!aa","snr":-2.0,"rx_time":1756500060}
        ]"#;
        let obs = neighbors_to_observations(neighbors, &idx).unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].rx_id, "!aa");
        assert_eq!(obs[0].tx_id, "!cc");
        assert_eq!(obs[0].snr_db, Some(6.5));
        assert_eq!(obs[0].pos_quality, PositionQuality::GpsFix);
        assert!(obs[0].tx_pos.is_some() && obs[0].rx_pos.is_some());
        // Reporter with truncated position propagates its quality.
        assert_eq!(obs[1].pos_quality, PositionQuality::Truncated);
        assert_eq!(obs[1].source, SourceTag::PotatoMesh);
    }

    #[test]
    fn traces_join_through_num() {
        let idx = parse_nodes(NODES).unwrap();
        let traces = r#"[
          {"id":1,"request_id":9,"src":11,"dest":33,"rx_time":1756500100,"rx_iso":"x","rssi":-95,"snr":3.25,"elapsed_ms":800},
          {"id":2,"src":99,"dest":11,"rx_time":1756500200,"rx_iso":"x"}
        ]"#;
        let obs = traces_to_observations(traces, &idx).unwrap();
        assert_eq!(obs.len(), 1, "unknown num 99 is dropped");
        assert_eq!(obs[0].tx_id, "!aa");
        assert_eq!(obs[0].rx_id, "!cc");
        assert_eq!(obs[0].rssi_dbm, Some(-95.0));
        assert_eq!(obs[0].snr_db, Some(3.25));
        assert_eq!(obs[0].pos_quality, PositionQuality::FixedSite);
    }
}
