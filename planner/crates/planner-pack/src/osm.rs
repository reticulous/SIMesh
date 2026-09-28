//! OpenStreetMap from one PBF extract (a Geofabrik region file): the roads
//! layer, the gazetteer and building footprints, from the same file.
//!
//! What is taken:
//! - roads: ways with `highway` motorway/trunk/primary/secondary or their
//!   `_link`, and `railway` rail/light_rail/subway ([`RoadClass::from_tag`]);
//! - places: named `place` nodes city, town, village, hamlet, suburb, quarter,
//!   borough, neighbourhood; named `natural=peak` nodes; named
//!   `man_made=mast|tower|communications_tower` nodes, and ways at the mean of
//!   their vertices; every named `highway` way as one street at the centre of
//!   its extent; `boundary=postal_code` relations with their member ways
//!   joined into rings;
//! - buildings, when asked: closed ways tagged `building` and
//!   `type=multipolygon` relations tagged `building` (`building=no` is not a
//!   building), with heights from `planner_buildings::osm_height`.
//!
//! Only features meeting the build's bounding box are kept: a node inside it,
//! a way or relation whose extent meets it.
//!
//! The reader makes three passes over the file, each decoding blocks in
//! parallel: relations (to learn which ways they need), ways (to learn which
//! nodes they need), nodes. Only the tags in [`KEYS`] are kept in memory.

use crate::lod2::{footprint, Lod2Building, Polygon};
use crate::places::{Gazetteer, GazetteerBuilder, Kind};
use crate::roads::{RoadClass, Way as Road};
use crate::PackError;
use osmpbf::{BlobDecode, BlobReader, PrimitiveBlock, RelMemberType};
use planner_buildings::{osm_height, HeightSource};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const OSM_BUILDINGS_NOTICE: &str =
    "Building footprints and heights \u{a9} OpenStreetMap contributors, ODbL 1.0 (opendatacommons.org/licenses/odbl)";

/// The tag keys the selection reads.
pub const KEYS: &[&str] = &[
    "addr:postcode",
    "boundary",
    "building",
    "building:levels",
    "ele",
    "height",
    "highway",
    "man_made",
    "name",
    "natural",
    "place",
    "postal_code",
    "railway",
    "roof:shape",
    "type",
];

/// (lat, lon) in degrees.
pub type LatLon = (f64, f64);

/// An element's tags, restricted to [`KEYS`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tags(pub Vec<(String, String)>);

impl Tags {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str())
    }

    pub fn read<'a>(it: impl Iterator<Item = (&'a str, &'a str)>) -> Self {
        Tags(
            it.filter(|(k, _)| KEYS.contains(k))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn without(&self, k: &str) -> Self {
        Tags(self.0.iter().filter(|(kk, _)| kk != k).cloned().collect())
    }
}

impl From<&[(&str, &str)]> for Tags {
    fn from(kv: &[(&str, &str)]) -> Self {
        Tags::read(kv.iter().copied())
    }
}

pub fn is_building(t: &Tags) -> bool {
    t.get("building").is_some_and(|b| b != "no")
}

fn site_kind(t: &Tags) -> Option<Kind> {
    match (t.get("natural"), t.get("man_made")) {
        (Some("peak"), _) => Some(Kind::Peak),
        (_, Some("mast" | "tower" | "communications_tower")) => Some(Kind::Tower),
        _ => None,
    }
}

fn node_kind(t: &Tags) -> Option<Kind> {
    t.get("place").and_then(Kind::from_place_tag).or_else(|| site_kind(t))
}

fn postal_code(t: &Tags) -> Option<&str> {
    t.get("postal_code").or_else(|| t.get("addr:postcode"))
}

pub fn wants_node(t: &Tags) -> bool {
    t.get("name").is_some() && node_kind(t).is_some()
}

pub fn wants_way(t: &Tags, buildings: bool) -> bool {
    RoadClass::from_tag(t.get("highway"), t.get("railway")).is_some()
        || (t.get("highway").is_some() && t.get("name").is_some())
        || (t.get("name").is_some() && site_kind(t) == Some(Kind::Tower))
        || (buildings && is_building(t))
}

pub fn wants_relation(t: &Tags, buildings: bool) -> bool {
    t.get("boundary") == Some("postal_code")
        || (buildings && t.get("type") == Some("multipolygon") && is_building(t))
}

fn extent(pts: &[LatLon]) -> [f64; 4] {
    let mut r = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for &(lat, lon) in pts {
        r = [r[0].min(lon), r[1].min(lat), r[2].max(lon), r[3].max(lat)];
    }
    r
}

fn meets(bbox: [f64; 4], e: [f64; 4]) -> bool {
    e[0] <= bbox[2] && e[2] >= bbox[0] && e[1] <= bbox[3] && e[3] >= bbox[1]
}

fn closed(pts: &[LatLon]) -> bool {
    pts.len() >= 4 && pts.first() == pts.last()
}

/// Join way pieces end to end, reversing pieces as needed, into rings.
/// Returns the closed rings and the pieces that would not close.
pub fn join_rings<P: Copy + PartialEq>(pieces: Vec<Vec<P>>) -> (Vec<Vec<P>>, Vec<Vec<P>>) {
    let mut open: Vec<Vec<P>> = pieces.into_iter().filter(|p| p.len() >= 2).collect();
    open.reverse();
    let (mut rings, mut stuck) = (Vec::new(), Vec::new());
    while let Some(mut cur) = open.pop() {
        loop {
            if cur.len() >= 4 && cur.first() == cur.last() {
                rings.push(cur);
                break;
            }
            let (start, end) = (cur[0], cur[cur.len() - 1]);
            if let Some(i) = open.iter().position(|p| p[0] == end || p[p.len() - 1] == end) {
                let mut p = open.remove(i);
                if p[0] != end {
                    p.reverse();
                }
                cur.extend_from_slice(&p[1..]);
            } else if let Some(i) =
                open.iter().position(|p| p[0] == start || p[p.len() - 1] == start)
            {
                let mut p = open.remove(i);
                if p[p.len() - 1] != start {
                    p.reverse();
                }
                p.extend_from_slice(&cur[1..]);
                cur = p;
            } else {
                stuck.push(cur);
                break;
            }
        }
    }
    (rings, stuck)
}

/// Even-odd point-in-ring.
fn ring_contains(ring: &[(f64, f64)], (x, y): (f64, f64)) -> bool {
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        if (y1 > y) != (y2 > y) && x < x1 + (y - y1) / (y2 - y1) * (x2 - x1) {
            inside = !inside;
        }
    }
    inside
}

/// What the extract gives a pack.
#[derive(Debug, Default)]
pub struct OsmLayers {
    pub roads: Vec<Road>,
    pub places: Gazetteer,
    /// Footprints in the pack CRS with their heights; `ground_z` is left at 0
    /// for the compiler to fill from the pack terrain.
    pub buildings: Vec<Lod2Building>,
}

/// Turns selected elements, with their geometry resolved, into layers.
/// The reader feeds it in element-id order so a build is reproducible.
pub struct Collector<'a> {
    bbox: [f64; 4],
    buildings: bool,
    to_xy: &'a (dyn Fn(f64, f64) -> (f64, f64) + Sync),
    roads: Vec<Road>,
    gaz: GazetteerBuilder,
    bldgs: Vec<Lod2Building>,
}

impl<'a> Collector<'a> {
    /// `bbox` is `[min_lon, min_lat, max_lon, max_lat]`; `to_xy` projects
    /// `(lat, lon)` to the pack CRS.
    pub fn new(
        bbox: [f64; 4],
        buildings: bool,
        to_xy: &'a (dyn Fn(f64, f64) -> (f64, f64) + Sync),
    ) -> Self {
        Collector { bbox, buildings, to_xy, roads: Vec::new(), gaz: GazetteerBuilder::new(), bldgs: Vec::new() }
    }

    fn xy(&self, (lat, lon): LatLon) -> (f64, f64) {
        (self.to_xy)(lat, lon)
    }

    pub fn node(&mut self, t: &Tags, lat: f64, lon: f64) {
        if !meets(self.bbox, [lon, lat, lon, lat]) {
            return;
        }
        let (Some(name), Some(kind)) = (t.get("name"), node_kind(t)) else { return };
        let (x, y) = self.xy((lat, lon));
        self.gaz.point(kind, name, t.get("ele"), x as f32, y as f32);
    }

    pub fn way(&mut self, id: i64, t: &Tags, pts: &[LatLon]) {
        if pts.len() < 2 {
            return;
        }
        let e = extent(pts);
        if !meets(self.bbox, e) {
            return;
        }
        if let Some(class) = RoadClass::from_tag(t.get("highway"), t.get("railway")) {
            let points = pts
                .iter()
                .map(|&p| {
                    let (x, y) = self.xy(p);
                    (x as f32, y as f32)
                })
                .collect();
            self.roads.push(Road { class, points });
        }
        if let Some(name) = t.get("name") {
            if site_kind(t) == Some(Kind::Tower) {
                let n = pts.len() as f64;
                let (sx, sy) = pts.iter().fold((0.0, 0.0), |(sx, sy), &p| {
                    let (x, y) = self.xy(p);
                    (sx + x, sy + y)
                });
                self.gaz.point(Kind::Tower, name, t.get("ele"), (sx / n) as f32, (sy / n) as f32);
            } else if t.get("highway").is_some() {
                let (x, y) = self.xy(((e[1] + e[3]) / 2.0, (e[0] + e[2]) / 2.0));
                self.gaz.street(name, postal_code(t), x, y);
            }
        }
        if self.buildings && is_building(t) && closed(pts) {
            let ring: Vec<(f64, f64)> = pts.iter().map(|&p| self.xy(p)).collect();
            self.building(format!("w{id}"), t, vec![Polygon { exterior: ring, interiors: Vec::new() }]);
        }
    }

    /// `members` are the relation's member ways in order, each with its role.
    pub fn relation(&mut self, id: i64, t: &Tags, members: &[(String, Vec<LatLon>)]) {
        let all: Vec<LatLon> = members.iter().flat_map(|(_, p)| p.iter().copied()).collect();
        if all.is_empty() || !meets(self.bbox, extent(&all)) {
            return;
        }
        if t.get("boundary") == Some("postal_code") {
            let Some(code) = postal_code(t).or_else(|| t.get("name")) else { return };
            let (rings, pieces) = join_rings(members.iter().map(|(_, p)| p.clone()).collect());
            let rings = rings
                .into_iter()
                .chain(pieces)
                .map(|r| {
                    r.into_iter()
                        .map(|p| {
                            let (x, y) = self.xy(p);
                            (x as f32, y as f32)
                        })
                        .collect()
                })
                .collect();
            self.gaz.postal_area(code, rings);
            return;
        }
        if self.buildings && t.get("type") == Some("multipolygon") && is_building(t) {
            let pieces = |inner: bool| -> Vec<Vec<LatLon>> {
                members
                    .iter()
                    .filter(|(role, _)| (role == "inner") == inner)
                    .map(|(_, p)| p.clone())
                    .collect()
            };
            let project = |r: Vec<LatLon>| -> Vec<(f64, f64)> { r.into_iter().map(|p| self.xy(p)).collect() };
            let (outers, _) = join_rings(pieces(false));
            let (inners, _) = join_rings(pieces(true));
            let mut polys: Vec<Polygon> = outers
                .into_iter()
                .map(|r| Polygon { exterior: project(r), interiors: Vec::new() })
                .collect();
            for inner in inners {
                let inner = project(inner);
                if let Some(p) = polys.iter_mut().find(|p| ring_contains(&p.exterior, inner[0])) {
                    p.interiors.push(inner);
                }
            }
            if !polys.is_empty() {
                self.building(format!("r{id}"), t, polys);
            }
        }
    }

    fn building(&mut self, id: String, t: &Tags, rings: Vec<Polygon>) {
        let Some((h, source)) = osm_height(|k| t.get(k)) else { return };
        let (area, e, n) = footprint(&rings);
        if area < 1.0 {
            return;
        }
        self.bldgs.push(Lod2Building {
            id,
            e,
            n,
            ground_z: 0.0,
            area_m2: area,
            height_m: h as f64,
            source,
            rings,
        });
    }

    pub fn finish(self) -> OsmLayers {
        OsmLayers { roads: self.roads, places: self.gaz.finish(), buildings: self.bldgs }
    }
}

fn pbf_err(path: &Path, e: osmpbf::Error) -> PackError {
    PackError::Invalid(format!("OpenStreetMap extract {}: {e}", path.display()))
}

/// Decode every data block of the file in parallel and map each with `f`.
fn blocks<T: Send>(
    path: &Path,
    f: impl Fn(&PrimitiveBlock) -> T + Sync + Send,
) -> Result<Vec<T>, PackError> {
    let reader = BlobReader::from_path(path).map_err(|e| pbf_err(path, e))?;
    reader
        .par_bridge()
        .map(|blob| -> Result<Option<T>, PackError> {
            let blob = blob.map_err(|e| pbf_err(path, e))?;
            match blob.decode().map_err(|e| pbf_err(path, e))? {
                BlobDecode::OsmData(block) => Ok(Some(f(&block))),
                _ => Ok(None),
            }
        })
        .filter_map(Result::transpose)
        .collect()
}

struct RelRec {
    id: i64,
    tags: Tags,
    members: Vec<(i64, String)>,
}

struct WayRec {
    id: i64,
    /// Empty when the way is read only as a relation member.
    tags: Tags,
    refs: Vec<i64>,
}

/// How many of each height source the buildings carry.
pub fn height_census(buildings: &[Lod2Building]) -> HashMap<HeightSource, usize> {
    let mut m = HashMap::new();
    for b in buildings {
        *m.entry(b.source).or_insert(0) += 1;
    }
    m
}

/// Read the layers a pack takes from an OpenStreetMap PBF extract.
///
/// `bbox` is `[min_lon, min_lat, max_lon, max_lat]`, `to_xy` projects
/// `(lat, lon)` to the pack CRS, and `pass(i, 3)` is called as each of the
/// three passes over the file ends.
pub fn read_pbf(
    path: &Path,
    bbox: [f64; 4],
    buildings: bool,
    to_xy: &(dyn Fn(f64, f64) -> (f64, f64) + Sync),
    pass: &dyn Fn(u64, u64),
) -> Result<OsmLayers, PackError> {
    // Relations, and the ways they need.
    let mut rels: Vec<RelRec> = blocks(path, |b| {
        let mut out = Vec::new();
        for g in b.groups() {
            for r in g.relations() {
                let tags = Tags::read(r.tags());
                if !wants_relation(&tags, buildings) {
                    continue;
                }
                let members = r
                    .members()
                    .filter(|m| m.member_type == RelMemberType::Way)
                    .map(|m| (m.member_id, m.role().unwrap_or("").to_string()))
                    .collect();
                out.push(RelRec { id: r.id(), tags, members });
            }
        }
        out
    })?
    .into_iter()
    .flatten()
    .collect();
    rels.sort_by_key(|r| r.id);
    let member_ways: HashSet<i64> = rels.iter().flat_map(|r| r.members.iter().map(|m| m.0)).collect();
    // A way that is an outer member of a building multipolygon is that
    // building; its own `building` tag would count it twice.
    let building_outers: HashSet<i64> = rels
        .iter()
        .filter(|r| r.tags.get("type") == Some("multipolygon") && is_building(&r.tags))
        .flat_map(|r| r.members.iter().filter(|m| m.1 != "inner").map(|m| m.0))
        .collect();
    pass(1, 3);

    // Ways, and the nodes they need.
    let mut ways: Vec<WayRec> = blocks(path, |b| {
        let mut out = Vec::new();
        for g in b.groups() {
            for w in g.ways() {
                let tags = Tags::read(w.tags());
                let own = wants_way(&tags, buildings);
                if !own && !member_ways.contains(&w.id()) {
                    continue;
                }
                out.push(WayRec {
                    id: w.id(),
                    tags: if own { tags } else { Tags::default() },
                    refs: w.refs().collect(),
                });
            }
        }
        out
    })?
    .into_iter()
    .flatten()
    .collect();
    ways.sort_by_key(|w| w.id);
    let mut needed: Vec<i64> = ways.iter().flat_map(|w| w.refs.iter().copied()).collect();
    needed.sort_unstable();
    needed.dedup();
    pass(2, 3);

    // Nodes: the coordinates the ways need, and the tagged nodes themselves.
    type NodeOut = (Vec<(usize, f64, f64)>, Vec<(i64, Tags, f64, f64)>);
    let parts: Vec<NodeOut> = blocks(path, |b| {
        let (mut coords, mut tagged) = (Vec::new(), Vec::new());
        let mut take = |id: i64, lat: f64, lon: f64, tags: Tags| {
            if let Ok(i) = needed.binary_search(&id) {
                coords.push((i, lat, lon));
            }
            if wants_node(&tags) {
                tagged.push((id, tags, lat, lon));
            }
        };
        for g in b.groups() {
            for n in g.dense_nodes() {
                take(n.id(), n.lat(), n.lon(), Tags::read(n.tags()));
            }
            for n in g.nodes() {
                take(n.id(), n.lat(), n.lon(), Tags::read(n.tags()));
            }
        }
        (coords, tagged)
    })?;
    let mut loc = vec![(f64::NAN, f64::NAN); needed.len()];
    let mut tagged = Vec::new();
    for (coords, t) in parts {
        for (i, lat, lon) in coords {
            loc[i] = (lat, lon);
        }
        tagged.extend(t);
    }
    tagged.sort_by_key(|t| t.0);
    pass(3, 3);

    let pts = |refs: &[i64]| -> Vec<LatLon> {
        refs.iter()
            .filter_map(|r| needed.binary_search(r).ok().map(|i| loc[i]))
            .filter(|p| p.0.is_finite())
            .collect()
    };
    let mut c = Collector::new(bbox, buildings, to_xy);
    for (_, tags, lat, lon) in &tagged {
        c.node(tags, *lat, *lon);
    }
    let way_index: HashMap<i64, usize> = ways.iter().enumerate().map(|(i, w)| (w.id, i)).collect();
    for w in &ways {
        if w.tags.0.is_empty() {
            continue;
        }
        if building_outers.contains(&w.id) {
            c.way(w.id, &w.tags.without("building"), &pts(&w.refs));
        } else {
            c.way(w.id, &w.tags, &pts(&w.refs));
        }
    }
    for r in &rels {
        let members: Vec<(String, Vec<LatLon>)> = r
            .members
            .iter()
            .filter_map(|(id, role)| way_index.get(id).map(|&i| (role.clone(), pts(&ways[i].refs))))
            .collect();
        c.relation(r.id, &r.tags, &members);
    }
    Ok(c.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: [f64; 4] = [13.0, 52.0, 14.0, 53.0];

    /// Metres-ish: 1e-3 degree to 1 m, so assertions read in whole numbers.
    fn proj(lat: f64, lon: f64) -> (f64, f64) {
        (lon * 1000.0, lat * 1000.0)
    }

    fn tags(kv: &[(&str, &str)]) -> Tags {
        Tags::from(kv)
    }

    fn collect(f: impl FnOnce(&mut Collector)) -> OsmLayers {
        let mut c = Collector::new(BOX, true, &proj);
        f(&mut c);
        c.finish()
    }

    /// A closed square way of `side` metres under [`proj`].
    fn square(lat: f64, lon: f64, side: f64) -> Vec<LatLon> {
        let d = side / 1000.0;
        vec![(lat, lon), (lat, lon + d), (lat + d, lon + d), (lat + d, lon), (lat, lon)]
    }

    #[test]
    fn roads_take_the_main_classes_and_rail_only() {
        let line = [(52.5, 13.4), (52.51, 13.41)];
        let l = collect(|c| {
            c.way(1, &tags(&[("highway", "primary_link")]), &line);
            c.way(2, &tags(&[("highway", "residential")]), &line);
            c.way(3, &tags(&[("railway", "subway")]), &line);
            c.way(4, &tags(&[("railway", "tram")]), &line);
            c.way(5, &tags(&[("highway", "motorway")]), &[(50.0, 10.0), (50.1, 10.1)]);
        });
        let classes: Vec<RoadClass> = l.roads.iter().map(|r| r.class).collect();
        assert_eq!(classes, [RoadClass::Primary, RoadClass::Rail], "outside the box is left out");
        assert_eq!(l.roads[0].points[0], (13400.0, 52500.0));
    }

    #[test]
    fn a_way_crossing_the_box_without_a_vertex_inside_is_kept() {
        let across = [(52.5, 12.5), (52.5, 14.5)];
        let l = collect(|c| c.way(1, &tags(&[("highway", "trunk")]), &across));
        assert_eq!(l.roads.len(), 1);
    }

    #[test]
    fn places_summits_and_masts_are_named_nodes_or_mast_ways() {
        let l = collect(|c| {
            c.node(&tags(&[("place", "suburb"), ("name", "Mitte")]), 52.52, 13.40);
            c.node(&tags(&[("place", "hamlet"), ("name", "Kleindorf")]), 52.6, 13.5);
            c.node(&tags(&[("place", "locality"), ("name", "Flur")]), 52.6, 13.5);
            c.node(&tags(&[("place", "city")]), 52.5, 13.4);
            c.node(&tags(&[("natural", "peak"), ("name", "Teufelsberg"), ("ele", "120")]), 52.4979, 13.2413);
            c.node(&tags(&[("place", "city"), ("name", "Hamburg")]), 53.55, 9.99);
            c.way(
                9,
                &tags(&[("man_made", "tower"), ("name", "Funkturm")]),
                &[(52.505, 13.278), (52.505, 13.280), (52.507, 13.280), (52.507, 13.278)],
            );
        });
        let g = l.places;
        let names: Vec<&str> = g.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Mitte", "Kleindorf", "Teufelsberg", "Funkturm"]);
        assert_eq!(g.search("teufelsberg", 1)[0].ctx, "120 m");
        let mast = &g.search("funkturm", 1)[0];
        assert_eq!(mast.kind, Kind::Tower);
        assert!((mast.x - 13279.0).abs() < 0.01 && (mast.y - 52506.0).abs() < 0.01);
    }

    #[test]
    fn a_named_street_is_one_point_at_the_centre_of_its_extent() {
        let l = collect(|c| {
            c.way(
                1,
                &tags(&[("highway", "residential"), ("name", "Kollwitzstraße"), ("addr:postcode", "10405")]),
                &[(52.530, 13.410), (52.532, 13.411), (52.534, 13.416)],
            );
            c.way(2, &tags(&[("highway", "residential")]), &[(52.53, 13.41), (52.54, 13.42)]);
        });
        assert_eq!(l.places.entries.len(), 1);
        let e = &l.places.entries[0];
        assert_eq!((e.kind, e.ctx.as_str()), (Kind::Street, "10405"));
        assert!((e.x - 13413.0).abs() < 0.01 && (e.y - 52532.0).abs() < 0.01);
        assert!(!l.places.search("kollwitzstr", 3).is_empty());
    }

    #[test]
    fn a_postal_relation_joins_its_member_ways_into_one_ring() {
        // Three pieces of one triangle, the middle one drawn backwards.
        let members = vec![
            ("outer".to_string(), vec![(52.0, 13.0), (52.0, 13.1)]),
            ("outer".to_string(), vec![(52.1, 13.1), (52.0, 13.1)]),
            ("outer".to_string(), vec![(52.1, 13.1), (52.0, 13.0)]),
        ];
        let l = collect(|c| c.relation(7, &tags(&[("boundary", "postal_code"), ("postal_code", "10115")]), &members));
        let a = &l.places.areas;
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].code, "10115");
        assert_eq!(a[0].rings.len(), 1, "joined: {:?}", a[0].rings);
        assert_eq!(a[0].rings[0].len(), 4);
        assert_eq!(a[0].rings[0].first(), a[0].rings[0].last());
        assert_eq!(l.places.search("10115", 1)[0].kind, Kind::Postcode);
    }

    #[test]
    fn a_building_way_becomes_a_footprint_record_with_its_height_source() {
        let l = collect(|c| {
            c.way(11, &tags(&[("building", "yes"), ("building:levels", "4"), ("roof:shape", "flat")]), &square(52.5, 13.4, 20.0));
            c.way(12, &tags(&[("building", "garage")]), &square(52.5, 13.5, 10.0));
            c.way(13, &tags(&[("building", "no")]), &square(52.5, 13.6, 10.0));
            c.way(14, &tags(&[("building", "yes")]), &square(52.5, 13.7, 10.0)[..4]);
        });
        let b = &l.buildings;
        assert_eq!(b.len(), 2, "building=no and an unclosed outline are not buildings");
        assert_eq!(b[0].id, "w11");
        assert_eq!((b[0].height_m, b[0].source), (12.0, HeightSource::OsmLevels));
        assert!((b[0].area_m2 - 400.0).abs() < 1e-6, "20×20 m: {}", b[0].area_m2);
        assert!((b[0].e - 13410.0).abs() < 1e-6 && (b[0].n - 52510.0).abs() < 1e-6);
        assert_eq!(b[0].rings.len(), 1);
        assert_eq!((b[1].height_m, b[1].source), (3.0, HeightSource::Default));
        let line: serde_json::Value = serde_json::from_str(&b[0].to_json_line(true).unwrap()).unwrap();
        assert_eq!(line["source"], "osm_levels");
        assert!(line["rings"][0]["exterior"].as_array().unwrap().len() == 5);
    }

    #[test]
    fn a_building_multipolygon_keeps_its_courtyard_as_a_hole() {
        // A 30×30 block drawn as two outer pieces, a 10×10 yard inside.
        let o = |lat: f64, lon: f64| (52.5 + lat / 1000.0, 13.4 + lon / 1000.0);
        let members = vec![
            ("outer".to_string(), vec![o(0.0, 0.0), o(0.0, 30.0), o(30.0, 30.0)]),
            ("outer".to_string(), vec![o(30.0, 30.0), o(30.0, 0.0), o(0.0, 0.0)]),
            ("inner".to_string(), vec![o(10.0, 10.0), o(10.0, 20.0), o(20.0, 20.0), o(20.0, 10.0), o(10.0, 10.0)]),
        ];
        let l = collect(|c| {
            c.relation(5, &tags(&[("type", "multipolygon"), ("building", "apartments"), ("height", "22")]), &members)
        });
        let b = &l.buildings[0];
        assert_eq!(b.id, "r5");
        assert_eq!((b.height_m, b.source), (22.0, HeightSource::OsmHeight));
        assert_eq!(b.rings.len(), 1);
        assert_eq!(b.rings[0].interiors.len(), 1);
        assert!((b.area_m2 - 800.0).abs() < 1e-3, "900 − 100: {}", b.area_m2);
    }

    #[test]
    fn selection_matches_what_the_reader_keeps() {
        assert!(wants_way(&tags(&[("highway", "service"), ("name", "Hof")]), false));
        assert!(!wants_way(&tags(&[("highway", "service")]), false));
        assert!(wants_way(&tags(&[("building", "yes")]), true));
        assert!(!wants_way(&tags(&[("building", "yes")]), false));
        assert!(!wants_way(&tags(&[("man_made", "tower")]), false));
        assert!(wants_relation(&tags(&[("boundary", "postal_code")]), false));
        assert!(wants_relation(&tags(&[("type", "multipolygon"), ("building", "yes")]), true));
        assert!(!wants_relation(&tags(&[("type", "multipolygon"), ("landuse", "grass")]), true));
        assert!(wants_node(&tags(&[("man_made", "mast"), ("name", "Sender")])));
        assert!(!wants_node(&tags(&[("amenity", "cafe"), ("name", "Kaffee")])));
        // Only the keys the selection reads are kept.
        assert_eq!(tags(&[("amenity", "cafe"), ("name", "Kaffee")]).0.len(), 1);
    }

    #[test]
    fn rings_that_will_not_close_are_returned_as_pieces() {
        let (rings, pieces) = join_rings(vec![vec![1, 2, 3], vec![3, 4], vec![7, 8]]);
        assert!(rings.is_empty());
        assert_eq!(pieces, vec![vec![1, 2, 3, 4], vec![7, 8]]);
        let (rings, pieces) = join_rings(vec![vec![1, 2], vec![3, 1], vec![2, 3]]);
        assert_eq!(rings, vec![vec![1, 2, 3, 1]]);
        assert!(pieces.is_empty());
    }
}
