//! Road/rail overlay layer.
//!
//! Source: the OpenStreetMap extract the build is given (`osm.rs` selects the
//! ways; the compiler never talks to the network, same rule as the DEM
//! tiles). Stored PROJECTED into the pack CRS as f32 metres so the renderer
//! needs no per-frame reprojection; f32 gives ~0.5 m at UTM northings, far
//! below a display pixel.
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
    /// The class of a way from its `highway` and `railway` tags:
    /// motorway/trunk/primary/secondary and their `_link`s, and
    /// rail/light_rail/subway. Every other way is not on the layer.
    pub fn from_tag(highway: Option<&str>, railway: Option<&str>) -> Option<Self> {
        Some(match (highway, railway) {
            (Some("motorway"), _) | (Some("motorway_link"), _) => RoadClass::Motorway,
            (Some("trunk"), _) | (Some("trunk_link"), _) => RoadClass::Trunk,
            (Some("primary"), _) | (Some("primary_link"), _) => RoadClass::Primary,
            (Some("secondary"), _) | (Some("secondary_link"), _) => RoadClass::Secondary,
            (_, Some("rail" | "light_rail" | "subway")) => RoadClass::Rail,
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

    #[test]
    fn classes_follow_the_highway_and_railway_selection() {
        assert_eq!(RoadClass::from_tag(Some("motorway_link"), None), Some(RoadClass::Motorway));
        assert_eq!(RoadClass::from_tag(Some("secondary"), None), Some(RoadClass::Secondary));
        assert_eq!(RoadClass::from_tag(Some("tertiary"), None), None);
        assert_eq!(RoadClass::from_tag(Some("residential"), None), None);
        assert_eq!(RoadClass::from_tag(None, Some("subway")), Some(RoadClass::Rail));
        assert_eq!(RoadClass::from_tag(None, Some("light_rail")), Some(RoadClass::Rail));
        assert_eq!(RoadClass::from_tag(None, Some("tram")), None);
        assert_eq!(RoadClass::from_tag(None, Some("abandoned")), None);
        assert_eq!(RoadClass::from_tag(None, Some("platform")), None);
    }

    #[test]
    fn binary_roundtrip() {
        let ways = vec![
            Way { class: RoadClass::Motorway, points: vec![(13300.0, 52500.0), (13310.0, 52510.0)] },
            Way { class: RoadClass::Rail, points: vec![(13400.0, 52600.0), (13410.0, 52610.0)] },
        ];
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
