//! LoD2 CityGML ingestion (Berlin ATOM: 1×1 km tiles, dl-de/zero-2.0;
//! same CityGML 2.0 shape as the other Länder feeds).
//!
//! Verified 2026-08-31 against LoD2_33_392_5820_1_BE.xml: `bldg:Building` and
//! `bldg:BuildingPart` each carry `bldg:measuredHeight` (meters) and one or
//! more `bldg:GroundSurface` rings as `gml:posList srsDimension="3"` E N Z
//! triplets in EPSG:25833. Every Building/BuildingPart with its own ground
//! surface + height becomes one record; parents that only aggregate parts
//! contribute nothing themselves.

use crate::PackError;
use planner_buildings::HeightSource;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use std::io::BufRead;

/// One ground-surface polygon: an outer ring and the courtyards inside it.
///
/// The distinction is not cosmetic. CityGML marks a courtyard as
/// `gml:interior` (OpenStreetMap as a multipolygon's `inner` member); read as
/// an ordinary outer ring, its area is ADDED to the footprint instead of
/// subtracted and `rasterize_building` fills it solid. On the Berlin tile
/// LoD2_33_392_5820, 12 of 1419 ground surfaces carry a hole, 7 688 m² of
/// courtyard against 274 426 m² of building.
///
/// A Blockrand courtyard is open sky where people and nodes actually are. It
/// is the last place that should be modelled as masonry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Polygon {
    pub exterior: Vec<(f64, f64)>,
    /// Courtyards. Subtracted from the area, punched out of the raster.
    #[serde(default)]
    pub interiors: Vec<Vec<(f64, f64)>>,
}

/// One `buildings.jsonl` record, from LoD2 or from OpenStreetMap (`osm.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lod2Building {
    /// The LoD2 `gml:id`, or `w<id>` / `r<id>` for an OpenStreetMap way or
    /// multipolygon relation.
    pub id: String,
    /// Area-weighted footprint centroid in the pack CRS (LoD2's EPSG:25833
    /// is taken as UTM 33 within a metre).
    pub e: f64,
    pub n: f64,
    /// Mean ground elevation (m amsl) of the footprint rings; for an
    /// OpenStreetMap building, the pack terrain under its centroid.
    pub ground_z: f64,
    /// Net footprint: outer rings MINUS courtyards.
    pub area_m2: f64,
    /// Height above ground to the top of the roof: bldg:measuredHeight, or
    /// what the OpenStreetMap tags give (`source` says which).
    pub height_m: f64,
    /// Where `height_m` came from.
    #[serde(default = "lod2_source")]
    pub source: HeightSource,
    /// Ground-surface polygons (E,N).
    ///
    /// Serialized only when the caller asks for it — see
    /// [`Lod2Building::to_json_line`]. A full-city sidecar with geometry is
    /// about 300 MB against 137 MB without, so which one a pack carries is a
    /// build decision rather than something this type should force.
    #[serde(skip_serializing, default)]
    pub rings: Vec<Polygon>,
}

fn lod2_source() -> HeightSource {
    HeightSource::Lod2
}

impl Lod2Building {
    /// The record as one `buildings.jsonl` line.
    ///
    /// `with_geometry` decides whether the footprint polygons ride along. The
    /// centroid-only fields are what the server's `BuildingRecord` requires;
    /// the geometry is additive, so a reader ignoring it still parses the
    /// line.
    pub fn to_json_line(&self, with_geometry: bool) -> Result<String, PackError> {
        let mut v = serde_json::to_value(self)
            .map_err(|e| PackError::Invalid(format!("building {}: {e}", self.id)))?;
        if with_geometry {
            if let Some(o) = v.as_object_mut() {
                o.insert(
                    "rings".into(),
                    serde_json::to_value(&self.rings)
                        .map_err(|e| PackError::Invalid(format!("rings {}: {e}", self.id)))?,
                );
            }
        }
        serde_json::to_string(&v)
            .map_err(|e| PackError::Invalid(format!("building {}: {e}", self.id)))
    }
}

#[derive(Default)]
struct Ctx {
    id: String,
    height: Option<f64>,
    rings: Vec<Polygon>,
    ground_z_sum: f64,
    ground_z_n: usize,
    in_ground: bool,
}

/// Shoelace area (absolute) and centroid of one ring.
fn ring_area_centroid(ring: &[(f64, f64)]) -> (f64, f64, f64) {
    let n = ring.len();
    if n < 3 {
        return (0.0, 0.0, 0.0);
    }
    let (mut a2, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x0, y0) = ring[i];
        let (x1, y1) = ring[(i + 1) % n];
        let cross = x0 * y1 - x1 * y0;
        a2 += cross;
        cx += (x0 + x1) * cross;
        cy += (y0 + y1) * cross;
    }
    if a2.abs() < 1e-9 {
        return (0.0, ring[0].0, ring[0].1);
    }
    (a2.abs() / 2.0, cx / (3.0 * a2), cy / (3.0 * a2))
}

/// Net area and area-weighted centroid `(area, e, n)` of a footprint.
///
/// Courtyards are SUBTRACTED, from both the area and the centroid moment:
/// adding them reports a ring block as more building than it is and drags the
/// centroid toward the middle of the yard — the one point in the block that
/// has no building on it. The centroid is meaningless when the area is below
/// a square metre.
pub fn footprint(rings: &[Polygon]) -> (f64, f64, f64) {
    let (mut area, mut cxw, mut cyw) = (0.0, 0.0, 0.0);
    for p in rings {
        let (a, cx, cy) = ring_area_centroid(&p.exterior);
        area += a;
        cxw += cx * a;
        cyw += cy * a;
        for hole in &p.interiors {
            let (ha, hx, hy) = ring_area_centroid(hole);
            area -= ha;
            cxw -= hx * ha;
            cyw -= hy * ha;
        }
    }
    if area < 1.0 {
        return (area, 0.0, 0.0);
    }
    (area, cxw / area, cyw / area)
}

/// The extent `[min_e, min_n, max_e, max_n]` of a building's outlines.
pub fn extent(b: &Lod2Building) -> [f64; 4] {
    let mut r = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in &b.rings {
        for &(x, y) in &p.exterior {
            r = [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)];
        }
    }
    r
}

fn finish(ctx: Ctx, out: &mut Vec<Lod2Building>) {
    let Some(height) = ctx.height else { return };
    if ctx.rings.is_empty() || height <= 0.0 {
        return;
    }
    let (area, e, n) = footprint(&ctx.rings);
    if area < 1.0 {
        return; // degenerate slivers, and yards that swallowed their building
    }
    out.push(Lod2Building {
        id: ctx.id,
        e,
        n,
        ground_z: if ctx.ground_z_n > 0 { ctx.ground_z_sum / ctx.ground_z_n as f64 } else { 0.0 },
        area_m2: area,
        height_m: height,
        source: HeightSource::Lod2,
        rings: ctx.rings,
    });
}

/// The 1 km tile a Berlin LoD2 file name holds, `LoD2_33_<E km>_<N km>_…`,
/// as its extent in metres. `None` for a name in any other form.
pub fn tile_extent(file_name: &str) -> Option<[f64; 4]> {
    let mut parts = file_name.strip_prefix("LoD2_")?.split('_');
    let _zone = parts.next()?;
    let e: f64 = parts.next()?.parse().ok()?;
    let n: f64 = parts.next()?.parse().ok()?;
    Some([e * 1000.0, n * 1000.0, (e + 1.0) * 1000.0, (n + 1.0) * 1000.0])
}

/// Streaming parse of one CityGML file.
pub fn parse_citygml<R: BufRead>(reader: R) -> Result<Vec<Lod2Building>, PackError> {
    let mut xml = Reader::from_reader(reader);
    xml.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    // Nested Building → BuildingPart contexts.
    let mut stack: Vec<Ctx> = Vec::new();
    let mut in_height = false;
    let mut in_poslist = false;
    // Which ring of a polygon we are inside. GML nests as
    // Polygon > (exterior|interior) > LinearRing > posList, so this is the
    // only place the courtyard/outline distinction exists in the document.
    let mut in_interior = false;

    loop {
        match xml.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    b"Building" | b"BuildingPart" => {
                        let id = e
                            .try_get_attribute("gml:id")
                            .ok()
                            .flatten()
                            .map(|a| String::from_utf8_lossy(&a.value).into_owned())
                            .unwrap_or_default();
                        stack.push(Ctx { id, ..Default::default() });
                    }
                    b"measuredHeight" => in_height = !stack.is_empty(),
                    b"GroundSurface" => {
                        if let Some(c) = stack.last_mut() {
                            c.in_ground = true;
                        }
                    }
                    b"exterior" => in_interior = false,
                    b"interior" => in_interior = true,
                    b"posList" => {
                        in_poslist = stack.last().map(|c| c.in_ground).unwrap_or(false);
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_height {
                    if let Some(c) = stack.last_mut() {
                        if let Ok(v) = t.unescape().unwrap_or_default().trim().parse::<f64>() {
                            c.height = Some(v);
                        }
                    }
                    in_height = false;
                } else if in_poslist {
                    if let Some(c) = stack.last_mut() {
                        let text = t.unescape().unwrap_or_default();
                        let vals: Vec<f64> = text
                            .split_ascii_whitespace()
                            .filter_map(|s| s.parse().ok())
                            .collect();
                        if vals.len() >= 9 && vals.len() % 3 == 0 {
                            let mut ring = Vec::with_capacity(vals.len() / 3);
                            for tri in vals.chunks_exact(3) {
                                ring.push((tri[0], tri[1]));
                                c.ground_z_sum += tri[2];
                                c.ground_z_n += 1;
                            }
                            // An interior ring belongs to the polygon that
                            // opened before it. If one arrives with no
                            // polygon open the document is malformed; keep it
                            // as its own outline rather than dropping a piece
                            // of building on the floor.
                            match (in_interior, c.rings.last_mut()) {
                                (true, Some(p)) => p.interiors.push(ring),
                                _ => c.rings.push(Polygon {
                                    exterior: ring,
                                    interiors: Vec::new(),
                                }),
                            }
                        }
                    }
                    in_poslist = false;
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"Building" | b"BuildingPart" => {
                    if let Some(ctx) = stack.pop() {
                        finish(ctx, &mut out);
                    }
                }
                b"GroundSurface" => {
                    if let Some(c) = stack.last_mut() {
                        c.in_ground = false;
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(PackError::Invalid(format!("citygml: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Rasterize a building at 1 m and feed (x, y, height) samples to the sink —
/// scanline fill, even-odd over each polygon's outline AND its courtyards.
///
/// Even-odd is applied per POLYGON, not across the whole building: a point
/// inside an outline and inside one of that outline's courtyards crosses two
/// boundaries and is correctly outside, while two separate wings of the same
/// building each fill independently. Running even-odd across every ring of
/// the building at once would punch a hole wherever two wings overlap, which
/// LoD2 `BuildingPart`s do at shared walls.
pub fn rasterize_building(b: &Lod2Building, mut sink: impl FnMut(f64, f64, f64)) {
    let mut xs: Vec<f64> = Vec::new();
    for poly in &b.rings {
        if poly.exterior.len() < 3 {
            continue;
        }
        let (mut min_y, mut max_y) = (f64::MAX, f64::MIN);
        for &(_, y) in &poly.exterior {
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let mut y = min_y.floor() + 0.5;
        while y <= max_y {
            xs.clear();
            // The outline and every courtyard contribute crossings to the
            // same scanline, so a span that enters the outline and then
            // enters a yard closes at the yard's edge.
            for ring in std::iter::once(&poly.exterior).chain(poly.interiors.iter()) {
                if ring.len() < 3 {
                    continue;
                }
                for i in 0..ring.len() {
                    let (x1, y1) = ring[i];
                    let (x2, y2) = ring[(i + 1) % ring.len()];
                    if (y1 <= y && y2 > y) || (y2 <= y && y1 > y) {
                        xs.push(x1 + (y - y1) / (y2 - y1) * (x2 - x1));
                    }
                }
            }
            xs.sort_by(f64::total_cmp);
            for pair in xs.chunks_exact(2) {
                let mut x = pair[0].floor() + 0.5;
                while x <= pair[1] {
                    if x >= pair[0] {
                        sink(x, y, b.height_m);
                    }
                    x += 1.0;
                }
            }
            y += 1.0;
        }
    }
}

pub const BERLIN_LOD2_NOTICE: &str =
    "Geoportal Berlin: 3D-Geb\u{e4}udemodelle LoD2 \u{2014} Datenlizenz Deutschland \u{2013} Zero \u{2013} Version 2.0 (dl-de/zero-2.0)";

#[cfg(test)]
mod tests {
    use super::*;

    const SNIPPET: &str = r#"<?xml version="1.0"?>
<CityModel xmlns:bldg="http://www.opengis.net/citygml/building/2.0" xmlns:gml="http://www.opengis.net/gml">
 <cityObjectMember>
  <bldg:Building gml:id="B1">
   <bldg:measuredHeight uom="urn:adv:uom:m">12.5</bldg:measuredHeight>
   <bldg:boundedBy><bldg:GroundSurface>
     <bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing>
      <gml:posList srsDimension="3">100.0 200.0 35.0 110.0 200.0 35.0 110.0 210.0 35.0 100.0 210.0 35.0 100.0 200.0 35.0</gml:posList>
     </gml:LinearRing></gml:exterior></gml:Polygon></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
   </bldg:GroundSurface></bldg:boundedBy>
   <bldg:boundedBy><bldg:WallSurface>
     <gml:posList srsDimension="3">0 0 0 1 1 1 2 2 2</gml:posList>
   </bldg:WallSurface></bldg:boundedBy>
  </bldg:Building>
 </cityObjectMember>
</CityModel>"#;

    #[test]
    fn parses_building_with_ground_ring() {
        let b = parse_citygml(SNIPPET.as_bytes()).unwrap();
        assert_eq!(b.len(), 1);
        let b = &b[0];
        assert_eq!(b.id, "B1");
        assert!((b.height_m - 12.5).abs() < 1e-9);
        assert!((b.area_m2 - 100.0).abs() < 1e-6, "{}", b.area_m2);
        assert!((b.e - 105.0).abs() < 1e-6 && (b.n - 205.0).abs() < 1e-6);
        assert!((b.ground_z - 35.0).abs() < 1e-9);
        // Wall posList must NOT be captured as a footprint.
        assert_eq!(b.rings.len(), 1);
    }

    #[test]
    fn rasterizes_square_footprint() {
        let b = parse_citygml(SNIPPET.as_bytes()).unwrap().remove(0);
        let mut count = 0usize;
        let mut inside = true;
        rasterize_building(&b, |x, y, h| {
            count += 1;
            inside &= (100.0..110.0).contains(&x) && (200.0..210.0).contains(&y);
            assert_eq!(h, 12.5);
        });
        assert_eq!(count, 100, "10×10 m at 1 m = 100 samples");
        assert!(inside);
    }

    /// A Blockrand block: 20×20 m outline with a 10×10 m courtyard.
    ///
    /// `gml:interior` is a hole: read as one more outer ring, the yard would
    /// be ADDED to the footprint and filled solid — 300 m² of building
    /// reported as 500 m², and the one patch of open sky in the middle of the
    /// block modelled as masonry.
    const COURTYARD: &str = r#"<?xml version="1.0"?>
<CityModel xmlns:bldg="http://www.opengis.net/citygml/building/2.0" xmlns:gml="http://www.opengis.net/gml">
 <cityObjectMember>
  <bldg:Building gml:id="HOF">
   <bldg:measuredHeight uom="urn:adv:uom:m">22.0</bldg:measuredHeight>
   <bldg:boundedBy><bldg:GroundSurface>
    <bldg:lod2MultiSurface><gml:MultiSurface><gml:surfaceMember><gml:Polygon>
     <gml:exterior><gml:LinearRing>
      <gml:posList srsDimension="3">0 0 40 20 0 40 20 20 40 0 20 40 0 0 40</gml:posList>
     </gml:LinearRing></gml:exterior>
     <gml:interior><gml:LinearRing>
      <gml:posList srsDimension="3">5 5 40 15 5 40 15 15 40 5 15 40 5 5 40</gml:posList>
     </gml:LinearRing></gml:interior>
    </gml:Polygon></gml:surfaceMember></gml:MultiSurface></bldg:lod2MultiSurface>
   </bldg:GroundSurface></bldg:boundedBy>
  </bldg:Building>
 </cityObjectMember>
</CityModel>"#;

    #[test]
    fn a_courtyard_is_subtracted_from_the_footprint_not_added_to_it() {
        let b = parse_citygml(COURTYARD.as_bytes()).unwrap();
        assert_eq!(b.len(), 1);
        let b = &b[0];
        assert_eq!(b.rings.len(), 1, "one polygon, not two rings");
        assert_eq!(b.rings[0].interiors.len(), 1, "the yard must be an interior");
        // 20×20 outline − 10×10 yard, not 400 + 100 = 500.
        assert!((b.area_m2 - 300.0).abs() < 1e-6, "area was {}", b.area_m2);
        // Centroid stays at the block's centre by symmetry, but it is now the
        // centre of the RING of building rather than an area-weighted blend
        // that counted the yard twice.
        assert!((b.e - 10.0).abs() < 1e-6 && (b.n - 10.0).abs() < 1e-6, "{},{}", b.e, b.n);
    }

    #[test]
    fn a_courtyard_is_open_sky_in_the_raster_and_not_filled_masonry() {
        let b = parse_citygml(COURTYARD.as_bytes()).unwrap().remove(0);
        let mut hits: Vec<(f64, f64)> = Vec::new();
        rasterize_building(&b, |x, y, h| {
            assert_eq!(h, 22.0);
            hits.push((x, y));
        });
        // 400 m² of outline minus 100 m² of yard, at 1 m sampling.
        assert_eq!(hits.len(), 300, "yard filled: {} samples", hits.len());
        // Nothing inside the yard, and the walls around it are still there.
        assert!(
            !hits.iter().any(|&(x, y)| (5.0..15.0).contains(&x) && (5.0..15.0).contains(&y)),
            "a sample landed inside the courtyard"
        );
        assert!(hits.contains(&(2.5, 10.5)) && hits.contains(&(17.5, 10.5)));
    }

    /// Two wings of one building that share a wall must both fill.
    ///
    /// This is why even-odd runs per POLYGON and not across every ring of the
    /// building at once: LoD2 splits a block into `BuildingPart`s whose ground
    /// surfaces touch, and a building-wide even-odd pass would cancel them
    /// against each other and punch a hole through the overlap.
    #[test]
    fn two_overlapping_outlines_of_one_building_do_not_cancel_each_other() {
        let b = Lod2Building {
            id: "W".into(),
            e: 0.0,
            n: 0.0,
            ground_z: 0.0,
            area_m2: 0.0,
            height_m: 10.0,
            source: HeightSource::Lod2,
            rings: vec![
                Polygon {
                    exterior: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
                    interiors: vec![],
                },
                Polygon {
                    exterior: vec![(5.0, 0.0), (15.0, 0.0), (15.0, 10.0), (5.0, 10.0)],
                    interiors: vec![],
                },
            ],
        };
        let mut cells = std::collections::HashSet::new();
        rasterize_building(&b, |x, y, _| {
            cells.insert((x.floor() as i32, y.floor() as i32));
        });
        // Union is 15×10. The overlap 5..10 must be building, not a hole.
        assert_eq!(cells.len(), 150, "union was {} cells", cells.len());
        assert!(cells.contains(&(7, 5)), "the shared wall was punched out");
    }

    #[test]
    fn geometry_rides_in_the_json_line_only_when_the_build_asks_for_it() {
        let b = parse_citygml(COURTYARD.as_bytes()).unwrap().remove(0);
        let lean = b.to_json_line(false).unwrap();
        assert!(!lean.contains("rings"), "centroid-only line carried geometry");
        assert!(lean.contains("area_m2"));
        let full = b.to_json_line(true).unwrap();
        assert!(full.contains("rings") && full.contains("interiors"));
        // Additive: the lean fields are unchanged, so a reader of the old
        // schema parses the geometry line without knowing about it.
        let a: serde_json::Value = serde_json::from_str(&lean).unwrap();
        let c: serde_json::Value = serde_json::from_str(&full).unwrap();
        for k in ["id", "e", "n", "ground_z", "area_m2", "height_m"] {
            assert_eq!(a[k], c[k], "field {k} changed between the two forms");
        }
    }
}
