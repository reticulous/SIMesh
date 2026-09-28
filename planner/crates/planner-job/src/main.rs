//! planner-job: the front's batch jobs, one per process.
//!
//! ```text
//! front ── spawn `planner-job <job>`, one JSON object on stdin ─► planner-job
//! planner-job ── JSON lines on stdout (progress, then the result) ─► front
//! planner-job ── diagnostics on stderr
//! ```
//!
//! `pack-build` builds a pack from files already on disk (the compiler never
//! fetches); `nodes-import` turns a saved node-list response into nodes. On
//! success the process exits 0 and the last line is the result; on failure it
//! exits non-zero and the last line is `{"error": "<one sentence>"}`.

use planner_import::meshcore_map::{self, MapBbox, MapFilter, MeshCoreKind};
use planner_import::potatomesh::{self, NodeFilter};
use planner_import::PositionQuality;
use planner_pack::build::{build, BuildParams, Progress};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;

/// Write one line to stdout and flush it, so the front sees it at once.
fn emit(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// A message as one line.
fn sentence(s: &str) -> String {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("; ")
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        eprintln!("planner-job: panic{at}: {msg}");
        emit(&json!({ "error": sentence(&format!("internal error: {msg}")) }));
    }));
    let job = std::env::args().nth(1).unwrap_or_default();
    let result = match job.as_str() {
        "pack-build" | "nodes-import" => {
            let mut input = String::new();
            match std::io::stdin().read_to_string(&mut input) {
                Err(e) => Err(format!("cannot read the job from standard input: {e}")),
                Ok(_) if job == "pack-build" => pack_build(&input),
                Ok(_) => nodes_import(&input),
            }
        }
        _ => Err(format!(
            "unknown job '{job}': run `planner-job pack-build` or `planner-job nodes-import` with the job as JSON on standard input"
        )),
    };
    match result {
        Ok(last) => emit(&last),
        Err(e) => {
            eprintln!("planner-job {job}: {e}");
            emit(&json!({ "error": sentence(&e) }));
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// pack-build
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PackBuild {
    name: String,
    out_dir: PathBuf,
    /// [min_lon, min_lat, max_lon, max_lat]
    bbox: [f64; 4],
    res_m: f64,
    utm_zone: u8,
    dsm_tiles: Vec<PathBuf>,
    #[serde(default)]
    worldcover_tiles: Vec<PathBuf>,
    #[serde(default)]
    itu_maps_dir: Option<PathBuf>,
    #[serde(default)]
    osm_pbf: Option<PathBuf>,
    /// LoD2 buildings from this directory's tiles, where they cover the grid.
    #[serde(default)]
    lod2_dir: Option<PathBuf>,
    /// OpenStreetMap buildings from `osm_pbf`, where LoD2 does not cover.
    #[serde(default)]
    osm_buildings: bool,
    #[serde(default)]
    berlin_1m_dir: Option<PathBuf>,
    #[serde(default)]
    zensus_csv: Option<PathBuf>,
    #[serde(default)]
    threads: usize,
}

fn pack_build(input: &str) -> Result<Value, String> {
    let job: PackBuild =
        serde_json::from_str(input).map_err(|e| format!("the pack-build job is not valid: {e}"))?;
    let [min_lon, min_lat, max_lon, max_lat] = job.bbox;
    if !(min_lon < max_lon && min_lat < max_lat && min_lat >= -80.0 && max_lat <= 84.0) {
        return Err(format!("bbox {:?} is empty, inverted or outside the UTM latitudes", job.bbox));
    }
    if !(job.res_m.is_finite() && job.res_m > 0.0) {
        return Err(format!("res_m {} is not a positive number of metres", job.res_m));
    }
    if !(1..=60).contains(&job.utm_zone) {
        return Err(format!("utm_zone {} is not a UTM zone (1 to 60)", job.utm_zone));
    }
    if job.name.trim().is_empty() {
        return Err("the pack has no name".into());
    }
    if job.osm_buildings && job.osm_pbf.is_none() {
        return Err("osm_buildings needs osm_pbf".into());
    }
    let params = BuildParams {
        dsm_tiles: job.dsm_tiles,
        center_lat_deg: (min_lat + max_lat) / 2.0,
        center_lon_deg: (min_lon + max_lon) / 2.0,
        half_km: 0.0,
        bbox_wgs84: Some(job.bbox),
        res_m: job.res_m,
        utm_zone: job.utm_zone,
        out_dir: job.out_dir.clone(),
        name: job.name,
        itu_maps_dir: job.itu_maps_dir,
        berlin_1m_dir: job.berlin_1m_dir,
        lod2_dir: job.lod2_dir,
        lod2_geometry: true,
        zensus_csv: job.zensus_csv,
        worldcover_tiles: job.worldcover_tiles,
        osm_pbf: job.osm_pbf,
        osm_buildings: job.osm_buildings,
        threads: job.threads,
        progress: Some(Box::new(|p: &Progress| {
            let mut line = json!({ "step": p.step, "done": p.done, "total": p.total });
            if let Some((part, parts)) = p.part {
                line["part"] = json!(part);
                line["parts"] = json!(parts);
            }
            emit(&line);
        })),
    };
    let t0 = std::time::Instant::now();
    build(&params).map_err(|e| e.to_string())?;
    eprintln!("pack build: done in {:.1} s", t0.elapsed().as_secs_f64());
    Ok(json!({ "manifest": job.out_dir.join("manifest.json") }))
}

// ---------------------------------------------------------------------------
// nodes-import
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Source {
    Meshcore,
    Potatomesh,
}

#[derive(Debug, Deserialize)]
struct NodesImport {
    source: Source,
    file: PathBuf,
    /// [min_lon, min_lat, max_lon, max_lat]
    bbox: [f64; 4],
    #[serde(default)]
    companions: bool,
    /// Adverts (MeshCore) or last-heard times (PotatoMesh) older than this
    /// are left out; null keeps every age.
    #[serde(default)]
    max_age_days: Option<u32>,
    /// The clock the ages are measured against; the system clock when absent.
    #[serde(default)]
    now_unix: Option<i64>,
}

/// Adverts dated further ahead than this are a clock that was never set.
const MAX_FUTURE_SKEW_DAYS: u32 = 7;

fn position(q: PositionQuality) -> &'static str {
    match q {
        PositionQuality::GpsFix => "gps",
        PositionQuality::FixedSite => "fixed",
        PositionQuality::Truncated => "truncated",
        PositionQuality::Unknown => "unknown",
    }
}

fn meshcore_kind(k: MeshCoreKind) -> &'static str {
    match k {
        MeshCoreKind::Companion => "companion",
        MeshCoreKind::Repeater => "repeater",
        MeshCoreKind::RoomServer => "room-server",
        MeshCoreKind::Sensor => "sensor",
        MeshCoreKind::Other(_) => "other",
    }
}

fn radio(freq_mhz: f64, sf: u8, bw_khz: f64, cr: u8) -> Value {
    json!({ "freq_mhz": freq_mhz, "sf": sf, "bw_khz": bw_khz, "cr": cr })
}

fn nodes_import(input: &str) -> Result<Value, String> {
    let job: NodesImport =
        serde_json::from_str(input).map_err(|e| format!("the nodes-import job is not valid: {e}"))?;
    let [min_lon, min_lat, max_lon, max_lat] = job.bbox;
    if !(min_lon < max_lon && min_lat < max_lat) {
        return Err(format!("bbox {:?} is empty or inverted", job.bbox));
    }
    let bbox = MapBbox::new(min_lon, min_lat, max_lon, max_lat);
    let now = job.now_unix.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });
    let text = std::fs::read_to_string(&job.file)
        .map_err(|e| format!("cannot read {}: {e}", job.file.display()))?;
    let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
    let (nodes, report): (Vec<Value>, Value) = match job.source {
        Source::Meshcore => {
            let mut kinds = vec![MeshCoreKind::Repeater, MeshCoreKind::RoomServer];
            if job.companions {
                kinds.push(MeshCoreKind::Companion);
            }
            let filter = MapFilter {
                bbox: Some(bbox),
                kinds,
                max_age_days: job.max_age_days,
                max_future_skew_days: Some(MAX_FUTURE_SKEW_DAYS),
                require_position: true,
            };
            let (kept, report) = meshcore_map::load_map(&text, &filter, now)
                .map_err(|e| format!("{} is not a MeshCore map node list: {e}", job.file.display()))?;
            eprintln!("{report}");
            let nodes = kept
                .iter()
                .filter_map(|n| {
                    let p = n.pos()?;
                    let kind = meshcore_kind(n.kind);
                    *by_kind.entry(kind.into()).or_default() += 1;
                    let mut v = json!({
                        "label": n.name,
                        "source_id": n.public_key,
                        "lat": p.lat_deg,
                        "lon": p.lon_deg,
                        "kind": kind,
                        "position": position(n.pos_quality),
                        "height_m": null,
                        "last_heard_unix": n.last_advert_unix,
                    });
                    if let (Some(f), Some(sf), Some(bw), Some(cr)) = (n.freq_mhz, n.sf, n.bw_khz, n.cr) {
                        v["radio"] = radio(f, sf, bw, cr);
                    }
                    Some(v)
                })
                .collect();
            (nodes, serde_json::to_value(report).map_err(|e| e.to_string())?)
        }
        Source::Potatomesh => {
            let rows = potatomesh::parse_node_rows(&text)
                .map_err(|e| format!("{} is not a PotatoMesh /api/nodes list: {e}", job.file.display()))?;
            let filter = NodeFilter {
                bbox: Some(bbox),
                companions: job.companions,
                max_age_days: job.max_age_days,
                max_future_skew_days: Some(MAX_FUTURE_SKEW_DAYS),
            };
            let (kept, report) = filter.apply(&rows, now);
            eprintln!("potatomesh: {report:?}");
            let nodes = kept
                .iter()
                .filter_map(|n| {
                    let p = n.pos()?;
                    let kind = potatomesh::node_kind(n.role.as_deref());
                    *by_kind.entry(kind.clone()).or_default() += 1;
                    let mut v = json!({
                        "label": n.label(),
                        "source_id": n.node_id,
                        "lat": p.lat_deg,
                        "lon": p.lon_deg,
                        "kind": kind,
                        "position": position(n.quality()),
                        "height_m": null,
                        "last_heard_unix": n.last_heard,
                    });
                    if let Some(protocol) = &n.protocol {
                        v["protocol"] = json!(protocol);
                    }
                    if let Some(r) = n.radio() {
                        v["radio"] = radio(r.freq_mhz, r.sf, r.bw_khz, r.cr);
                    }
                    Some(v)
                })
                .collect();
            (nodes, serde_json::to_value(report).map_err(|e| e.to_string())?)
        }
    };
    let mut report = report;
    report["kept_by_kind"] = json!(by_kind);
    Ok(json!({ "nodes": nodes, "report": report }))
}
