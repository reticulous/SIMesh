//! Offline gazetteer: place names, street names and postal-code areas.
//!
//! This is what makes "where is Bernau?" or "show me 12043" answerable with
//! the network down. Everything is resolved at pack-build time from the
//! OpenStreetMap extract (`osm.rs` selects the features, [`GazetteerBuilder`]
//! indexes them) and stored PROJECTED into the pack CRS, so a lookup at
//! activation time is a string match and a pan — no geocoding service, no
//! reprojection, no round trip.
//!
//! Deliberately *not* a full house-number index. Berlin plus Brandenburg is
//! ~1.5 M addressable points; street-level plus postal areas answers the
//! operational question ("which streets does this repeater cover?") at a few
//! percent of the bytes. House numbers can be added later as a second file
//! without changing this one.
//!
//! Binary format (little-endian):
//!   magic "PGZ1"
//!   u32 entry_count
//!     per entry: u8 kind | u8 name_len | name | u8 ctx_len | ctx | f32 x | f32 y
//!   u32 area_count
//!     per area: u8 code_len | code | u16 ring_count
//!       per ring: u32 n | n × (f32 x, f32 y)

use crate::PackError;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

/// What a gazetteer hit refers to. Ordering matters: it is the tie-break for
/// equally good name matches, coarsest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    City = 0,
    Town = 1,
    Village = 2,
    Suburb = 3,
    Street = 4,
    /// A postal code with a boundary polygon in the area table.
    Postcode = 5,
    /// A named summit (`natural=peak`). Berlin's high ground is rubble hills
    /// like Teufelsberg — the sites a mesh actually hangs off, and none of
    /// them are tagged `place=*`.
    Peak = 6,
    /// A mast or tower (`man_made=mast|tower|communications_tower`).
    Tower = 7,
}

impl Kind {
    pub fn from_place_tag(v: &str) -> Option<Self> {
        Some(match v {
            "city" => Kind::City,
            "town" => Kind::Town,
            "village" | "hamlet" => Kind::Village,
            "suburb" | "quarter" | "neighbourhood" | "borough" => Kind::Suburb,
            _ => return None,
        })
    }
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => Kind::City,
            1 => Kind::Town,
            2 => Kind::Village,
            3 => Kind::Suburb,
            4 => Kind::Street,
            5 => Kind::Postcode,
            6 => Kind::Peak,
            7 => Kind::Tower,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::City => "city",
            Kind::Town => "town",
            Kind::Village => "village",
            Kind::Suburb => "district",
            Kind::Street => "street",
            Kind::Postcode => "postcode",
            Kind::Peak => "summit",
            Kind::Tower => "mast",
        }
    }

    /// Search tie-break rank, low wins. Deliberately NOT the enum
    /// discriminant: those are wire codes and must stay stable, while the
    /// ranking is a product decision. High ground and masts outrank streets
    /// because someone typing them is looking for a site, not an address.
    fn rank(self) -> i32 {
        match self {
            Kind::City => 0,
            Kind::Town => 1,
            Kind::Peak => 2,
            Kind::Tower => 3,
            Kind::Village => 4,
            Kind::Suburb => 5,
            Kind::Postcode => 6,
            Kind::Street => 7,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    /// Disambiguator shown after the name: the municipality for a street, the
    /// district for a suburb. Empty when there is nothing useful to add.
    pub ctx: String,
    /// Projected pack-CRS metres.
    pub x: f32,
    pub y: f32,
    /// Index into `Gazetteer::areas` for postcodes; `usize::MAX` otherwise.
    pub area: usize,
    /// `normalize(name)`, precomputed. Not stored on disk — recomputed on
    /// load, because normalizing 150 k names inside every keystroke's search
    /// is the difference between a 3 ms and a 90 ms response.
    pub norm: String,
}

/// A postal-code boundary: outer rings in projected metres.
#[derive(Debug, Clone)]
pub struct Area {
    pub code: String,
    pub rings: Vec<Vec<(f32, f32)>>,
}

#[derive(Debug, Clone, Default)]
pub struct Gazetteer {
    pub entries: Vec<Entry>,
    pub areas: Vec<Area>,
}

pub const OSM_NOTICE: &str =
    "Place, street and postal-code geometry \u{a9} OpenStreetMap contributors, ODbL 1.0 (opendatacommons.org/licenses/odbl)";

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Fold a name to a comparable key: lowercase, German umlauts expanded, and
/// the many spellings of "Straße" collapsed. Without the last part, "Haupt
/// Str" fails to find "Hauptstraße", which is most of what anyone types.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'ä' | 'Ä' => out.push_str("ae"),
            'ö' | 'Ö' => out.push_str("oe"),
            'ü' | 'Ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            'é' | 'è' | 'ê' => out.push('e'),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    // Collapse street-suffix spellings onto one token.
    for suffix in ["strasse", "str ", "str"] {
        if let Some(rest) = out.strip_suffix(suffix) {
            out = format!("{}strasse", rest.trim_end());
            break;
        }
    }
    let mut collapsed = String::with_capacity(out.len());
    let mut space = false;
    for c in out.chars() {
        if c == ' ' {
            space = true;
        } else {
            if space && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            space = false;
            collapsed.push(c);
        }
    }
    collapsed
}

/// Score a candidate against a normalized query. Higher is better; `None`
/// means no match at all.
fn score(hay: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return None;
    }
    if hay == needle {
        return Some(1000);
    }
    let pos = hay.find(needle)?;
    // Prefix beats word-start beats mid-word; shorter names beat longer ones,
    // so "Berlin" outranks "Berlingerode" for the query "berlin".
    let base = if pos == 0 {
        700
    } else if hay.as_bytes().get(pos.wrapping_sub(1)) == Some(&b' ') {
        500
    } else {
        250
    };
    Some(base - (hay.len() as i32 - needle.len() as i32).min(200))
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub kind: Kind,
    pub name: String,
    pub ctx: String,
    pub x: f32,
    pub y: f32,
    pub area: usize,
    pub score: i32,
}

impl Gazetteer {
    /// Rank entries against a raw user query. Cheap linear scan: even a
    /// full-Brandenburg gazetteer is a few hundred thousand short strings, and
    /// this keeps the format dependency-free.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let q = normalize(query);
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = Vec::new();
        for e in &self.entries {
            let Some(mut s) = score(&e.norm, &q) else { continue };
            // Coarser features win ties: a town is a likelier target than one
            // of the many streets that share its name.
            s -= e.kind.rank() * 4;
            hits.push(Hit {
                kind: e.kind,
                name: e.name.clone(),
                ctx: e.ctx.clone(),
                x: e.x,
                y: e.y,
                area: e.area,
                score: s,
            });
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.len().cmp(&b.name.len())));
        hits.dedup_by(|a, b| a.name == b.name && a.ctx == b.ctx && a.kind == b.kind);
        hits.truncate(limit);
        hits
    }
}

// ---------------------------------------------------------------------------
// Building the index
// ---------------------------------------------------------------------------

/// Accumulates gazetteer entries in pack-CRS metres. `osm.rs` decides what
/// is a place, a site, a street or a postal area; this decides how each one
/// is indexed.
#[derive(Default)]
pub struct GazetteerBuilder {
    g: Gazetteer,
    /// code -> index into `g.areas`, so a code met twice accumulates rings.
    area_of: HashMap<String, usize>,
    /// Street de-duplication: one entry per (name, rounded 2 km cell), which
    /// keeps "Bahnhofstraße" in twelve different towns as twelve hits without
    /// keeping all 900 segments of each.
    seen_street: HashSet<(String, i32, i32)>,
}

impl GazetteerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A postal-code boundary. Every ring is kept, open pieces included: an
    /// outline drawn from pieces still frames the district.
    pub fn postal_area(&mut self, code: &str, rings: Vec<Vec<(f32, f32)>>) {
        let rings: Vec<_> = rings.into_iter().filter(|r| r.len() >= 2).collect();
        if code.is_empty() || rings.is_empty() {
            return;
        }
        let idx = *self.area_of.entry(code.to_string()).or_insert_with(|| {
            self.g.areas.push(Area { code: code.to_string(), rings: Vec::new() });
            self.g.areas.len() - 1
        });
        self.g.areas[idx].rings.extend(rings);
    }

    /// A populated place, a summit or a mast, at one point. `ele` (metres,
    /// as tagged) becomes the context of a summit or mast.
    pub fn point(&mut self, kind: Kind, name: &str, ele: Option<&str>, x: f32, y: f32) {
        if name.is_empty() {
            return;
        }
        let ctx = match (kind, ele) {
            (Kind::Peak | Kind::Tower, Some(e)) => format!("{e} m"),
            _ => String::new(),
        };
        self.g.entries.push(Entry {
            kind,
            norm: normalize(name),
            name: name.to_string(),
            ctx,
            x,
            y,
            area: usize::MAX,
        });
    }

    /// A named street at one point, its postcode as context.
    pub fn street(&mut self, name: &str, postcode: Option<&str>, x: f64, y: f64) {
        if name.is_empty() || !x.is_finite() || !y.is_finite() {
            return;
        }
        let cell = ((x / 2000.0) as i32, (y / 2000.0) as i32);
        if !self.seen_street.insert((name.to_string(), cell.0, cell.1)) {
            return;
        }
        self.g.entries.push(Entry {
            norm: normalize(name),
            kind: Kind::Street,
            name: name.to_string(),
            ctx: postcode.unwrap_or_default().to_string(),
            x: x as f32,
            y: y as f32,
            area: usize::MAX,
        });
    }

    pub fn finish(self) -> Gazetteer {
        let mut g = self.g;
        postcode_entries(&mut g);
        g
    }
}

/// Every postal area becomes searchable by its code, centred on its own
/// outline so selecting it frames the district.
fn postcode_entries(g: &mut Gazetteer) {
    for (i, a) in g.areas.iter().enumerate() {
        let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0usize);
        for ring in &a.rings {
            for (x, y) in ring {
                sx += *x as f64;
                sy += *y as f64;
                n += 1;
            }
        }
        if n == 0 {
            continue;
        }
        g.entries.push(Entry {
            kind: Kind::Postcode,
            norm: normalize(&a.code),
            name: a.code.clone(),
            ctx: String::new(),
            x: (sx / n as f64) as f32,
            y: (sy / n as f64) as f32,
            area: i,
        });
    }
}

// ---------------------------------------------------------------------------
// Binary IO
// ---------------------------------------------------------------------------

fn put_str<W: Write>(w: &mut W, s: &str) -> Result<(), PackError> {
    let b = s.as_bytes();
    let n = b.len().min(255);
    w.write_all(&[n as u8])?;
    w.write_all(&b[..n])?;
    Ok(())
}

fn get_str<R: Read>(r: &mut R) -> Result<String, PackError> {
    let mut len = [0u8; 1];
    r.read_exact(&mut len)?;
    let mut buf = vec![0u8; len[0] as usize];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn write_binary<W: Write>(w: &mut W, g: &Gazetteer) -> Result<(), PackError> {
    w.write_all(b"PGZ1")?;
    w.write_all(&(g.entries.len() as u32).to_le_bytes())?;
    for e in &g.entries {
        w.write_all(&[e.kind as u8])?;
        put_str(w, &e.name)?;
        put_str(w, &e.ctx)?;
        w.write_all(&e.x.to_le_bytes())?;
        w.write_all(&e.y.to_le_bytes())?;
        let area = if e.area == usize::MAX { u32::MAX } else { e.area.min(u32::MAX as usize - 1) as u32 };
        w.write_all(&area.to_le_bytes())?;
    }
    w.write_all(&(g.areas.len() as u32).to_le_bytes())?;
    for a in &g.areas {
        put_str(w, &a.code)?;
        w.write_all(&(a.rings.len() as u16).to_le_bytes())?;
        for ring in &a.rings {
            w.write_all(&(ring.len() as u32).to_le_bytes())?;
            for (x, y) in ring {
                w.write_all(&x.to_le_bytes())?;
                w.write_all(&y.to_le_bytes())?;
            }
        }
    }
    Ok(())
}

pub fn read_binary<R: Read>(r: &mut R) -> Result<Gazetteer, PackError> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"PGZ1" {
        return Err(PackError::Invalid("gazetteer: bad magic".into()));
    }
    let mut u32b = [0u8; 4];
    let mut f32b = [0u8; 4];
    r.read_exact(&mut u32b)?;
    let n = u32::from_le_bytes(u32b) as usize;
    let mut entries = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let mut k = [0u8; 1];
        r.read_exact(&mut k)?;
        let kind = Kind::from_code(k[0]).ok_or_else(|| PackError::Invalid("bad kind".into()))?;
        let name = get_str(r)?;
        let ctx = get_str(r)?;
        r.read_exact(&mut f32b)?;
        let x = f32::from_le_bytes(f32b);
        r.read_exact(&mut f32b)?;
        let y = f32::from_le_bytes(f32b);
        r.read_exact(&mut u32b)?;
        // "no area" is written as a truncated usize::MAX; widen it back so the
        // sentinel still compares equal on 64-bit.
        let raw = u32::from_le_bytes(u32b);
        let area = if raw == u32::MAX { usize::MAX } else { raw as usize };
        entries.push(Entry { kind, norm: normalize(&name), name, ctx, x, y, area });
    }
    r.read_exact(&mut u32b)?;
    let na = u32::from_le_bytes(u32b) as usize;
    let mut areas = Vec::with_capacity(na.min(1 << 16));
    for _ in 0..na {
        let code = get_str(r)?;
        let mut u16b = [0u8; 2];
        r.read_exact(&mut u16b)?;
        let nr = u16::from_le_bytes(u16b) as usize;
        let mut rings = Vec::with_capacity(nr);
        for _ in 0..nr {
            r.read_exact(&mut u32b)?;
            let np = u32::from_le_bytes(u32b) as usize;
            let mut ring = Vec::with_capacity(np.min(1 << 20));
            for _ in 0..np {
                r.read_exact(&mut f32b)?;
                let x = f32::from_le_bytes(f32b);
                r.read_exact(&mut f32b)?;
                let y = f32::from_le_bytes(f32b);
                ring.push((x, y));
            }
            rings.push(ring);
        }
        areas.push(Area { code, rings });
    }
    Ok(Gazetteer { entries, areas })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umlauts_and_street_suffixes_fold_together() {
        assert_eq!(normalize("Hauptstraße"), "hauptstrasse");
        assert_eq!(normalize("Haupt Str."), "hauptstrasse");
        assert_eq!(normalize("Müllerstr"), "muellerstrasse");
        // The two spellings OSM actually carries must land on one key, so a
        // search for either finds both.
        assert_eq!(normalize("KÖPENICKER  STRASSE"), normalize("Köpenickerstraße"));
    }

    #[test]
    fn exact_and_prefix_matches_outrank_substrings() {
        let exact = score("berlin", "berlin").unwrap();
        let prefix = score("berlingerode", "berlin").unwrap();
        let word = score("neu berlin", "berlin").unwrap();
        let mid = score("oberlin", "berlin").unwrap();
        assert!(exact > prefix && prefix > word && word > mid);
    }

    #[test]
    fn search_prefers_the_town_over_a_street_of_the_same_name() {
        let g = Gazetteer {
            entries: vec![
                Entry { kind: Kind::Street, norm: "bernau".into(), name: "Bernau".into(),
                        ctx: String::new(), x: 0.0, y: 0.0, area: usize::MAX },
                Entry { kind: Kind::Town, norm: "bernau".into(), name: "Bernau".into(),
                        ctx: String::new(), x: 10.0, y: 10.0, area: usize::MAX },
            ],
            areas: Vec::new(),
        };
        let hits = g.search("bernau", 5);
        assert_eq!(hits[0].kind, Kind::Town);
    }

    #[test]
    fn postcodes_become_searchable_entries_pointing_at_their_outline() {
        let mut b = GazetteerBuilder::new();
        b.postal_area("12043", vec![vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]]);
        // A second piece of the same code joins the first area.
        b.postal_area("12043", vec![vec![(100.0, 100.0), (0.0, 0.0)]]);
        let g = b.finish();
        assert_eq!(g.areas.len(), 1);
        assert_eq!(g.areas[0].code, "12043");
        assert_eq!(g.areas[0].rings.len(), 2);
        let hits = g.search("12043", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Kind::Postcode);
        assert_eq!(hits[0].area, 0);
    }

    #[test]
    fn repeated_street_names_survive_in_different_towns_but_not_in_one() {
        // Same name twice inside one 2 km cell collapses; 50 km away it does not.
        let mut b = GazetteerBuilder::new();
        b.street("Bahnhofstrasse", None, 100.0, 100.0);
        b.street("Bahnhofstrasse", Some("10115"), 300.0, 100.0);
        b.street("Bahnhofstrasse", None, 50_100.0, 100.0);
        assert_eq!(b.finish().entries.len(), 2);
    }

    #[test]
    fn summits_and_masts_carry_their_elevation_and_outrank_streets() {
        let mut b = GazetteerBuilder::new();
        b.point(Kind::Peak, "Teufelsberg", Some("120"), 0.0, 0.0);
        b.point(Kind::Tower, "Funkturm", None, 10.0, 0.0);
        b.point(Kind::Town, "Bernau", Some("70"), 20.0, 0.0);
        b.street("Teufelsberger Weg", None, 30.0, 0.0);
        let g = b.finish();
        let hits = g.search("teufelsberg", 5);
        assert_eq!(hits[0].name, "Teufelsberg", "the summit must outrank the street");
        assert_eq!(hits[0].kind, Kind::Peak);
        assert_eq!(hits[0].ctx, "120 m");
        assert_eq!(g.search("funkturm", 3)[0].kind, Kind::Tower);
        assert_eq!(g.search("bernau", 3)[0].ctx, "", "a town's elevation is not its context");
    }

    #[test]
    fn binary_round_trips() {
        let g = Gazetteer {
            entries: vec![Entry { kind: Kind::Postcode, norm: "10115".into(),
                                  name: "10115".into(), ctx: "Mitte".into(),
                                  x: 1.5, y: -2.5, area: 0 }],
            areas: vec![Area { code: "10115".into(),
                               rings: vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]] }],
        };
        let mut buf = Vec::new();
        write_binary(&mut buf, &g).unwrap();
        let back = read_binary(&mut buf.as_slice()).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].name, "10115");
        assert_eq!(back.entries[0].ctx, "Mitte");
        assert_eq!(back.areas[0].rings[0].len(), 3);
    }

    #[test]
    fn the_no_area_sentinel_survives_the_u32_round_trip() {
        // usize::MAX truncated to u32 and widened again is 4_294_967_295, not
        // usize::MAX — which made every street look like it had a boundary.
        let g = Gazetteer {
            entries: vec![Entry { kind: Kind::Street, norm: "a".into(), name: "A".into(),
                                  ctx: String::new(), x: 0.0, y: 0.0, area: usize::MAX }],
            areas: Vec::new(),
        };
        let mut buf = Vec::new();
        write_binary(&mut buf, &g).unwrap();
        let back = read_binary(&mut buf.as_slice()).unwrap();
        assert_eq!(back.entries[0].area, usize::MAX);
    }

}
