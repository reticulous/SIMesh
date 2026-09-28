//! Building heights. The pack compiler ingests footprints (Länder LoD2
//! CityGML, OpenStreetMap); this crate holds where a height came from and the
//! rules that turn OpenStreetMap tags into one.

use serde::{Deserialize, Serialize};

/// Where a building's height came from — kept per building so calibration and
/// UIs can weigh trust. Serialized as the `source` field of a
/// `buildings.jsonl` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeightSource {
    /// Official LoD1/LoD2 (lidar-derived) — the German gold tier.
    Lod2,
    /// OSM `height` tag (includes roof).
    OsmHeight,
    /// OSM `building:levels` × [`M_PER_LEVEL`], plus a roof.
    OsmLevels,
    /// Overture / Microsoft ML / EUBUCCO modeled height.
    Modeled,
    /// GHS-OBAT / GHS-BUILT-H gridded estimate.
    Gridded,
    /// Community-entered (local override layer).
    Crowd,
    /// Class default by `building=*` — last resort.
    Default,
}

impl HeightSource {
    /// A height someone measured or tagged, as against a class default.
    pub fn is_tagged(self) -> bool {
        !matches!(self, HeightSource::Default)
    }
}

/// Meters per storey — literature range 2.8–3.5 m (GeoClimate/Bocher 2022).
pub const M_PER_LEVEL: f32 = 3.0;

/// Extra height for a pitched roof when only levels are known (OSM semantics:
/// `building:levels` excludes roof levels).
pub const PITCHED_ROOF_M: f32 = 2.0;

pub fn height_from_levels(levels: u8, pitched_roof: bool) -> f32 {
    levels as f32 * M_PER_LEVEL + if pitched_roof { PITCHED_ROOF_M } else { 0.0 }
}

/// A length tag in metres: `12`, `12.5`, `12 m`, `12,5`, `40'`, `40 ft`,
/// `40'6"`. Only the first of several `;`-separated values counts. `None` for
/// anything unreadable, non-positive or taller than any building.
pub fn parse_length_m(s: &str) -> Option<f32> {
    let s = s.split(';').next()?.trim();
    let feet = s.contains('\'') || s.ends_with("ft") || s.ends_with("feet");
    let (num, rest) = leading_number(s)?;
    let mut v = num;
    if feet {
        // `40'6"`: inches follow the foot mark.
        let inches = rest
            .trim_start()
            .strip_prefix('\'')
            .and_then(|r| leading_number(r.trim_start()))
            .map(|(i, _)| i)
            .unwrap_or(0.0);
        v = (num + inches / 12.0) * 0.3048;
    }
    (v.is_finite() && v > 0.0 && v < 1000.0).then_some(v)
}

/// A storey count: the first `;`-separated value, rounded. Zero (a roof or a
/// carport mapped with no storeys) and absurd counts give `None`.
pub fn parse_levels(s: &str) -> Option<u8> {
    let (v, _) = leading_number(s.split(';').next()?.trim())?;
    let n = v.round();
    (1.0..=200.0).contains(&n).then_some(n as u8)
}

fn leading_number(s: &str) -> Option<(f32, &str)> {
    let end = s
        .char_indices()
        .find(|&(_, c)| !(c.is_ascii_digit() || c == '.' || c == ','))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let v: f32 = s[..end].replace(',', ".").parse().ok()?;
    Some((v, &s[end..]))
}

/// Height of a building of this `building=*` value when nothing better is
/// tagged, metres above ground to the top of the roof.
pub fn class_default_m(building: &str) -> f32 {
    match building {
        "garage" | "garages" | "carport" | "shed" | "hut" | "kiosk" | "roof" | "toilets"
        | "service" | "container" | "bunker" | "guardhouse" => 3.0,
        "greenhouse" | "allotment_house" => 4.0,
        "barn" | "farm_auxiliary" | "stable" | "cowshed" | "sty" => 6.0,
        "house" | "detached" | "semidetached_house" | "terrace" | "bungalow" | "farm"
        | "cabin" | "static_caravan" => 8.0,
        "industrial" | "warehouse" | "manufacture" | "hangar" | "storage_tank" | "supermarket" => {
            10.0
        }
        "commercial" | "office" | "retail" | "hotel" | "school" | "university" | "college"
        | "kindergarten" | "hospital" | "public" | "civic" | "government" | "train_station"
        | "transportation" | "sports_hall" | "stadium" => 12.0,
        "apartments" | "residential" | "dormitory" => 15.0,
        "church" | "cathedral" | "mosque" | "synagogue" | "temple" => 20.0,
        _ => 9.0,
    }
}

/// Height of an OpenStreetMap building from its tags, and where it came from.
///
/// The `height` tag wins; else `building:levels` × [`M_PER_LEVEL`] plus a
/// pitched roof unless `roof:shape=flat`; else the class default for the
/// `building=*` value. `None` when the object is not a building
/// (`building=no` or no `building` tag).
pub fn osm_height<'a>(tag: impl Fn(&str) -> Option<&'a str>) -> Option<(f32, HeightSource)> {
    let building = tag("building")?;
    if building == "no" {
        return None;
    }
    if let Some(h) = tag("height").and_then(parse_length_m) {
        return Some((h, HeightSource::OsmHeight));
    }
    if let Some(levels) = tag("building:levels").and_then(parse_levels) {
        let pitched = tag("roof:shape").map_or(true, |s| s != "flat");
        return Some((height_from_levels(levels, pitched), HeightSource::OsmLevels));
    }
    Some((class_default_m(building), HeightSource::Default))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags<'a>(kv: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> + 'a {
        move |k| kv.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v)
    }

    #[test]
    fn levels_heuristic() {
        assert_eq!(height_from_levels(5, false), 15.0);
        assert_eq!(height_from_levels(5, true), 17.0);
    }

    #[test]
    fn the_height_tag_wins_over_levels_and_class() {
        let t = [("building", "apartments"), ("height", "21.5"), ("building:levels", "4")];
        assert_eq!(osm_height(tags(&t)), Some((21.5, HeightSource::OsmHeight)));
    }

    #[test]
    fn levels_come_next_with_a_pitched_roof_unless_it_is_flat() {
        let t = [("building", "yes"), ("building:levels", "5")];
        assert_eq!(osm_height(tags(&t)), Some((17.0, HeightSource::OsmLevels)));
        let t = [("building", "yes"), ("building:levels", "5"), ("roof:shape", "gabled")];
        assert_eq!(osm_height(tags(&t)), Some((17.0, HeightSource::OsmLevels)));
        let t = [("building", "yes"), ("building:levels", "5"), ("roof:shape", "flat")];
        assert_eq!(osm_height(tags(&t)), Some((15.0, HeightSource::OsmLevels)));
        // An unreadable height falls through to the levels.
        let t = [("building", "yes"), ("height", "tall"), ("building:levels", "2")];
        assert_eq!(osm_height(tags(&t)), Some((8.0, HeightSource::OsmLevels)));
    }

    #[test]
    fn the_class_default_is_the_last_resort() {
        let t = [("building", "garage")];
        assert_eq!(osm_height(tags(&t)), Some((3.0, HeightSource::Default)));
        let t = [("building", "church")];
        assert_eq!(osm_height(tags(&t)), Some((20.0, HeightSource::Default)));
        let t = [("building", "yes"), ("building:levels", "0")];
        assert_eq!(osm_height(tags(&t)), Some((9.0, HeightSource::Default)));
        assert!(!HeightSource::Default.is_tagged());
        assert!(HeightSource::OsmLevels.is_tagged() && HeightSource::OsmHeight.is_tagged());
    }

    #[test]
    fn not_a_building_has_no_height() {
        assert_eq!(osm_height(tags(&[("building", "no"), ("height", "10")])), None);
        assert_eq!(osm_height(tags(&[("height", "10")])), None);
    }

    #[test]
    fn length_tags_in_the_spellings_mappers_use() {
        assert_eq!(parse_length_m("12"), Some(12.0));
        assert_eq!(parse_length_m("12 m"), Some(12.0));
        assert_eq!(parse_length_m("12m"), Some(12.0));
        assert_eq!(parse_length_m("12,5"), Some(12.5));
        assert_eq!(parse_length_m("10;14"), Some(10.0));
        assert!((parse_length_m("40'").unwrap() - 12.192).abs() < 1e-3);
        assert!((parse_length_m("40 ft").unwrap() - 12.192).abs() < 1e-3);
        assert!((parse_length_m("40'6\"").unwrap() - 12.344).abs() < 1e-3);
        assert_eq!(parse_length_m("0"), None);
        assert_eq!(parse_length_m("-3"), None);
        assert_eq!(parse_length_m("approx"), None);
        assert_eq!(parse_levels("3.4"), Some(3));
        assert_eq!(parse_levels("2;3"), Some(2));
        assert_eq!(parse_levels("0"), None);
    }

    #[test]
    fn sources_serialize_as_the_jsonl_field_values() {
        let s = |h: HeightSource| serde_json::to_string(&h).unwrap();
        assert_eq!(s(HeightSource::Lod2), "\"lod2\"");
        assert_eq!(s(HeightSource::OsmHeight), "\"osm_height\"");
        assert_eq!(s(HeightSource::OsmLevels), "\"osm_levels\"");
        assert_eq!(s(HeightSource::Default), "\"default\"");
    }
}
