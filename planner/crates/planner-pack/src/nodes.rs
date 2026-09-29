//! The deployed network: where the repeaters actually are.
//!
//! The `Nodes` layer format. The pack compiler never writes one — nodes
//! belong to nodesets, which stand on any ground that holds them — and
//! planner-web reads one when a pack still carries it. The CSV side reads and
//! writes the deployed-network CSV the importer and the community exchange.
//!
//! Two caveats live in the data model rather than in a README:
//!   * Advert positions are self-reported and unverified. Some are rounded to
//!     a street, some are the operator's flat rather than the mast on the roof
//!     three houses down. This layer is an input to planning, not ground truth.
//!   * `height_agl_m` and `tx_power_dbm` are absent for practically every
//!     scraped node, and they are exactly the two numbers propagation needs.
//!     So they are `Option`, they round-trip as ABSENT rather than as 0.0, and
//!     the CSV path below exists so a human can fill them in.
//!
//! The layer is small: the whole global advert dump is ~60 k nodes, of which
//! 45 k are repeaters, so a regional pack carries a few hundred records — a
//! rounding error next to one raster tile.
//!
//! Coordinates are stored PROJECTED into the pack CRS as f32 metres, the same
//! convention as roads.rs: the renderer draws these every frame and must not
//! reproject, and f32 holds ~0.5 m at UTM northings — far finer than the
//! accuracy of the positions themselves.
//!
//! Binary format (little-endian):
//!   magic "PND1" | u32 node_count
//!   per node: u8 kind | u8 id_len | id | u8 name_len | name
//!             | f32 x | f32 y | f32 height_agl_m | f32 tx_power_dbm
//!             | i64 last_seen_unix
//! Absent optionals are NaN (the two f32 fields) and i64::MIN (last seen).

use crate::PackError;
use std::io::{Read, Write};

/// What the node is. The discriminants deliberately equal the MeshCore advert
/// `type` codes (1 companion, 2 repeater, 3 room server, 4 sensor) so an
/// importer maps a scraped advert with a cast instead of a lookup table; 0 is
/// ours, for hand-entered rows and for advert types that do not exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Unknown = 0,
    /// A phone or handheld client. Not infrastructure — it moves with its
    /// owner, so it is drawn but never treated as a fixed site.
    Companion = 1,
    /// The one that matters for planning: a fixed store-and-forward repeater.
    Repeater = 2,
    RoomServer = 3,
    Sensor = 4,
}

impl NodeKind {
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => NodeKind::Unknown,
            1 => NodeKind::Companion,
            2 => NodeKind::Repeater,
            3 => NodeKind::RoomServer,
            4 => NodeKind::Sensor,
            _ => return None,
        })
    }

    pub fn code(self) -> u8 {
        self as u8
    }

    /// The spelling written to CSV. `from_label` accepts it back, which is
    /// what makes the export/edit/re-import round trip closed.
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Unknown => "unknown",
            NodeKind::Companion => "companion",
            NodeKind::Repeater => "repeater",
            NodeKind::RoomServer => "room",
            NodeKind::Sensor => "sensor",
        }
    }

    /// Parse the `kind` column. Accepts our own labels, the words people
    /// actually type, and the bare MeshCore advert number — community rows get
    /// pasted straight out of the map's JSON, where the kind is `2`.
    pub fn from_label(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        Some(match t.as_str() {
            "unknown" | "?" => NodeKind::Unknown,
            "companion" | "client" | "chat" | "1" => NodeKind::Companion,
            "repeater" | "relay" | "rep" | "2" => NodeKind::Repeater,
            "room" | "room_server" | "roomserver" | "room server" | "3" => NodeKind::RoomServer,
            "sensor" | "4" => NodeKind::Sensor,
            _ => return None,
        })
    }
}

/// One deployed node in the pack CRS.
///
/// Every field is public: the CLI and the importers build these directly from
/// whatever source they have (scraped adverts, a club's spreadsheet, a survey
/// form), and this crate deliberately knows nothing about those formats.
#[derive(Debug, Clone, PartialEq)]
pub struct DeployedNode {
    /// Stable identity — the MeshCore public key for scraped nodes, any stable
    /// string for hand-entered ones. Kept as-is so a re-import can match rows
    /// against the previous export instead of duplicating them.
    pub id: String,
    pub name: String,
    pub kind: NodeKind,
    /// Projected pack-CRS metres.
    pub x: f32,
    pub y: f32,
    /// Antenna height above ground. `None` means nobody has measured it — NOT
    /// zero, which is a legitimate value (a node sitting on the ground).
    pub height_agl_m: Option<f32>,
    /// `None` means unknown; 0.0 dBm is a legitimate value (1 mW).
    pub tx_power_dbm: Option<f32>,
    /// Last advert, Unix seconds. Freshness filtering happens upstream — this
    /// is carried so the UI can grey out a node nobody has heard from in a
    /// year instead of promising coverage that no longer exists.
    pub last_seen_unix: Option<i64>,
}

impl DeployedNode {
    /// Minimal constructor: position and identity, everything unknown. The
    /// unknowns are the honest default for scraped data.
    pub fn new(id: impl Into<String>, name: impl Into<String>, kind: NodeKind, x: f32, y: f32) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            x,
            y,
            height_agl_m: None,
            tx_power_dbm: None,
            last_seen_unix: None,
        }
    }

    /// Same, from geographic coordinates, projecting with the caller's
    /// closure. Every source of node positions is WGS84 lat/lon, so this is
    /// the constructor an importer actually reaches for; `to_xy` takes
    /// `(lat, lon)` like every projection closure in the compiler.
    pub fn from_wgs84(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: NodeKind,
        lat: f64,
        lon: f64,
        mut to_xy: impl FnMut(f64, f64) -> (f64, f64),
    ) -> Self {
        let (x, y) = to_xy(lat, lon);
        Self::new(id, name, kind, x as f32, y as f32)
    }
}

/// Attribution for the layer. Not a copyright license — a provenance warning,
/// which is the thing an operator has to see before trusting a dot on a map.
pub const NODES_NOTICE: &str =
    "Deployed-node positions are third-party advert data self-reported by node operators \
(e.g. the public MeshCore map), plus community corrections. Positions are unverified and may be \
approximate or out of date; antenna heights and transmit powers are unknown unless an operator \
supplied them.";

// ---------------------------------------------------------------------------
// Binary IO
// ---------------------------------------------------------------------------

fn put_str<W: Write>(w: &mut W, s: &str) -> Result<(), PackError> {
    // Cap at what the u8 length prefix can express. Cutting at byte 255 can
    // land inside a multi-byte codepoint — plenty of adv_name values carry
    // emoji and umlauts — and the reader would then lossily mangle the tail,
    // so back off to a char boundary.
    let mut n = s.len().min(255);
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    w.write_all(&[n as u8])?;
    w.write_all(&s.as_bytes()[..n])?;
    Ok(())
}

fn get_str<R: Read>(r: &mut R) -> Result<String, PackError> {
    let mut len = [0u8; 1];
    r.read_exact(&mut len)?;
    let mut buf = vec![0u8; len[0] as usize];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// "Unknown" for the two measured floats is NaN, not 0.0 or -1.0: zero is a
/// real height and a real power, and a sentinel that collides with a valid
/// measurement makes an operator unable to tell an unfilled field from one
/// they filled in. NaN is the only f32 no measurement produces.
fn put_opt_f32<W: Write>(w: &mut W, v: Option<f32>) -> Result<(), PackError> {
    w.write_all(&v.unwrap_or(f32::NAN).to_le_bytes())?;
    Ok(())
}

fn opt_f32(v: f32) -> Option<f32> {
    // NaN != NaN, so this cannot be an equality test against the sentinel.
    if v.is_nan() {
        None
    } else {
        Some(v)
    }
}

pub fn write_binary<W: Write>(w: &mut W, nodes: &[DeployedNode]) -> Result<(), PackError> {
    w.write_all(b"PND1")?;
    w.write_all(&(nodes.len() as u32).to_le_bytes())?;
    for n in nodes {
        // NaN is the "unknown" sentinel for height and power, but for a
        // POSITION it is a failed projection: `from_wgs84` hands the caller's
        // closure straight through, and the build closure reports a point
        // outside the CRS domain as NaN. Such a node writes fine, reads back
        // fine, and is counted in the build's "N deployed" line — while being
        // invisible on the map and unusable by propagation. Refuse it here,
        // because write_binary is the one gate every source of nodes (CSV,
        // scraper, struct literal) has to pass through.
        if !n.x.is_finite() || !n.y.is_finite() {
            return Err(PackError::Invalid(format!(
                "nodes: {:?} ({:?}) has no usable position ({}, {}) - it did not project into the pack CRS",
                n.id, n.name, n.x, n.y
            )));
        }
        w.write_all(&[n.kind.code()])?;
        put_str(w, &n.id)?;
        put_str(w, &n.name)?;
        w.write_all(&n.x.to_le_bytes())?;
        w.write_all(&n.y.to_le_bytes())?;
        put_opt_f32(w, n.height_agl_m)?;
        put_opt_f32(w, n.tx_power_dbm)?;
        // i64::MIN is the "never heard" sentinel: no real Unix time reaches it,
        // and unlike 0 it does not collide with 1970 — which is exactly what a
        // broken clock on a field node reports.
        w.write_all(&n.last_seen_unix.unwrap_or(i64::MIN).to_le_bytes())?;
    }
    Ok(())
}

pub fn read_binary<R: Read>(r: &mut R) -> Result<Vec<DeployedNode>, PackError> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"PND1" {
        return Err(PackError::Invalid("nodes: bad magic".into()));
    }
    let mut u32b = [0u8; 4];
    let mut f32b = [0u8; 4];
    let mut i64b = [0u8; 8];
    r.read_exact(&mut u32b)?;
    let count = u32::from_le_bytes(u32b) as usize;
    // Bounded pre-allocation: the count comes off disk, and a corrupt header
    // must not turn into a multi-gigabyte reserve before the first read fails.
    let mut nodes = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        let mut k = [0u8; 1];
        r.read_exact(&mut k)?;
        let kind = NodeKind::from_code(k[0])
            .ok_or_else(|| PackError::Invalid(format!("nodes: bad kind code {}", k[0])))?;
        let id = get_str(r)?;
        let name = get_str(r)?;
        r.read_exact(&mut f32b)?;
        let x = f32::from_le_bytes(f32b);
        r.read_exact(&mut f32b)?;
        let y = f32::from_le_bytes(f32b);
        r.read_exact(&mut f32b)?;
        let height_agl_m = opt_f32(f32::from_le_bytes(f32b));
        r.read_exact(&mut f32b)?;
        let tx_power_dbm = opt_f32(f32::from_le_bytes(f32b));
        r.read_exact(&mut i64b)?;
        let raw = i64::from_le_bytes(i64b);
        let last_seen_unix = if raw == i64::MIN { None } else { Some(raw) };
        nodes.push(DeployedNode {
            id,
            name,
            kind,
            x,
            y,
            height_agl_m,
            tx_power_dbm,
            last_seen_unix,
        });
    }
    Ok(nodes)
}

// ---------------------------------------------------------------------------
// CSV: the community-data path
// ---------------------------------------------------------------------------

/// The columns we understand. Anything else in the file is carried past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Id,
    Name,
    Kind,
    Lat,
    Lon,
    Height,
    Power,
    LastSeen,
}

/// Map one header cell onto a column. Deliberately generous: this file is
/// typed by people, exported from spreadsheets, and pasted out of the map's
/// JSON, so `adv_lat`, `Latitude` and `lat` all have to mean the same thing.
fn column_of(header: &str) -> Option<Col> {
    let h = header
        .trim()
        .trim_start_matches('\u{feff}') // Excel writes a BOM on the first cell
        .to_ascii_lowercase()
        .replace([' ', '-'], "_");
    Some(match h.as_str() {
        "id" | "node_id" | "public_key" | "pubkey" | "key" => Col::Id,
        "name" | "adv_name" | "label" => Col::Name,
        "kind" | "type" | "role" | "node_type" | "adv_type" => Col::Kind,
        "lat" | "latitude" | "adv_lat" => Col::Lat,
        "lon" | "lng" | "long" | "longitude" | "adv_lon" => Col::Lon,
        "height_agl_m" | "height_m" | "height" | "agl_m" | "antenna_height_m" => Col::Height,
        "tx_power_dbm" | "tx_power" | "power_dbm" | "power" | "tx_dbm" => Col::Power,
        // `last_advert` is what the MeshCore map actually calls this field, and
        // it is the one column a scraped export is guaranteed to carry. Without
        // the alias the column was ignored like any unknown one, so every node
        // arrived with last_seen = None and the freshness filter had nothing to
        // work with - silently, since most of that dump is one to two years
        // stale and a stale repeater looks exactly like a fresh one.
        "last_seen_unix" | "last_seen" | "last_advert_unix" | "last_advert" => Col::LastSeen,
        _ => return None,
    })
}

/// Split one line into fields, honouring double quotes and doubled quotes.
///
/// A plain `split(',')` is not enough: node names contain commas ("Berlin,
/// Kreuzberg") and quotes, and one such name silently shifts every later
/// column of that row — which shows up as a repeater in the North Sea rather
/// than as a parse error.
fn split_line(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            quoted = true;
        } else if c == delim {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// Pick the separator from the header row. German spreadsheets export `;`
/// (comma is their decimal point), and the Zensus CSV this project already
/// ingests is semicolon-separated, so guessing wrong is the common case, not
/// the exotic one.
fn delimiter_of(header: &str) -> char {
    if header.matches(';').count() > header.matches(',').count() {
        ';'
    } else {
        ','
    }
}

/// Where each known column landed in the header, resolved once instead of
/// re-scanned per row.
#[derive(Debug, Clone, Copy, Default)]
struct Columns {
    id: Option<usize>,
    name: Option<usize>,
    kind: Option<usize>,
    lat: Option<usize>,
    lon: Option<usize>,
    height: Option<usize>,
    power: Option<usize>,
    last_seen: Option<usize>,
}

impl Columns {
    fn from_header(header: &str, delim: char) -> Self {
        let mut c = Columns::default();
        for (i, cell) in split_line(header, delim).iter().enumerate() {
            // First occurrence wins: a duplicated column in a hand-merged file
            // must not silently shadow the one the operator filled in.
            let slot = match column_of(cell) {
                Some(Col::Id) => &mut c.id,
                Some(Col::Name) => &mut c.name,
                Some(Col::Kind) => &mut c.kind,
                Some(Col::Lat) => &mut c.lat,
                Some(Col::Lon) => &mut c.lon,
                Some(Col::Height) => &mut c.height,
                Some(Col::Power) => &mut c.power,
                Some(Col::LastSeen) => &mut c.last_seen,
                None => continue,
            };
            slot.get_or_insert(i);
        }
        c
    }
}

fn field<'a>(row: &'a [String], idx: Option<usize>) -> &'a str {
    // A short row (trailing empties dropped by a spreadsheet) reads as blank,
    // not as an error — blank optional fields are the norm in this file.
    idx.and_then(|i| row.get(i)).map(|s| s.trim()).unwrap_or("")
}

/// Parse a community node CSV, projecting with `to_xy` — which takes
/// `(lat, lon)`, like every projection closure in the compiler.
///
/// Columns `id,name,kind,lat,lon,height_agl_m,tx_power_dbm[,last_seen_unix]`
/// in any order; unknown columns are ignored, `#` comments and blank lines are
/// skipped, and a blank optional field stays `None` rather than becoming 0.0.
///
/// Malformed rows are REPORTED, with the line number as the operator's editor
/// counts it. Skipping them silently is the failure mode that matters here: a
/// club submits forty corrected heights, three rows have a decimal comma, and
/// nobody finds out that those three sites are still being planned with
/// guessed antenna heights.
pub fn parse_csv(
    text: &str,
    mut to_xy: impl FnMut(f64, f64) -> (f64, f64),
) -> Result<Vec<DeployedNode>, PackError> {
    let mut header: Option<Columns> = None;
    let mut delim = ',';
    let mut out = Vec::new();

    for (n, raw) in text.lines().enumerate() {
        let lineno = n + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(cols) = header else {
            delim = delimiter_of(line);
            let c = Columns::from_header(line, delim);
            if c.lat.is_none() || c.lon.is_none() {
                return Err(PackError::Invalid(format!(
                    "nodes csv line {lineno}: header has no lat/lon columns (got: {line})"
                )));
            }
            header = Some(c);
            continue;
        };
        let row = split_line(line, delim);

        // A row whose every cell is blank is a spreadsheet artifact (`,,,,` at
        // the end of an exported range), not a node and not a correction, so
        // there is nothing to report about it. Erroring here would fail a whole
        // regional build over a cosmetic trailing row.
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }

        // Two exports concatenated with `cat` leave a second header in the
        // middle of the file. Treat it as a header again, not as a node whose
        // latitude is the word "lat".
        if column_of(field(&row, cols.lat)) == Some(Col::Lat) {
            continue;
        }

        let (lat_s, lon_s) = (field(&row, cols.lat), field(&row, cols.lon));
        let (Ok(lat), Ok(lon)) = (lat_s.parse::<f64>(), lon_s.parse::<f64>()) else {
            return Err(PackError::Invalid(format!(
                "nodes csv line {lineno}: lat/lon not numeric (lat={lat_s:?}, lon={lon_s:?})"
            )));
        };
        // Catches the blatant half of the classic swapped-columns mistake, and
        // any row where a shifted field put a name where a number belongs.
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return Err(PackError::Invalid(format!(
                "nodes csv line {lineno}: lat/lon out of range ({lat}, {lon}) - columns swapped?"
            )));
        }

        let kind_s = field(&row, cols.kind);
        let kind = if kind_s.is_empty() {
            NodeKind::Unknown
        } else {
            NodeKind::from_label(kind_s).ok_or_else(|| {
                PackError::Invalid(format!(
                    "nodes csv line {lineno}: unknown kind {kind_s:?} \
                     (expected repeater/companion/room/sensor/unknown or a MeshCore type number)"
                ))
            })?
        };

        // Blank stays None; a present-but-unparseable value is an error. A
        // decimal comma from a German spreadsheet lands here, and it must be
        // fixed rather than dropped.
        let opt_num = |s: &str, what: &str| -> Result<Option<f32>, PackError> {
            if s.is_empty() {
                return Ok(None);
            }
            s.parse::<f32>().map(Some).map_err(|_| {
                PackError::Invalid(format!("nodes csv line {lineno}: {what} {s:?} is not a number"))
            })
        };
        let height_agl_m = opt_num(field(&row, cols.height), "height_agl_m")?;
        let tx_power_dbm = opt_num(field(&row, cols.power), "tx_power_dbm")?;
        let seen_s = field(&row, cols.last_seen);
        let last_seen_unix = if seen_s.is_empty() {
            None
        } else {
            Some(seen_s.parse::<i64>().map_err(|_| {
                PackError::Invalid(format!(
                    // The MeshCore map's own last_advert is an ISO timestamp,
                    // so name the expected unit rather than just refusing:
                    // this is the message an importer author reads.
                    "nodes csv line {lineno}: last_seen {seen_s:?} is not Unix seconds \
                     (convert an ISO timestamp at import time)"
                ))
            })?)
        };

        let (x, y) = to_xy(lat, lon);
        // The build closure reports a failed projection as NaN. Writing NaN
        // coordinates into the pack hides the node instead of the problem.
        if !x.is_finite() || !y.is_finite() {
            return Err(PackError::Invalid(format!(
                "nodes csv line {lineno}: ({lat}, {lon}) does not project into the pack CRS"
            )));
        }

        out.push(DeployedNode {
            id: field(&row, cols.id).to_string(),
            name: field(&row, cols.name).to_string(),
            kind,
            x: x as f32,
            y: y as f32,
            height_agl_m,
            tx_power_dbm,
            last_seen_unix,
        });
    }

    if header.is_none() {
        return Err(PackError::Invalid("nodes csv: no header row".into()));
    }
    Ok(keep_last_row_per_id(out))
}

/// Collapse rows that share an `id`, keeping the LAST one in the file.
///
/// The header-skip above exists because operators concatenate exports, and the
/// rows under that second header are usually the SAME nodes: without this the
/// pack carries every repeater twice, the build reports twice the deployed
/// count, and a coverage-gap analysis sees redundancy that does not exist.
/// Last wins because the appended file is the newer one - that is what makes
/// "scrape, then `cat` the club's corrections onto it" work, and it is the
/// merge semantic the `id` field was documented to provide.
///
/// A blank id is not an identity: hand-entered rows often have none, and they
/// are distinct nodes rather than one node listed many times.
fn keep_last_row_per_id(nodes: Vec<DeployedNode>) -> Vec<DeployedNode> {
    let mut last: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if !n.id.is_empty() {
            last.insert(n.id.as_str(), i);
        }
    }
    if last.len() == nodes.iter().filter(|n| !n.id.is_empty()).count() {
        return nodes; // nothing repeated, and the common case allocates nothing
    }
    let keep: Vec<bool> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| n.id.is_empty() || last[n.id.as_str()] == i)
        .collect();
    nodes
        .into_iter()
        .zip(keep)
        .filter_map(|(n, k)| k.then_some(n))
        .collect()
}

fn csv_escape(s: &str) -> String {
    // The reader is line-oriented (`text.lines()`), so a record MUST stay on
    // one line - quoting an embedded newline is not enough, and an adv_name
    // with one in it produced an export that its own parser rejected at the
    // following line, with a line number pointing at a row the operator can
    // see nothing wrong with. Losing the newline is the cheaper loss.
    let flat = if s.contains(['\n', '\r']) {
        s.replace(['\n', '\r'], " ")
    } else {
        s.to_string()
    };
    // A leading '#' is quoted as well: the first column starts the line, and
    // an unquoted "#12" id would come back as a comment — i.e. the re-import
    // would drop that node without a word.
    if flat.starts_with('#') || flat.contains([',', ';', '"']) {
        format!("\"{}\"", flat.replace('"', "\"\""))
    } else {
        flat
    }
}

/// Write the layer back out as the same CSV `parse_csv` reads, converting
/// projected metres back to lat/lon with `from_xy` (the inverse of the closure
/// given to `parse_csv`, taking `(x, y)` and returning `(lat, lon)`).
///
/// The round trip is the whole point: a scraped network is never complete or
/// correct, so the operator exports what we have, fixes the heights and the
/// obviously-wrong positions in a spreadsheet, and re-imports. Any field this
/// writer drops is a field the community can never contribute, so everything —
/// including `last_seen_unix` — goes out, and absent optionals go out BLANK so
/// they come back absent rather than as a confident zero.
pub fn write_csv<W: Write>(
    w: &mut W,
    nodes: &[DeployedNode],
    mut from_xy: impl FnMut(f64, f64) -> (f64, f64),
) -> Result<(), PackError> {
    writeln!(w, "id,name,kind,lat,lon,height_agl_m,tx_power_dbm,last_seen_unix")?;
    for n in nodes {
        let (lat, lon) = from_xy(n.x as f64, n.y as f64);
        // Six decimals is ~0.11 m of latitude: finer than any advert position
        // and finer than the f32 metres we store, while staying readable in a
        // spreadsheet. The other numbers use Rust's shortest round-tripping
        // form, so a hand-entered 12.5 comes back as 12.5.
        writeln!(
            w,
            "{},{},{},{:.6},{:.6},{},{},{}",
            csv_escape(&n.id),
            csv_escape(&n.name),
            n.kind.label(),
            lat,
            lon,
            n.height_agl_m.map(|v| v.to_string()).unwrap_or_default(),
            n.tx_power_dbm.map(|v| v.to_string()).unwrap_or_default(),
            n.last_seen_unix.map(|v| v.to_string()).unwrap_or_default(),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Identifying a node heard on the air
// ---------------------------------------------------------------------------

/// Which population a prefix was resolved against, and how OPEN it is.
///
/// "Unique in Berlin" is not "unique globally", and neither is "unique in this
/// pack" the same as "this is that node". Both figures travel with every match
/// because a reader who sees only `sole_known_candidate: true` will read it as
/// an identification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopulationId {
    /// What the pack is called, so a report line names its evidence.
    pub label: String,
    /// Nodes in it that carry a usable 32-byte public key.
    pub keyed_nodes: usize,
    /// Nodes EXCLUDED for having no key. "N of M nodes in this pack carry no
    /// public key and cannot be identified from the air" is a fact the operator
    /// needs: silently dropping them is how a survey concludes a repeater is
    /// unknown when it is merely un-keyed.
    pub unkeyed_nodes: usize,
}

/// The result of looking a prefix up. Never a node.
///
/// There is deliberately no `fn resolve(&self, p) -> Option<&DeployedNode>` on
/// [`PrefixIndex`]. The single-node answer is not merely discouraged, it is
/// unwritable, because at MeshCore's default one-byte path hash it is usually
/// wrong: measured over the 269 keyed Berlin repeaters a 1-byte prefix
/// identifies 112 of them (42%), leaving 157 colliding with a worst bucket of
/// four.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefixMatch {
    /// The prefix as it came off the air, in lowercase hex.
    pub prefix_hex: String,
    /// Its WIDTH in bytes. A match at one byte and a match at three are
    /// different claims and must never be printed the same way.
    pub prefix_len: usize,
    /// Indices into the node slice the index was built from, ascending.
    pub candidates: Vec<usize>,
    /// Exactly one KNOWN node is consistent with this prefix.
    ///
    /// Named at length on purpose. It is a statement about the PACK, not about
    /// the world: it does not mean "this is that node", because a node absent
    /// from the pack can share the prefix and usually does. See
    /// [`PrefixMatch::unlisted_node_collision_chance`].
    pub sole_known_candidate: bool,
    pub scope: PopulationId,
    /// How many of the possible prefix values AT THIS WIDTH are already taken
    /// by some node in the pack.
    ///
    /// This is the closed-world correction, and without it the match is
    /// misleading. Measured on the real advert dump, a 269-key population
    /// already occupies 170 of the 256 one-byte prefixes -- so a relay that is
    /// NOT in the pack still matches something about two times in three, and
    /// lands in a sole-occupant bucket (reported "unique") about one time in
    /// three. `candidates.is_empty()` is therefore NOT a reliable signal for
    /// "a repeater the layer does not know about" at one byte.
    pub occupied_prefixes: u64,
    /// Prefix values that exist at this width: 256^len, capped.
    pub prefix_space: u64,
}

impl PrefixMatch {
    /// Probability that a node NOT in this pack would nonetheless have matched
    /// something here, at this width. The number that turns
    /// `sole_known_candidate` from an identification into a candidate.
    pub fn unlisted_node_collision_chance(&self) -> f64 {
        if self.prefix_space == 0 {
            return 0.0;
        }
        self.occupied_prefixes as f64 / self.prefix_space as f64
    }
}

/// An offline index from public-key PREFIX to the nodes consistent with it.
///
/// WHY A PREFIX IS THE JOIN KEY AT ALL. MeshCore's `node_hash` is not a hash:
/// mcrs `protocol/src/crypto.rs` implements it as
/// `out[..copy_len].copy_from_slice(&public_key[..copy_len])`. Every identifier
/// on the air -- a path hash, a destination hash, a source hash -- is a prefix
/// of an ed25519 public key. This layer already stores the FULL 32 bytes in
/// [`DeployedNode::id`], with [`DeployedNode::name`] beside it. So a prefix
/// heard on a balcony joins offline to a known repeater and the name is free,
/// on the host, forever. Nothing about identification needs a byte of name
/// storage on the sensing node.
///
/// WHY IT LIVES HERE. Next to the data it indexes. Not in the shared no_std
/// record crate, which the firmware links and which has no business knowing the
/// pack format; not in the CLI, because this is pure testable data logic with
/// no command line in it. It adds no dependency.
#[derive(Debug, Clone)]
pub struct PrefixIndex {
    /// (key, index into the caller's node slice), sorted ascending by key.
    /// A few hundred entries: no hashing, no allocation per query.
    keys: Vec<([u8; 32], usize)>,
    scope: PopulationId,
}

impl PrefixIndex {
    /// Build over a node slice.
    ///
    /// A node is admitted ONLY if its `id` is exactly 64 hex characters
    /// decoding to 32 bytes. Hand-entered rows carry arbitrary ids (the `id`
    /// doc says "any stable string") and blank ids are explicitly not
    /// identities; matching an on-air prefix against those would fabricate an
    /// identification out of a spreadsheet cell.
    pub fn build(nodes: &[DeployedNode], label: impl Into<String>) -> Self {
        let mut keys: Vec<([u8; 32], usize)> = Vec::with_capacity(nodes.len());
        let mut unkeyed = 0usize;
        for (i, n) in nodes.iter().enumerate() {
            match planner_core::hex::decode_exact::<32>(&n.id) {
                Some(k) => keys.push((k, i)),
                None => unkeyed += 1,
            }
        }
        keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let scope = PopulationId {
            label: label.into(),
            keyed_nodes: keys.len(),
            unkeyed_nodes: unkeyed,
        };
        PrefixIndex { keys, scope }
    }

    pub fn scope(&self) -> &PopulationId {
        &self.scope
    }
    pub fn keyed_nodes(&self) -> usize {
        self.keys.len()
    }

    /// Every node consistent with `prefix`.
    ///
    /// Two `partition_point` calls give the contiguous range of keys sharing
    /// the prefix: exact, no false negatives, O(log n), and the SAME code path
    /// for widths 1, 2, 3 and 8. That uniformity is the point -- a per-width
    /// index (a 256-entry bucket table, a two-byte map) is three
    /// implementations that will drift.
    pub fn lookup(&self, prefix: &[u8]) -> PrefixMatch {
        let n = prefix.len().min(32);
        let lo = self.keys.partition_point(|(k, _)| k[..n] < prefix[..n]);
        let hi = self.keys.partition_point(|(k, _)| k[..n] <= prefix[..n]);
        let mut candidates: Vec<usize> = self.keys[lo..hi].iter().map(|(_, i)| *i).collect();
        candidates.sort_unstable();
        PrefixMatch {
            prefix_hex: planner_core::hex::encode(&prefix[..n]),
            prefix_len: n,
            sole_known_candidate: candidates.len() == 1,
            candidates,
            scope: self.scope.clone(),
            occupied_prefixes: self.occupied_prefixes(n),
            prefix_space: prefix_space(n),
        }
    }

    /// How many distinct prefix values of `len` bytes some node in this pack
    /// already occupies. Computed from the loaded index, never hardcoded: the
    /// figure is a property of the population that happens to be loaded, and a
    /// constant lifted from one Berlin pack would be off by orders of magnitude
    /// against the global dump.
    pub fn occupied_prefixes(&self, len: usize) -> u64 {
        let n = len.min(32);
        if n == 0 || self.keys.is_empty() {
            return 0;
        }
        let mut count = 1u64;
        for w in self.keys.windows(2) {
            if w[0].0[..n] != w[1].0[..n] {
                count += 1;
            }
        }
        count
    }

    /// Nodes uniquely identifiable at `len` bytes, and the largest collision
    /// bucket. THE number that says what an identification at this width is
    /// worth, computed against whatever population is actually loaded.
    pub fn uniqueness(&self, len: usize) -> (usize, usize) {
        let n = len.min(32);
        if self.keys.is_empty() {
            return (0, 0);
        }
        let mut unique = 0usize;
        let mut worst = 0usize;
        let mut run = 1usize;
        for i in 1..=self.keys.len() {
            let same = i < self.keys.len() && self.keys[i].0[..n] == self.keys[i - 1].0[..n];
            if same {
                run += 1;
            } else {
                if run == 1 {
                    unique += 1;
                }
                worst = worst.max(run);
                run = 1;
            }
        }
        (unique, worst)
    }
}

/// Prefix values expressible in `len` bytes, saturating so an 8-byte width does
/// not overflow into nonsense.
fn prefix_space(len: usize) -> u64 {
    if len == 0 {
        return 0;
    }
    if len >= 8 {
        return u64::MAX;
    }
    1u64 << (8 * len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<DeployedNode> {
        vec![
            DeployedNode {
                id: "01000001536e".into(),
                name: "BLN_Waldeckpark".into(),
                kind: NodeKind::Repeater,
                x: 13.402,
                y: 52.506,
                height_agl_m: Some(18.5),
                tx_power_dbm: Some(27.0),
                last_seen_unix: Some(1_753_811_012),
            },
            DeployedNode {
                id: "deadbeef".into(),
                name: "hand entered".into(),
                kind: NodeKind::Unknown,
                x: 13.5,
                y: 52.4,
                height_agl_m: None,
                tx_power_dbm: None,
                last_seen_unix: None,
            },
        ]
    }

    // -----------------------------------------------------------------
    // PrefixIndex: joining an on-air identifier to a deployed node
    // -----------------------------------------------------------------

    /// A deterministic population whose first bytes are CHOSEN, so the
    /// collision behaviour under test is a property of the fixture and not of
    /// whatever the scraper happened to return today.
    fn keyed(first: &[u8], name: &str) -> DeployedNode {
        let mut key = [0u8; 32];
        key[..first.len()].copy_from_slice(first);
        // Fill the tail from the name so two nodes sharing a prefix still
        // differ as full keys, which is the situation the index exists for.
        for (i, b) in name.bytes().enumerate() {
            if first.len() + i < 32 {
                key[first.len() + i] = b;
            }
        }
        DeployedNode::new(planner_core::hex::encode(&key), name, NodeKind::Repeater, 0.0, 0.0)
    }

    fn population() -> Vec<DeployedNode> {
        vec![
            keyed(&[0x3F, 0x01], "one"),
            keyed(&[0x3F, 0x02], "two"),
            keyed(&[0x3F, 0x03], "three"),
            keyed(&[0xA1, 0x10], "solo"),
            // No public key at all: a hand-entered candidate site.
            DeployedNode::new("kreuzberg-mast", "hand entered", NodeKind::Unknown, 0.0, 0.0),
        ]
    }

    #[test]
    fn a_one_byte_path_hash_resolves_to_a_candidate_set_and_never_to_a_single_node() {
        // MeshCore's default path hash is ONE byte, and there is no API here
        // that turns one into a node: `lookup` returns a set, and there is
        // deliberately no `resolve() -> Option<&DeployedNode>` to reach for.
        let nodes = population();
        let idx = PrefixIndex::build(&nodes, "test-pack");
        let m = idx.lookup(&[0x3F]);
        assert_eq!(m.candidates.len(), 3);
        assert!(!m.sole_known_candidate);
        assert_eq!(m.prefix_len, 1);
        assert_eq!(m.prefix_hex, "3f");
        let names: Vec<&str> = m.candidates.iter().map(|&i| nodes[i].name.as_str()).collect();
        assert_eq!(names, vec!["one", "two", "three"]);
    }

    #[test]
    fn a_unique_one_byte_match_is_reported_as_consistent_with_a_node_not_as_that_node() {
        let nodes = population();
        let idx = PrefixIndex::build(&nodes, "test-pack");
        let m = idx.lookup(&[0xA1]);
        assert_eq!(m.candidates.len(), 1);
        assert!(m.sole_known_candidate);
        // ... and the closed-world correction travels with it. Two of the 256
        // one-byte values are occupied here, so an unlisted node would still
        // have matched something 2/256 of the time -- small in this fixture,
        // and about two times in three against a real 269-node Berlin pack,
        // which is why the figure is carried rather than assumed negligible.
        assert_eq!(m.occupied_prefixes, 2);
        assert_eq!(m.prefix_space, 256);
        assert!((m.unlisted_node_collision_chance() - 2.0 / 256.0).abs() < 1e-12);
    }

    #[test]
    fn a_wider_prefix_narrows_the_candidate_set_to_one() {
        let nodes = population();
        let idx = PrefixIndex::build(&nodes, "test-pack");
        let m = idx.lookup(&[0x3F, 0x02]);
        assert_eq!(m.candidates.len(), 1);
        assert_eq!(nodes[m.candidates[0]].name, "two");
        assert_eq!(m.prefix_len, 2);
    }

    #[test]
    fn a_prefix_that_matches_no_known_node_is_reported_as_unmatched_not_dropped() {
        // A relay on air the deployed-node layer does not know about is a
        // FINDING -- a repeater somebody put up that this pack has never heard
        // of -- not an absence to skip past.
        let nodes = population();
        let idx = PrefixIndex::build(&nodes, "test-pack");
        let m = idx.lookup(&[0x77]);
        assert!(m.candidates.is_empty());
        assert!(!m.sole_known_candidate);
        assert_eq!(m.scope.keyed_nodes, 4);
    }

    #[test]
    fn a_node_without_a_public_key_is_excluded_from_the_prefix_index_and_counted() {
        // NOTE the fixture: this needs a HAND-ENTERED id, because every one of
        // the 59525 rows in the real advert dump carries a valid 64-hex key.
        // Against real data alone the excluded count is always zero and a
        // broken filter would never show.
        let nodes = population();
        let idx = PrefixIndex::build(&nodes, "test-pack");
        assert_eq!(idx.keyed_nodes(), 4);
        assert_eq!(idx.scope().unkeyed_nodes, 1);
        // And its id is not silently matchable as bytes.
        assert!(idx.lookup(b"kr").candidates.is_empty());
    }

    #[test]
    fn uniqueness_is_measured_against_the_loaded_population_not_hardcoded() {
        let nodes = population();
        let idx = PrefixIndex::build(&nodes, "test-pack");
        assert_eq!(idx.uniqueness(1), (1, 3), "one solo node, worst bucket three");
        assert_eq!(idx.uniqueness(2), (4, 1));
        assert_eq!(idx.occupied_prefixes(1), 2);
    }

    #[test]
    fn the_real_advert_dump_confirms_that_a_one_byte_prefix_identifies_almost_nothing() {
        // The claim this whole ambiguity policy rests on, checked against the
        // actual scraped population rather than against a remembered figure.
        // Skipped rather than failed when the cache is absent, the same rule
        // the importer's real-data tests already use.
        let path = std::env::var_os("SIMESH_MESHCORE_SNAPSHOT").map(std::path::PathBuf::from).unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../testbed/geodata/.cache/meshcore/nodes.json")
        });
        if !path.exists() {
            eprintln!("SKIP: MeshCore advert map not downloaded ({})", path.display());
            return;
        }
        let json = std::fs::read_to_string(&path).expect("cached MeshCore map");
        let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("a JSON array");

        let mut global = Vec::new();
        let mut berlin = Vec::new();
        for r in &rows {
            let Some(key) = r.get("public_key").and_then(|v| v.as_str()) else { continue };
            let node = DeployedNode::new(key, "", NodeKind::Repeater, 0.0, 0.0);
            let (lat, lon) = (
                r.get("adv_lat").and_then(|v| v.as_f64()),
                r.get("adv_lon").and_then(|v| v.as_f64()),
            );
            if let (Some(la), Some(lo)) = (lat, lon) {
                if (52.3..=52.7).contains(&la) && (13.05..=13.8).contains(&lo) {
                    berlin.push(node.clone());
                }
            }
            global.push(node);
        }

        let g = PrefixIndex::build(&global, "global advert dump");
        // 59525 keyed rows in the 2026 snapshot. At one byte the prefix space
        // is 256 values, so identification is essentially impossible.
        assert!(g.keyed_nodes() > 50_000, "snapshot had 59525, got {}", g.keyed_nodes());
        let (unique_1, worst_1) = g.uniqueness(1);
        assert!(unique_1 < 10, "one byte identifies {unique_1} of {} nodes", g.keyed_nodes());
        assert!(worst_1 > 200, "worst one-byte bucket held {worst_1}");
        // And THREE bytes is not 100% globally either, which is exactly why the
        // summary is computed from the loaded pack instead of quoting a table
        // measured on one small population.
        let (unique_3, _) = g.uniqueness(3);
        assert!(unique_3 < g.keyed_nodes(), "three bytes is not globally unique");

        let b = PrefixIndex::build(&berlin, "berlin bbox");
        assert!(b.keyed_nodes() > 300, "berlin bbox held {}", b.keyed_nodes());
        let (bu1, _) = b.uniqueness(1);
        assert!(
            bu1 * 3 < b.keyed_nodes(),
            "one byte identified {bu1} of {} Berlin nodes, which should be well under a third",
            b.keyed_nodes()
        );
        // Occupancy is the closed-world correction: most one-byte values are
        // already taken, so an unlisted node matches something more often than
        // not.
        assert!(b.occupied_prefixes(1) > 128, "occupancy {}", b.occupied_prefixes(1));
    }

    #[test]
    fn the_binary_round_trip_keeps_absent_optionals_absent() {
        let nodes = sample();
        let mut buf = Vec::new();
        write_binary(&mut buf, &nodes).unwrap();
        let back = read_binary(&mut &buf[..]).unwrap();
        assert_eq!(back, nodes);
        // The point of the NaN sentinel: unknown must not read back as 0.0,
        // which propagation would happily believe.
        assert_eq!(back[1].height_agl_m, None);
        assert_eq!(back[1].tx_power_dbm, None);
        assert_eq!(back[1].last_seen_unix, None);
        assert_eq!(back[0].height_agl_m, Some(18.5));
    }

    #[test]
    fn a_zero_height_is_not_confused_with_an_unknown_one() {
        // A node on the ground and a node nobody has measured are different
        // facts, and the format has to carry both.
        let nodes = vec![
            DeployedNode { height_agl_m: Some(0.0), tx_power_dbm: Some(0.0),
                           ..DeployedNode::new("a", "on the ground", NodeKind::Sensor, 1.0, 2.0) },
            DeployedNode::new("b", "unmeasured", NodeKind::Sensor, 1.0, 2.0),
        ];
        let mut buf = Vec::new();
        write_binary(&mut buf, &nodes).unwrap();
        let back = read_binary(&mut &buf[..]).unwrap();
        assert_eq!(back[0].height_agl_m, Some(0.0));
        assert_eq!(back[0].tx_power_dbm, Some(0.0));
        assert_eq!(back[1].height_agl_m, None);
        assert_eq!(back[1].tx_power_dbm, None);
    }

    #[test]
    fn a_last_seen_of_zero_is_a_broken_clock_not_a_missing_advert() {
        // Unix 0 is what a field node with a dead RTC reports; it must not
        // collide with "never heard".
        let n = DeployedNode { last_seen_unix: Some(0),
                               ..DeployedNode::new("c", "1970", NodeKind::Repeater, 0.0, 0.0) };
        let mut buf = Vec::new();
        write_binary(&mut buf, &[n]).unwrap();
        assert_eq!(read_binary(&mut &buf[..]).unwrap()[0].last_seen_unix, Some(0));
    }

    #[test]
    fn foreign_data_is_rejected() {
        assert!(read_binary(&mut &b"XXXX"[..]).is_err());
    }

    #[test]
    fn an_emoji_name_survives_the_binary_round_trip() {
        // Plenty of adv_name values carry emoji, and the length prefix is a
        // byte count, so the truncation has to respect char boundaries.
        let n = DeployedNode::new("k", "Berlin \u{1f4e1} Nord", NodeKind::Repeater, 1.0, 2.0);
        let mut buf = Vec::new();
        write_binary(&mut buf, &[n.clone()]).unwrap();
        assert_eq!(read_binary(&mut &buf[..]).unwrap()[0].name, n.name);
    }

    #[test]
    fn an_overlong_name_is_cut_on_a_char_boundary_not_mid_codepoint() {
        let n = DeployedNode::new("k", "\u{00e4}".repeat(200), NodeKind::Repeater, 1.0, 2.0);
        let mut buf = Vec::new();
        write_binary(&mut buf, &[n]).unwrap();
        let back = read_binary(&mut &buf[..]).unwrap();
        // 127 two-byte chars = 254 bytes; the 255th byte would split one.
        assert_eq!(back[0].name.chars().count(), 127);
        assert!(back[0].name.chars().all(|c| c == '\u{00e4}'), "no replacement chars");
    }

    #[test]
    fn csv_columns_may_arrive_in_any_order_with_comments_and_blank_optionals() {
        let csv = "\
# Berlin repeaters, corrected 2026-08 by the local group
lon,tx_power_dbm,name,kind,id,lat,height_agl_m
13.402,27,BLN_Waldeckpark,repeater,01000001536e,52.506,18.5

13.5,,\"Neukoelln, Rathaus\",2,abc,52.48,
";
        let nodes = parse_csv(csv, |lat, lon| (lon * 1000.0, lat * 1000.0)).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "BLN_Waldeckpark");
        assert_eq!(nodes[0].kind, NodeKind::Repeater);
        assert_eq!(nodes[0].height_agl_m, Some(18.5));
        assert!((nodes[0].x - 13402.0).abs() < 0.5);
        assert!((nodes[0].y - 52506.0).abs() < 0.5);
        // A quoted name keeps its comma, and the numeric MeshCore type parses.
        assert_eq!(nodes[1].name, "Neukoelln, Rathaus");
        assert_eq!(nodes[1].kind, NodeKind::Repeater);
        // Blank optionals stay unknown.
        assert_eq!(nodes[1].height_agl_m, None);
        assert_eq!(nodes[1].tx_power_dbm, None);
    }

    #[test]
    fn a_csv_round_trip_preserves_every_field_so_an_operator_can_edit_and_reimport() {
        let nodes = sample();
        let mut csv = Vec::new();
        // Identity "projection" both ways: this test is about the file, not
        // about proj.
        write_csv(&mut csv, &nodes, |x, y| (y, x)).unwrap();
        let text = String::from_utf8(csv).unwrap();
        let back = parse_csv(&text, |lat, lon| (lon, lat)).unwrap();
        assert_eq!(back.len(), nodes.len());
        for (a, b) in back.iter().zip(&nodes) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.name, b.name);
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.height_agl_m, b.height_agl_m);
            assert_eq!(a.tx_power_dbm, b.tx_power_dbm);
            assert_eq!(a.last_seen_unix, b.last_seen_unix);
            // Six decimals of degrees is ~0.1 m on the ground.
            assert!((a.x - b.x).abs() < 1e-4, "{} vs {}", a.x, b.x);
            assert!((a.y - b.y).abs() < 1e-4, "{} vs {}", a.y, b.y);
        }
    }

    #[test]
    fn a_hash_leading_id_survives_the_round_trip_instead_of_becoming_a_comment() {
        let n = DeployedNode::new("#12", "hash id", NodeKind::Repeater, 13.4, 52.5);
        let mut csv = Vec::new();
        write_csv(&mut csv, &[n], |x, y| (y, x)).unwrap();
        let back = parse_csv(std::str::from_utf8(&csv).unwrap(), |lat, lon| (lon, lat)).unwrap();
        assert_eq!(back.len(), 1, "the row was swallowed as a comment");
        assert_eq!(back[0].id, "#12");
    }

    #[test]
    fn a_malformed_row_is_reported_with_its_line_number_not_silently_dropped() {
        let csv = "id,name,kind,lat,lon\na1,ok,repeater,52.5,13.4\nb2,broken,repeater,fifty-two,13.4\n";
        let err = parse_csv(csv, |lat, lon| (lon, lat)).unwrap_err().to_string();
        assert!(err.contains("line 3"), "{err}");

        // A decimal comma (quoted, the way a German spreadsheet exports it) is
        // reported too, rather than leaving the operator believing their
        // corrected height was imported.
        let csv = "id,lat,lon,height_agl_m\nx,52.5,13.4,\"18,5\"\n";
        let err = parse_csv(csv, |lat, lon| (lon, lat)).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");

        // And an unrecognised kind is a typo worth telling someone about.
        let csv = "id,lat,lon,kind\nx,52.5,13.4,repaeter\n";
        let err = parse_csv(csv, |lat, lon| (lon, lat)).unwrap_err().to_string();
        assert!(err.contains("repaeter"), "{err}");
    }

    #[test]
    fn swapped_lat_lon_columns_are_reported_rather_than_projected_into_the_sea() {
        let csv = "id,lat,lon\nx,13.4,952.5\n";
        assert!(parse_csv(csv, |lat, lon| (lon, lat)).is_err());
    }

    #[test]
    fn a_semicolon_export_from_a_spreadsheet_still_parses() {
        let csv = "id;name;kind;lat;lon\nx;Funkturm;repeater;52.5;13.3\n";
        let nodes = parse_csv(csv, |lat, lon| (lon, lat)).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "Funkturm");
    }

    #[test]
    fn concatenated_exports_do_not_turn_the_second_header_into_a_node() {
        let csv = "id,name,lat,lon\na,A,52.5,13.4\nid,name,lat,lon\nb,B,52.6,13.5\n";
        let nodes = parse_csv(csv, |lat, lon| (lon, lat)).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].name, "B");
    }

    #[test]
    fn a_file_without_coordinates_is_rejected_up_front() {
        let csv = "id,name,kind\na,A,repeater\n";
        assert!(parse_csv(csv, |lat, lon| (lon, lat)).is_err());
    }

    #[test]
    fn concatenated_exports_of_the_same_network_do_not_double_the_node_count() {
        // The scenario the mid-file header skip exists for: the same export
        // appended to itself. Every repeater used to land in the pack twice,
        // so the build reported twice the deployed count and the network
        // looked twice as redundant as it is.
        let csv = "id,name,lat,lon\na,A,52.5,13.4\nb,B,52.6,13.5\n\
                   id,name,lat,lon\na,A,52.5,13.4\nb,B,52.6,13.5\n";
        let nodes = parse_csv(csv, |lat, lon| (lon, lat)).unwrap();
        assert_eq!(nodes.len(), 2, "ids: {:?}", nodes.iter().map(|n| &n.id).collect::<Vec<_>>());
    }

    #[test]
    fn a_correction_appended_after_a_scrape_wins_over_the_scraped_row() {
        // The reason the merge keeps the LAST row: the club's corrected
        // heights are `cat`-ed onto the scrape, and the scrape has none.
        let csv = "id,name,lat,lon,height_agl_m\nk1,Waldeckpark,52.506,13.402,\n\
                   k1,Waldeckpark (roof),52.506,13.402,18.5\n";
        let nodes = parse_csv(csv, |lat, lon| (lon, lat)).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].height_agl_m, Some(18.5));
        assert_eq!(nodes[0].name, "Waldeckpark (roof)");
    }

    #[test]
    fn rows_without_an_id_are_separate_nodes_rather_than_one_node_repeated() {
        // Hand-entered candidate sites have no public key, and a blank id is
        // not an identity two of them share.
        let csv = "id,name,lat,lon\n,site A,52.5,13.4\n,site B,52.6,13.5\n";
        assert_eq!(parse_csv(csv, |lat, lon| (lon, lat)).unwrap().len(), 2);
    }

    #[test]
    fn the_meshcore_last_advert_column_is_not_silently_ignored() {
        // `last_advert` is the map's own spelling. When it was not an alias the
        // column was skipped like any unknown one, and every node came back
        // with no freshness at all - on a dump where most adverts are a year
        // or two old, that is the difference between a live repeater and a
        // dead one.
        let csv = "public_key,adv_name,adv_lat,adv_lon,last_advert\nk1,BLN,52.5,13.4,1753811012\n";
        let nodes = parse_csv(csv, |lat, lon| (lon, lat)).unwrap();
        assert_eq!(nodes[0].last_seen_unix, Some(1_753_811_012));
    }

    #[test]
    fn a_node_that_failed_to_project_is_refused_instead_of_baked_in_invisible() {
        // from_wgs84 passes the caller's closure straight through, and the
        // build closure signals an out-of-domain point as NaN. Such a node
        // used to round-trip happily and be counted as deployed while being
        // undrawable and unusable.
        let n = DeployedNode::from_wgs84("k1", "off world", NodeKind::Repeater, 52.5, 13.4, |_, _| {
            (f64::NAN, f64::NAN)
        });
        let mut buf = Vec::new();
        let err = write_binary(&mut buf, &[n]).unwrap_err().to_string();
        assert!(err.contains("k1"), "{err}");
    }

    #[test]
    fn a_name_carrying_a_newline_still_exports_to_a_file_that_reimports() {
        // The reader is line-oriented, so an embedded newline used to split one
        // record across two lines and the export failed to reimport at all.
        let n = DeployedNode::new("k1", "Berlin\nNord", NodeKind::Repeater, 13.4, 52.5);
        let mut csv = Vec::new();
        write_csv(&mut csv, &[n], |x, y| (y, x)).unwrap();
        let back = parse_csv(std::str::from_utf8(&csv).unwrap(), |lat, lon| (lon, lat)).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Berlin Nord");
    }

    #[test]
    fn a_trailing_all_blank_spreadsheet_row_does_not_fail_the_build() {
        let csv = "id,name,kind,lat,lon\na,A,repeater,52.5,13.4\n,,,,\n";
        let nodes = parse_csv(csv, |lat, lon| (lon, lat)).unwrap();
        assert_eq!(nodes.len(), 1);
        // But a row that HAS content and no position is still an error: that is
        // a node whose coordinates were deleted, not an empty line.
        let csv = "id,name,kind,lat,lon\na,A,repeater,,\n";
        assert!(parse_csv(csv, |lat, lon| (lon, lat)).is_err());
    }

    #[test]
    fn an_unknown_kind_code_on_disk_is_an_error_not_a_guess() {
        let mut buf = Vec::new();
        write_binary(&mut buf, &[DeployedNode::new("a", "A", NodeKind::Repeater, 1.0, 2.0)]).unwrap();
        buf[8] = 9; // the kind byte of the first record
        assert!(read_binary(&mut &buf[..]).is_err());
    }

    #[test]
    fn a_truncated_pack_file_is_an_error_rather_than_a_short_node_list() {
        // A half-written nodes.bin must not read back as "the network is
        // smaller than it is" - that is the failure that looks like data.
        let mut buf = Vec::new();
        write_binary(&mut buf, &sample()).unwrap();
        buf.truncate(buf.len() - 5);
        assert!(read_binary(&mut &buf[..]).is_err());
    }
}


