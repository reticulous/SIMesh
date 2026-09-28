//! The stdin/stdout contract of `planner-job`, run as the front runs it.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Run a job; returns (exit ok, stdout lines as JSON).
fn run(job: &str, input: &Value) -> (bool, Vec<Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_planner-job"))
        .arg(job)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("planner-job runs");
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let lines = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
        .collect();
    (out.status.success(), lines)
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn cache() -> PathBuf {
    repo().join("packs/.cache")
}

#[test]
fn an_unknown_job_fails_with_an_error_line() {
    let (ok, lines) = run("fly", &json!({}));
    assert!(!ok);
    assert!(lines.last().unwrap()["error"].as_str().unwrap().contains("unknown job"));
}

#[test]
fn a_bad_pack_build_names_the_problem_in_one_sentence() {
    let (ok, lines) = run(
        "pack-build",
        &json!({"name": "x", "out_dir": "/nonexistent/x", "bbox": [13.38, 52.51, 13.42, 52.53],
                "res_m": 30, "utm_zone": 33, "dsm_tiles": [], "osm_buildings": true}),
    );
    assert!(!ok);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["error"], "osm_buildings needs osm_pbf");

    let (ok, lines) = run(
        "pack-build",
        &json!({"name": "x", "out_dir": "/nonexistent/x", "bbox": [13.38, 52.51, 13.42, 52.53],
                "res_m": 30, "utm_zone": 33, "dsm_tiles": ["/nonexistent/a.tif"],
                "zensus_csv": "/nonexistent/z.csv"}),
    );
    assert!(!ok);
    let e = lines.last().unwrap()["error"].as_str().unwrap();
    assert!(!e.contains('\n') && e.contains("/nonexistent/a.tif") && e.contains("/nonexistent/z.csv"), "{e}");
}

#[test]
fn potatomesh_nodes_import_from_the_saved_fixture() {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../planner-import/tests/fixtures/potatomesh-nodes.json");
    let (ok, lines) = run(
        "nodes-import",
        &json!({"source": "potatomesh", "file": file, "bbox": [13.033, 52.310, 13.810, 52.710],
                "companions": false, "max_age_days": 365, "now_unix": 1_790_512_724i64}),
    );
    assert!(ok, "{lines:?}");
    let last = lines.last().unwrap();
    let nodes = last["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 5);
    let r = &last["report"];
    assert_eq!(r["dropped_truncated"], 1);
    assert_eq!(r["kept"], 5);
    assert_eq!(r["kept_by_kind"]["repeater"], 1);
    let rep = nodes.iter().find(|n| n["kind"] == "repeater").unwrap();
    assert_eq!(rep["source_id"], "!31549ae1");
    assert_eq!(rep["label"], "Friedrichshain Repeater \u{2600}\u{fe0f}");
    assert_eq!(rep["position"], "fixed");
    assert_eq!(rep["radio"], json!({"freq_mhz": 869.0, "sf": 8, "bw_khz": 62.5, "cr": 8}));
    assert!(rep["height_m"].is_null());
    assert_eq!(nodes.iter().find(|n| n["kind"] == "client").unwrap()["position"], "gps");
}

/// The saved MeshCore map: `SIMESH_MESHCORE_SNAPSHOT`, else the front's
/// download cache. Skips when neither is there.
#[test]
fn meshcore_nodes_import_from_the_saved_map() {
    let file = std::env::var_os("SIMESH_MESHCORE_SNAPSHOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache().join("meshcore/nodes.json"));
    if !file.exists() {
        eprintln!("SKIP: no saved MeshCore map ({})", file.display());
        return;
    }
    let job = |companions: bool, max_age: Value| {
        json!({"source": "meshcore", "file": file, "bbox": [13.033, 52.310, 13.810, 52.710],
               "companions": companions, "max_age_days": max_age, "now_unix": 1_790_000_000i64})
    };
    // Every age: the box's repeaters and room servers, ~395 and ~36.
    let (ok, lines) = run("nodes-import", &job(false, Value::Null));
    assert!(ok);
    let r = &lines.last().unwrap()["report"];
    let rep = r["kept_by_kind"]["repeater"].as_u64().unwrap();
    let room = r["kept_by_kind"]["room-server"].as_u64().unwrap();
    assert!((300..500).contains(&rep) && (15..80).contains(&room), "{r}");
    assert!(r["kept_by_kind"].get("companion").is_none());
    // Companions too, ~69.
    let (_, lines) = run("nodes-import", &job(true, Value::Null));
    let comp = lines.last().unwrap()["report"]["kept_by_kind"]["companion"].as_u64().unwrap();
    assert!((30..150).contains(&comp), "{comp}");
    // A year's age limit leaves fewer, and says why.
    let (_, lines) = run("nodes-import", &job(false, json!(365)));
    let r = &lines.last().unwrap()["report"];
    assert!(r["kept"].as_u64().unwrap() < rep + room, "{r}");
    assert!(r["dropped_stale"].as_u64().unwrap() > 0, "{r}");
    let n = &lines.last().unwrap()["nodes"][0];
    for k in ["label", "source_id", "lat", "lon", "kind", "position"] {
        assert!(n.get(k).is_some(), "{k} missing: {n}");
    }
    assert_eq!(n["source_id"].as_str().unwrap().len(), 64);
}

fn manifest_of(lines: &[Value]) -> Value {
    let path = lines.last().unwrap()["manifest"].as_str().expect("manifest line").to_string();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn kinds(m: &Value, key: &str, field: &str) -> Vec<String> {
    m[key].as_array().unwrap().iter().map(|l| l[field].as_str().unwrap().to_string()).collect()
}

/// The LoD2 a build is given: none, the whole cached set, or only the tiles
/// of one kilometre column (east 390 km), so OpenStreetMap has the rest.
fn lod2_input(buildings: &str, all: &Path, out: &Path) -> Value {
    match buildings {
        "lod2" => json!(all),
        "both" => {
            let dir = out.with_extension("lod2");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for f in std::fs::read_dir(all).unwrap().flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                if name.starts_with("LoD2_33_390_") {
                    std::os::unix::fs::symlink(f.path(), dir.join(&name)).unwrap();
                }
            }
            json!(dir)
        }
        _ => Value::Null,
    }
}

fn small_build(buildings: &str, out: &Path) -> Option<(Vec<Value>, Value)> {
    let c = cache();
    let inputs = [
        c.join("glo30/Copernicus_DSM_COG_10_N52_00_E013_00_DEM.tif"),
        c.join("worldcover/ESA_WorldCover_10m_2021_v200_N51E012_Map.tif"),
        c.join("itu/DN50.TXT"),
        c.join("geofabrik/berlin-latest.osm.pbf"),
        c.join("zensus/Zensus2022_Bevoelkerungszahl_100m-Gitter.csv"),
        c.join("berlin-lod2"),
    ];
    if let Some(missing) = inputs.iter().find(|p| !p.exists()) {
        eprintln!("SKIP: {} not in the download cache", missing.display());
        return None;
    }
    let (ok, lines) = run(
        "pack-build",
        &json!({
            "name": format!("mitte-{buildings}"), "out_dir": out,
            "bbox": [13.38, 52.51, 13.42, 52.53], "res_m": 30, "utm_zone": 33,
            "dsm_tiles": [inputs[0]], "worldcover_tiles": [inputs[1]],
            "itu_maps_dir": c.join("itu"), "osm_pbf": inputs[3],
            "osm_buildings": buildings != "lod2",
            "lod2_dir": lod2_input(buildings, &inputs[5], out),
            "berlin_1m_dir": null, "zensus_csv": inputs[4], "threads": 0
        }),
    );
    assert!(ok, "{lines:?}");
    Some((lines.clone(), manifest_of(&lines)))
}

/// A few square kilometres of Berlin from the real inputs in the download
/// cache: with OpenStreetMap buildings, with LoD2, and with LoD2 on one
/// kilometre column and OpenStreetMap on the rest. Slow in a debug build:
/// `cargo test --release -p planner-job -- --ignored`.
#[test]
#[ignore = "reads the real inputs in packs/.cache; run with --release -- --ignored"]
fn small_real_builds_with_osm_lod2_and_both_buildings() {
    let base = std::env::temp_dir().join("planner_job_real_builds");
    let common_layers = [
        "TerrainDtm", "ClutterHeight", "Buildings", "BuiltFraction", "BuildingTop", "Population",
        "ClutterClass", "DataQuality", "Roads", "Places",
    ];
    for buildings in ["osm", "lod2", "both"] {
        let out = base.join(buildings);
        let t0 = std::time::Instant::now();
        let Some((lines, m)) = small_build(buildings, &out) else { return };
        let secs = t0.elapsed().as_secs_f64();
        // Progress: every step starts and ends, `done` counting up to `total`.
        let steps: Vec<&Value> = lines.iter().filter(|l| l.get("step").is_some()).collect();
        let total = steps[0]["total"].as_u64().unwrap();
        assert_eq!(total, 7, "terrain osm buildings landcover clutter population manifest");
        assert_eq!(steps.last().unwrap()["done"], total);
        let names: Vec<&str> = steps
            .iter()
            .filter(|l| l.get("part").is_none())
            .map(|l| l["step"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "terrain", "terrain", "osm", "osm", "buildings", "buildings", "landcover",
                "landcover", "clutter", "clutter", "population", "population", "manifest",
                "manifest"
            ]
        );
        assert_eq!(kinds(&m, "layers", "kind"), common_layers);
        let lic = kinds(&m, "licenses", "source");
        assert!(lic.contains(&"OpenStreetMap roads/rail".to_string()), "{lic:?}");
        assert!(lic.contains(&"OpenStreetMap places/streets/postal codes".to_string()), "{lic:?}");
        let osm_b = lic.contains(&"OpenStreetMap buildings".to_string());
        let lod2 = lic.contains(&"Berlin LoD2 3D building models".to_string());
        assert_eq!((osm_b, lod2), (buildings != "lod2", buildings != "osm"), "{lic:?}");
        assert!(!lic.iter().any(|l| l.contains("nodes")), "no nodes in a pack: {lic:?}");
        // Every sidecar line carries rings and a height source; with both,
        // LoD2 holds the 390 km column and OpenStreetMap the rest.
        let jsonl = std::fs::read_to_string(out.join("buildings.jsonl")).unwrap();
        let (mut n, mut n_lod2) = (0, 0);
        for line in jsonl.lines() {
            let b: Value = serde_json::from_str(line).unwrap();
            assert!(b["rings"][0]["exterior"].as_array().unwrap().len() >= 4);
            let src = b["source"].as_str().unwrap();
            let in_column = (390_000.0..391_000.0).contains(&b["e"].as_f64().unwrap());
            if src == "lod2" {
                assert_ne!(buildings, "osm");
                n_lod2 += 1;
            } else {
                assert_ne!(buildings, "lod2");
                assert!(["osm_height", "osm_levels", "default"].contains(&src), "{src}");
                assert!(b["ground_z"].as_f64().unwrap() > 20.0, "{b}");
                assert!(buildings == "osm" || !in_column, "an OSM building on LoD2's ground: {b}");
            }
            n += 1;
        }
        if buildings == "both" {
            assert!(n_lod2 > 100 && n - n_lod2 > 100, "{n_lod2} LoD2 of {n}");
        }
        let size: u64 = std::fs::read_dir(&out).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum();
        println!("{buildings}: {n} buildings, {size} bytes, {secs:.1} s, layers {:?}", kinds(&m, "layers", "kind"));
        assert!(n > 1000, "{n} buildings in central Berlin");
    }
}
