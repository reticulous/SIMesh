//! Meshtastic Range Test module CSV importer.
//!
//! The de-facto wardriving format (review §8.2). Verified 2026-08-30 against
//! the firmware source (`src/modules/RangeTestModule.cpp`, master): header is
//! `time,from,sender name,sender lat,sender long,rx lat,rx long,rx elevation,
//! rx snr,distance,hop limit,payload,rx rssi` — note `rx rssi` DOES exist in
//! current firmware (older files lack it; the parser treats it as optional),
//! sender coordinates are present (0,0 when unknown), and `time` is a
//! date-less HH:MM:SS clock string (kept out of `time_unix`; epoch-style
//! values from other tools still parse). Columns resolve by fuzzy header
//! name so firmware variations keep working.

use crate::{Observation, PositionQuality, SourceTag};
use std::io::Read;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RangeTestError {
    #[error("csv error: {0}")]
    Csv(#[from] csv::Error),
    #[error("missing required column (aliases tried: {0})")]
    MissingColumn(&'static str),
}

fn norm(s: &str) -> String {
    s.trim()
        .trim_start_matches('\u{feff}')
        .to_ascii_lowercase()
        .replace(['_', '-'], " ")
}

fn find_col(headers: &[String], aliases: &[&str]) -> Option<usize> {
    headers.iter().position(|h| aliases.contains(&h.as_str()))
}

/// Parse a rangetest.csv stream into observations. Rows missing coordinates
/// or SNR are skipped rather than failing the whole file — field logs are
/// messy by nature.
pub fn parse<R: Read>(reader: R) -> Result<Vec<Observation>, RangeTestError> {
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(reader);

    let headers: Vec<String> = rdr.headers()?.iter().map(norm).collect();

    let col_snr = find_col(&headers, &["rx snr", "snr"])
        .ok_or(RangeTestError::MissingColumn("rx snr | snr"))?;
    let col_lat = find_col(&headers, &["rx lat", "lat", "latitude"])
        .ok_or(RangeTestError::MissingColumn("rx lat | lat | latitude"))?;
    let col_lon = find_col(&headers, &["rx long", "rx lon", "long", "lon", "longitude"])
        .ok_or(RangeTestError::MissingColumn("rx long | rx lon | lon | longitude"))?;
    // Optional columns.
    let col_time = find_col(&headers, &["time", "timestamp", "date"]);
    let col_from = find_col(&headers, &["from", "tx"]);
    let col_sender = find_col(&headers, &["sender name", "sender"]);
    let col_tx_lat = find_col(&headers, &["sender lat", "tx lat"]);
    let col_tx_lon = find_col(&headers, &["sender long", "sender lon", "tx lon"]);
    let col_rssi = find_col(&headers, &["rx rssi", "rssi"]);

    let mut out = Vec::new();
    for record in rdr.records() {
        let record = record?;
        let get = |i: usize| record.get(i).unwrap_or("").trim();
        let (Ok(lat), Ok(lon), Ok(snr)) = (
            get(col_lat).parse::<f64>(),
            get(col_lon).parse::<f64>(),
            get(col_snr).parse::<f32>(),
        ) else {
            continue;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }
        // Sender position: firmware writes 0,0 when unknown.
        let tx_pos = match (
            col_tx_lat.and_then(|i| get(i).parse::<f64>().ok()),
            col_tx_lon.and_then(|i| get(i).parse::<f64>().ok()),
        ) {
            (Some(tlat), Some(tlon))
                if (tlat, tlon) != (0.0, 0.0)
                    && (-90.0..=90.0).contains(&tlat)
                    && (-180.0..=180.0).contains(&tlon) =>
            {
                Some(planner_core::geo::GeoPos { lat_deg: tlat, lon_deg: tlon })
            }
            _ => None,
        };
        let tx_id = col_from
            .map(|i| get(i).to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| col_sender.map(|i| get(i).to_string()))
            .unwrap_or_default();
        out.push(Observation {
            // Firmware writes date-less HH:MM:SS — not representable as an
            // epoch, so it stays None; numeric timestamps still parse.
            time_unix: col_time.and_then(|i| get(i).parse::<i64>().ok()),
            tx_id,
            rx_id: String::new(),
            tx_pos,
            rx_pos: Some(planner_core::geo::GeoPos { lat_deg: lat, lon_deg: lon }),
            pos_quality: PositionQuality::GpsFix,
            snr_db: Some(snr),
            rssi_dbm: col_rssi.and_then(|i| get(i).parse::<f32>().ok()),
            freq_mhz: None,
            source: SourceTag::MeshtasticRangeTest,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_firmware_layout_verified_2026_08_30() {
        // Exact header from src/modules/RangeTestModule.cpp (master).
        let csv_data = "\
time,from,sender name,sender lat,sender long,rx lat,rx long,rx elevation,rx snr,distance,hop limit,payload,rx rssi
12:34:56,305419896,Kiez Node,52.5100,13.3900,52.5200,13.4050,34,7.25,1234,3,\"seq 1\",-87
12:35:56,305419896,Kiez Node,0,0,52.5210,13.4060,35,-3.5,0,3,\"seq 2\",-101
??:??:??,305419896,Kiez Node,52.5100,13.3900,,,36,,600,3,\"seq 3\",-90
";
        let obs = parse(csv_data.as_bytes()).unwrap();
        assert_eq!(obs.len(), 2, "row with empty coords/snr is skipped");
        assert_eq!(obs[0].tx_id, "305419896");
        assert_eq!(obs[0].snr_db, Some(7.25));
        assert_eq!(obs[0].rssi_dbm, Some(-87.0), "current firmware writes rx rssi");
        assert!(obs[0].time_unix.is_none(), "HH:MM:SS has no date — stays None");
        let tx = obs[0].tx_pos.unwrap();
        assert!((tx.lat_deg - 52.51).abs() < 1e-9);
        let p = obs[0].rx_pos.unwrap();
        assert!((p.lat_deg - 52.52).abs() < 1e-9);
        // Sender 0,0 = unknown → None.
        assert!(obs[1].tx_pos.is_none());
        assert_eq!(obs[1].rssi_dbm, Some(-101.0));
        assert_eq!(obs[0].source, SourceTag::MeshtasticRangeTest);
    }

    #[test]
    fn parses_older_layout_without_rssi_and_sender() {
        let csv_data = "\
time,from,rx lat,rx long,rx elevation,rx snr,distance,hop limit,payload
1714000000,!a1b2c3d4,52.5200,13.4050,34,7.25,150,3,seq 1
";
        let obs = parse(csv_data.as_bytes()).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].tx_id, "!a1b2c3d4");
        assert!(obs[0].rssi_dbm.is_none(), "older files lack rx rssi");
        assert!(obs[0].tx_pos.is_none());
        assert_eq!(obs[0].time_unix, Some(1714000000), "epoch-style time parses");
    }

    #[test]
    fn missing_snr_column_is_an_error() {
        let csv_data = "time,rx lat,rx long\n1,52.0,13.0\n";
        assert!(matches!(
            parse(csv_data.as_bytes()),
            Err(RangeTestError::MissingColumn(_))
        ));
    }
}
