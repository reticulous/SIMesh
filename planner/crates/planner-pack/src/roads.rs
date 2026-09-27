//! Road/rail overlay layer.
//!
//! Source: an Overpass JSON export (fetched at pack-build time — the
//! compiler itself never talks to the network, same rule as the DEM tiles).
//! Stored PROJECTED into the pack CRS as f32 metres so the renderer needs no
//! per-frame reprojection; f32 gives ~0.5 m at UTM northings, far below a
//! display pixel.
//!
//! Binary format (little-endian):
//!   magic "PRD1" | u32 way_count
//!   per way: u8 class | u16 point_count | point_count × (f32 x, f32 y)

use crate::PackError;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoadClass {
    Motorway = 0,
    Trunk = 1,
    Primary = 2,
    Secondary = 3,
    Rail = 4,
}

impl RoadClass {
    pub fn from_tag(highway: Option<&str>, railway: Option<&str>) -> Option<Self> {
        Some(match (highway, railway) {
            (Some("motorway"), _) | (Some("motorway_link"), _) => RoadClass::Motorway,
            (Some("trunk"), _) | (Some("trunk_link"), _) => RoadClass::Trunk,
            (Some("primary"), _) | (Some("primary_link"), _) => RoadClass::Primary,
            (Some("secondary"), _) | (Some("secondary_link"), _) => RoadClass::Secondary,
            (_, Some(_)) => RoadClass::Rail,
            _ => return None,
        })
    }
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => RoadClass::Motorway,
            1 => RoadClass::Trunk,
            2 => RoadClass::Primary,
            3 => RoadClass::Secondary,
            4 => RoadClass::Rail,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Way {
    pub class: RoadClass,
    /// Projected pack-CRS metres.
    pub points: Vec<(f32, f32)>,
}

pub const OSM_NOTICE: &str =
    "Road and rail geometry \u{a9} OpenStreetMap contributors, ODbL 1.0 (opendatacommons.org/licenses/odbl)";

/// Parse an Overpass `out geom;` JSON export, projecting with `to_xy`.
/// Deliberately a hand-rolled scan rather than a JSON dependency: the export
/// is ~36 MB of mostly-tags and we want only geometry and one tag each.
pub fn parse_overpass(
    json: &str,
    mut to_xy: impl FnMut(f64, f64) -> (f64, f64),
) -> Vec<Way> {
    let mut ways = Vec::new();
    // Elements are separated by `"type": "way"`; within one element we take
    // the highway/railway tag and the geometry array.
    for chunk in json.split("\"type\":").skip(1) {
        let highway = extract_str(chunk, "\"highway\":");
        let railway = extract_str(chunk, "\"railway\":");
        let Some(class) = RoadClass::from_tag(highway.as_deref(), railway.as_deref()) else {
            continue;
        };
        let Some(geo_start) = chunk.find("\"geometry\":") else { continue };
        let geo = &chunk[geo_start..];
        let Some(end) = geo.find(']') else { continue };
        let mut points = Vec::new();
        for pt in geo[..end].split('{').skip(1) {
            let (Some(lat), Some(lon)) = (extract_num(pt, "\"lat\":"), extract_num(pt, "\"lon\":"))
            else {
                continue;
            };
            let (x, y) = to_xy(lat, lon);
            points.push((x as f32, y as f32));
        }
        if points.len() >= 2 {
            ways.push(Way { class, points });
        }
    }
    ways
}

fn extract_str(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_num(s: &str, key: &str) -> Option<f64> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == '+'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub fn write_binary<W: Write>(w: &mut W, ways: &[Way]) -> Result<(), PackError> {
    w.write_all(b"PRD1")?;
    w.write_all(&(ways.len() as u32).to_le_bytes())?;
    for way in ways {
        w.write_all(&[way.class as u8])?;
        let n = way.points.len().min(u16::MAX as usize) as u16;
        w.write_all(&n.to_le_bytes())?;
        for (x, y) in way.points.iter().take(n as usize) {
            w.write_all(&x.to_le_bytes())?;
            w.write_all(&y.to_le_bytes())?;
        }
    }
    Ok(())
}

pub fn read_binary<R: Read>(r: &mut R) -> Result<Vec<Way>, PackError> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"PRD1" {
        return Err(PackError::Invalid("roads: bad magic".into()));
    }
    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4)?;
    let count = u32::from_le_bytes(buf4) as usize;
    let mut ways = Vec::with_capacity(count);
    for _ in 0..count {
        let mut b1 = [0u8; 1];
        r.read_exact(&mut b1)?;
        let Some(class) = RoadClass::from_code(b1[0]) else {
            return Err(PackError::Invalid("roads: bad class".into()));
        };
        let mut b2 = [0u8; 2];
        r.read_exact(&mut b2)?;
        let n = u16::from_le_bytes(b2) as usize;
        let mut points = Vec::with_capacity(n);
        for _ in 0..n {
            r.read_exact(&mut buf4)?;
            let x = f32::from_le_bytes(buf4);
            r.read_exact(&mut buf4)?;
            let y = f32::from_le_bytes(buf4);
            points.push((x, y));
        }
        ways.push(Way { class, points });
    }
    Ok(ways)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"elements":[
      {"type":"way","id":1,"tags":{"highway":"motorway","ref":"A100"},
       "geometry":[{"lat":52.5,"lon":13.3},{"lat":52.51,"lon":13.31}]},
      {"type":"way","id":2,"tags":{"highway":"residential"},
       "geometry":[{"lat":52.4,"lon":13.2},{"lat":52.41,"lon":13.21}]},
      {"type":"way","id":3,"tags":{"railway":"rail","usage":"main"},
       "geometry":[{"lat":52.6,"lon":13.4},{"lat":52.61,"lon":13.41}]}
    ]}"#;

    #[test]
    fn parses_classes_and_skips_minor_roads() {
        // Identity "projection" so the test checks parsing, not proj.
        let ways = parse_overpass(SAMPLE, |lat, lon| (lon, lat));
        assert_eq!(ways.len(), 2, "residential must be skipped");
        assert_eq!(ways[0].class, RoadClass::Motorway);
        assert_eq!(ways[1].class, RoadClass::Rail);
        assert_eq!(ways[0].points.len(), 2);
        assert!((ways[0].points[0].0 - 13.3).abs() < 1e-4);
        assert!((ways[0].points[0].1 - 52.5).abs() < 1e-4);
    }

    #[test]
    fn binary_roundtrip() {
        let ways = parse_overpass(SAMPLE, |lat, lon| (lon * 1000.0, lat * 1000.0));
        let mut buf = Vec::new();
        write_binary(&mut buf, &ways).unwrap();
        let back = read_binary(&mut &buf[..]).unwrap();
        assert_eq!(back.len(), ways.len());
        assert_eq!(back[0].class, ways[0].class);
        assert_eq!(back[0].points.len(), ways[0].points.len());
        assert!((back[0].points[1].0 - ways[0].points[1].0).abs() < 1e-3);
        // Compact: 2 short ways well under 200 bytes.
        assert!(buf.len() < 200, "{} bytes", buf.len());
    }

    #[test]
    fn rejects_foreign_data() {
        assert!(read_binary(&mut &b"XXXX"[..]).is_err());
    }
}
