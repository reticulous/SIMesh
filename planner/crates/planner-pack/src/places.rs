//! Offline gazetteer: place names, street names and postal-code areas.
//!
//! This is what makes "where is Bernau?" or "show me 12043" answerable with
//! the network down. Everything is resolved at pack-build time from an
//! Overpass export and stored PROJECTED into the pack CRS, so a lookup at
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
use std::collections::HashSet;
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
    /// them are tagged `place=*`, so they were unsearchable.
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
// Overpass ingest
// ---------------------------------------------------------------------------

fn extract_str(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) =
                        u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                    {
                        out.push(ch);
                    }
                }
                Some(esc) => out.push(esc),
                None => return Some(out),
            },
            _ => out.push(c),
        }
    }
    Some(out)
}

fn extract_num(s: &str, key: &str) -> Option<f64> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == '+'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Split the `elements` array into whole top-level elements by brace depth.
///
/// Splitting on `"type":` (the obvious shortcut) breaks on relations: a
/// `boundary=postal_code` relation carries its member ways inline, each with
/// its own `"type"` and `"geometry"`, and the relation's own tags come last.
/// Naive splitting therefore attaches the postcode to one member way and
/// throws the rest of the boundary away.
/// Handles SEVERAL concatenated Overpass documents: the runbook builds the
/// gazetteer input by `cat`-ing the areas export onto the streets export, so
/// stopping at the first array's `]` silently drops every street.
fn elements(json: &str) -> Vec<&str> {
    let bytes = json.as_bytes();
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = json[cursor..].find("\"elements\"") {
        let hdr = cursor + rel;
        let Some(open) = json[hdr..].find('[').map(|j| hdr + j + 1) else { break };
        let (mut depth, mut begin, mut in_str, mut esc) = (0usize, 0usize, false, false);
        let mut end = bytes.len();
        for i in open..bytes.len() {
            let c = bytes[i];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'{' => {
                    if depth == 0 {
                        begin = i;
                    }
                    depth += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(&json[begin..=i]);
                    }
                }
                b']' if depth == 0 => {
                    end = i + 1;
                    break;
                }
                _ => {}
            }
        }
        cursor = end.max(open);
    }
    out
}

/// The value of an element's OWN `"type"` — the first one in the element.
fn element_type(chunk: &str) -> Option<String> {
    extract_str(chunk, "\"type\":")
}

/// Every `"geometry":[...]` array in an element, in order. A postal-code
/// relation has one per member way, and all of them are part of the boundary.
fn all_geometries(
    chunk: &str,
    to_xy: &mut impl FnMut(f64, f64) -> (f64, f64),
) -> Vec<Vec<(f32, f32)>> {
    let mut out = Vec::new();
    let mut rest = chunk;
    while let Some(start) = rest.find("\"geometry\":") {
        let seg = &rest[start..];
        let Some(end) = seg.find(']') else { break };
        let mut pts = Vec::new();
        for pt in seg[..end].split('{').skip(1) {
            if let (Some(lat), Some(lon)) =
                (extract_num(pt, "\"lat\":"), extract_num(pt, "\"lon\":"))
            {
                let (x, y) = to_xy(lat, lon);
                pts.push((x as f32, y as f32));
            }
        }
        if !pts.is_empty() {
            out.push(pts);
        }
        rest = &seg[end..];
    }
    out
}

/// Collect `{"lat":..,"lon":..}` pairs out of one `"geometry":[...]` array,
/// falling back to the single `"center"` point that Overpass emits for
/// `out center;`. Streets are fetched that way: a gazetteer only needs one
/// point per street, and asking for full geometry multiplies the export by
/// an order of magnitude for nothing.
fn geometry(chunk: &str, to_xy: &mut impl FnMut(f64, f64) -> (f64, f64)) -> Vec<(f32, f32)> {
    let Some(start) = chunk.find("\"geometry\":") else {
        if let Some(c) = chunk.find("\"center\":") {
            let seg = &chunk[c..];
            let end = seg.find('}').unwrap_or(seg.len());
            if let (Some(lat), Some(lon)) = (
                extract_num(&seg[..end], "\"lat\":"),
                extract_num(&seg[..end], "\"lon\":"),
            ) {
                let (x, y) = to_xy(lat, lon);
                return vec![(x as f32, y as f32)];
            }
        }
        return Vec::new();
    };
    let geo = &chunk[start..];
    let Some(end) = geo.find(']') else { return Vec::new() };
    let mut pts = Vec::new();
    for pt in geo[..end].split('{').skip(1) {
        if let (Some(lat), Some(lon)) = (extract_num(pt, "\"lat\":"), extract_num(pt, "\"lon\":")) {
            let (x, y) = to_xy(lat, lon);
            pts.push((x as f32, y as f32));
        }
    }
    pts
}

/// Parse an Overpass export holding place nodes, named highways and
/// `boundary=postal_code` members. One pass, one file, three element kinds —
/// which is how the fetch script queries it.
pub fn parse_overpass(
    json: &str,
    mut to_xy: impl FnMut(f64, f64) -> (f64, f64),
) -> Gazetteer {
    let mut g = Gazetteer::default();
    // code -> index into g.areas, so multi-way postal relations accumulate.
    let mut area_of: Vec<(String, usize)> = Vec::new();
    // Street de-duplication: one entry per (name, rounded 2 km cell), which
    // keeps "Bahnhofstraße" in twelve different towns as twelve hits without
    // keeping all 900 segments of each. A hash set, not a scan: a Berlin-sized
    // export is ~150 k ways, and a linear membership test makes the build
    // quadratic.
    let mut seen_street: HashSet<(String, i32, i32)> = HashSet::new();

    for chunk in elements(json) {
        let is_node = element_type(chunk).as_deref() == Some("node");
        let place = extract_str(chunk, "\"place\":");
        let name = extract_str(chunk, "\"name\":");
        let postal = extract_str(chunk, "\"postal_code\":")
            .or_else(|| extract_str(chunk, "\"addr:postcode\":"));
        let boundary = extract_str(chunk, "\"boundary\":");

        // 1. Postal-code areas (relations/ways carrying boundary=postal_code).
        if boundary.as_deref() == Some("postal_code") {
            if let Some(code) = postal.clone().or_else(|| name.clone()) {
                // Every member way of the relation, not just the first.
                let rings: Vec<_> = all_geometries(chunk, &mut to_xy)
                    .into_iter()
                    .filter(|r| r.len() >= 2)
                    .collect();
                if !rings.is_empty() {
                    let idx = match area_of.iter().find(|(c, _)| *c == code) {
                        Some((_, i)) => *i,
                        None => {
                            g.areas.push(Area { code: code.clone(), rings: Vec::new() });
                            area_of.push((code.clone(), g.areas.len() - 1));
                            g.areas.len() - 1
                        }
                    };
                    g.areas[idx].rings.extend(rings);
                }
            }
            continue;
        }

        // 2. Populated places, summits and masts. The last two are why a
        // planner needs a gazetteer at all: they are the candidate sites.
        let natural = extract_str(chunk, "\"natural\":");
        let man_made = extract_str(chunk, "\"man_made\":");
        let site_kind = match (natural.as_deref(), man_made.as_deref()) {
            (Some("peak"), _) => Some(Kind::Peak),
            (_, Some("mast" | "tower" | "communications_tower")) => Some(Kind::Tower),
            _ => None,
        };
        if let (Some(kind), Some(n)) = (
            place.as_deref().and_then(Kind::from_place_tag).or(site_kind),
            name.as_deref(),
        ) {
            // Peaks and masts are mapped as nodes or as ways/areas; take a
            // centroid when there is geometry and the plain node position
            // otherwise.
            let pos = if is_node {
                extract_num(chunk, "\"lat\":")
                    .zip(extract_num(chunk, "\"lon\":"))
                    .map(|(lat, lon)| {
                        let (x, y) = to_xy(lat, lon);
                        (x as f32, y as f32)
                    })
            } else {
                let pts = geometry(chunk, &mut to_xy);
                if pts.is_empty() {
                    None
                } else {
                    let n = pts.len() as f32;
                    Some((
                        pts.iter().map(|p| p.0).sum::<f32>() / n,
                        pts.iter().map(|p| p.1).sum::<f32>() / n,
                    ))
                }
            };
            if let Some((x, y)) = pos {
                g.entries.push(Entry {
                    kind,
                    norm: normalize(n),
                    name: n.to_string(),
                    ctx: if kind == Kind::Peak || kind == Kind::Tower {
                        extract_str(chunk, "\"ele\":")
                            .map(|e| format!("{e} m"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    },
                    x,
                    y,
                    area: usize::MAX,
                });
            }
            continue;
        }
        if is_node {
            continue;
        }

        // 3. Named streets.
        if extract_str(chunk, "\"highway\":").is_some() {
            let Some(n) = name.clone() else { continue };
            let pts = geometry(chunk, &mut to_xy);
            if pts.is_empty() {
                continue;
            }
            let cx = pts.iter().map(|p| p.0 as f64).sum::<f64>() / pts.len() as f64;
            let cy = pts.iter().map(|p| p.1 as f64).sum::<f64>() / pts.len() as f64;
            let cell = ((cx / 2000.0) as i32, (cy / 2000.0) as i32);
            if !seen_street.insert((n.clone(), cell.0, cell.1)) {
                continue;
            }
            g.entries.push(Entry {
                norm: normalize(&n),
                kind: Kind::Street,
                name: n,
                ctx: postal.unwrap_or_default(),
                x: cx as f32,
                y: cy as f32,
                area: usize::MAX,
            });
        }
    }

    // Every postal area becomes searchable by its code, centred on its own
    // outline so selecting it frames the district.
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

    g
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
        let json = r#"
        {"elements":[
          {"type":"relation","tags":{"boundary":"postal_code","postal_code":"12043"},
           "geometry":[{"lat":52.0,"lon":13.0},{"lat":52.1,"lon":13.0},{"lat":52.1,"lon":13.1}]}
        ]}"#;
        let g = parse_overpass(json, |lat, lon| (lon * 1000.0, lat * 1000.0));
        assert_eq!(g.areas.len(), 1);
        assert_eq!(g.areas[0].code, "12043");
        let hits = g.search("12043", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, Kind::Postcode);
        assert_eq!(hits[0].area, 0);
    }

    #[test]
    fn repeated_street_names_survive_in_different_towns_but_not_in_one() {
        // Same name twice inside one 2 km cell collapses; 50 km away it does not.
        let json = r#"
        {"elements":[
          {"type":"way","tags":{"highway":"residential","name":"Bahnhofstrasse"},
           "geometry":[{"lat":0.0,"lon":0.0},{"lat":0.0,"lon":0.001}]},
          {"type":"way","tags":{"highway":"residential","name":"Bahnhofstrasse"},
           "geometry":[{"lat":0.0,"lon":0.002},{"lat":0.0,"lon":0.003}]},
          {"type":"way","tags":{"highway":"residential","name":"Bahnhofstrasse"},
           "geometry":[{"lat":0.0,"lon":50.0},{"lat":0.0,"lon":50.001}]}
        ]}"#;
        let g = parse_overpass(json, |lat, lon| (lon * 1000.0, lat * 1000.0));
        assert_eq!(g.entries.len(), 2);
    }

    #[test]
    fn streets_fetched_with_out_center_still_land_in_the_index() {
        let json = r#"{"elements":[
          {"type":"way","center":{"lat":52.5,"lon":13.4},
           "tags":{"highway":"residential","name":"Kollwitzstraße"}}
        ]}"#;
        let g = parse_overpass(json, |lat, lon| (lon * 1000.0, lat * 1000.0));
        assert_eq!(g.entries.len(), 1);
        assert_eq!(g.entries[0].kind, Kind::Street);
        assert!((g.entries[0].x - 13400.0).abs() < 1.0);
        assert!(!g.search("kollwitzstr", 3).is_empty());
    }

    #[test]
    fn a_postal_relation_keeps_every_member_way() {
        // Overpass nests member ways inside the relation, each with its own
        // "type" and "geometry", and puts the relation's tags last. Splitting
        // on "type" kept one fragment and dropped the rest of the boundary.
        let json = r#"{"elements":[
          {"type":"relation","id":7,
           "members":[
             {"type":"way","ref":1,"role":"outer",
              "geometry":[{"lat":0.0,"lon":0.0},{"lat":0.0,"lon":1.0}]},
             {"type":"way","ref":2,"role":"outer",
              "geometry":[{"lat":0.0,"lon":1.0},{"lat":1.0,"lon":1.0}]},
             {"type":"way","ref":3,"role":"outer",
              "geometry":[{"lat":1.0,"lon":1.0},{"lat":0.0,"lon":0.0}]}],
           "tags":{"boundary":"postal_code","postal_code":"10115"}}
        ]}"#;
        let g = parse_overpass(json, |lat, lon| (lon * 1000.0, lat * 1000.0));
        assert_eq!(g.areas.len(), 1);
        assert_eq!(g.areas[0].code, "10115");
        assert_eq!(g.areas[0].rings.len(), 3, "every member way must survive");
        // The boundary must span the full extent, not one 1 km fragment.
        let xs: Vec<f32> = g.areas[0].rings.iter().flatten().map(|p| p.0).collect();
        assert!((xs.iter().cloned().fold(f32::MIN, f32::max)
            - xs.iter().cloned().fold(f32::MAX, f32::min))
            > 900.0);
    }

    #[test]
    fn element_splitting_survives_braces_inside_strings() {
        let json = r#"{"elements":[
          {"type":"node","lat":1.0,"lon":2.0,"tags":{"place":"town","name":"Ha}{ha"}},
          {"type":"node","lat":3.0,"lon":4.0,"tags":{"place":"city","name":"Real"}}
        ]}"#;
        let els = elements(json);
        assert_eq!(els.len(), 2);
        let g = parse_overpass(json, |lat, lon| (lon, lat));
        assert_eq!(g.entries.len(), 2);
    }

    #[test]
    fn concatenated_exports_are_both_scanned() {
        // The runbook builds the input with `cat areas.json streets.json`.
        let a = r#"{"elements":[
          {"type":"node","lat":1.0,"lon":2.0,"tags":{"place":"town","name":"Alpha"}}
        ]}"#;
        let b = r#"{"elements":[
          {"type":"way","center":{"lat":3.0,"lon":4.0},
           "tags":{"highway":"residential","name":"Beta"}}
        ]}"#;
        let g = parse_overpass(&format!("{a}\n{b}"), |lat, lon| (lon * 1000.0, lat * 1000.0));
        assert_eq!(g.entries.len(), 2, "streets from the second document were dropped");
        assert!(g.entries.iter().any(|e| e.name == "Beta" && e.kind == Kind::Street));
    }

    #[test]
    fn summits_and_masts_are_searchable_sites() {
        // Teufelsberg is `natural=peak`, not `place=*` — it was unfindable,
        // which is backwards for a tool whose job is choosing high sites.
        let json = r#"{"elements":[
          {"type":"node","lat":52.4979,"lon":13.2413,
           "tags":{"natural":"peak","name":"Teufelsberg","ele":"120"}},
          {"type":"node","lat":52.5,"lon":13.3,
           "tags":{"man_made":"communications_tower","name":"Funkturm"}},
          {"type":"way","center":{"lat":52.5,"lon":13.3},
           "tags":{"highway":"residential","name":"Teufelsberger Weg"}}
        ]}"#;
        let g = parse_overpass(json, |lat, lon| (lon * 1000.0, lat * 1000.0));
        let hits = g.search("teufelsberg", 5);
        assert_eq!(hits[0].name, "Teufelsberg", "the summit must outrank the street");
        assert_eq!(hits[0].kind, Kind::Peak);
        assert_eq!(hits[0].ctx, "120 m");
        assert_eq!(g.search("funkturm", 3)[0].kind, Kind::Tower);
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

    #[test]
    fn unicode_escapes_in_overpass_names_are_decoded() {
        let json = r#"{"elements":[{"type":"node","lat":52.5,"lon":13.4,
            "tags":{"place":"city","name":"Köpenick"}}]}"#;
        let g = parse_overpass(json, |lat, lon| (lon, lat));
        assert_eq!(g.entries[0].name, "Köpenick");
        assert!(!g.search("koepenick", 3).is_empty());
    }
}
