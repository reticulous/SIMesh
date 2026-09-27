//! Zensus 2022 100 m population grid → Population layer.
//!
//! Source: destatis "Zensus2022_Bevoelkerungszahl.zip" (18.5 MB; CSV
//! `GITTER_ID_100m;x_mp_100m;y_mp_100m;Einwohner`, EPSG:3035 cell midpoints,
//! ~3.09 M populated cells; verified 2026-08-31). License dl-de/by-2-0 with
//! destatis attribution.
//!
//! Population is spread uniformly over each 100 m cell and accumulated onto
//! the pack grid as persons-per-pack-cell (approximately conserving totals).

use crate::PackError;
use proj4rs::Proj;
use std::io::{BufRead, BufReader, Read};

pub const ZENSUS_NOTICE: &str =
    "\u{a9} Statistisches Bundesamt (Destatis), Zensus 2022 \u{2014} Datenlizenz Deutschland \u{2013} Namensnennung \u{2013} Version 2.0 (dl-de/by-2-0)";

pub fn laea_proj() -> Result<Proj, PackError> {
    Proj::from_proj_string(
        "+proj=laea +lat_0=52 +lon_0=10 +x_0=4321000 +y_0=3210000 +ellps=GRS80 +units=m +no_defs",
    )
    .map_err(|e| PackError::Proj(format!("laea/EPSG:3035: {e}")))
}

/// Stream the CSV; for every populated 100 m cell inside the caller's region
/// (decided by `target_index`), add its population share to the pack grid.
/// `to_target` converts EPSG:3035 → the pack CRS.
pub fn accumulate_population<R: Read>(
    reader: R,
    mut to_target: impl FnMut(f64, f64) -> Result<(f64, f64), PackError>,
    mut target_index: impl FnMut(f64, f64) -> Option<usize>,
    res_m: f64,
    // f32 by the footprint rule: persons-per-cell magnitudes lose nothing,
    // and a full-region grid halves to one layer-equivalent.
    population: &mut [f32],
) -> Result<u64, PackError> {
    let mut total_in_region = 0u64;
    // Sub-sample each 100 m square finely enough that every overlapped pack
    // cell receives its share.
    let steps = ((100.0 / res_m).ceil() as i32).max(1);
    for line in BufReader::new(reader).lines() {
        let line = line?;
        let mut it = line.split(';');
        let (Some(_id), Some(xs), Some(ys), Some(es)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let (Ok(x), Ok(y), Ok(pop)) =
            (xs.trim().parse::<f64>(), ys.trim().parse::<f64>(), es.trim().parse::<f64>())
        else {
            continue; // header and malformed rows
        };
        if pop <= 0.0 {
            continue;
        }
        // Cheap reject before projecting: transform the midpoint, check the
        // region, then spread over sub-samples.
        let (tx, ty) = to_target(x, y)?;
        if target_index(tx, ty).is_none() {
            continue;
        }
        total_in_region += pop as u64;
        let mut placed = 0usize;
        let mut hits: Vec<usize> = Vec::with_capacity((steps * steps) as usize);
        for iy in 0..steps {
            for ix in 0..steps {
                let sx = x - 50.0 + (ix as f64 + 0.5) * (100.0 / steps as f64);
                let sy = y - 50.0 + (iy as f64 + 0.5) * (100.0 / steps as f64);
                let (px, py) = to_target(sx, sy)?;
                if let Some(idx) = target_index(px, py) {
                    hits.push(idx);
                    placed += 1;
                }
            }
        }
        if placed > 0 {
            // Uniform split over the sub-samples that landed in the region —
            // conserves each cell's population exactly.
            let split = (pop / placed as f64) as f32;
            for idx in hits {
                population[idx] += split;
            }
        }
    }
    Ok(total_in_region)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The review flagged "verify EPSG:3035 parity early" — this is that
    /// check: proj4rs must support LAEA with GRS80 well enough to place a
    /// known south-German cell midpoint in the right few-km neighborhood and
    /// round-trip to sub-meter.
    #[test]
    fn laea_projection_parity() {
        let laea = laea_proj().expect("proj4rs supports +proj=laea");
        let ll = Proj::from_proj_string("+proj=longlat +ellps=GRS80 +no_defs").unwrap();
        // First data row of the real CSV: E 4337050, N 2689150.
        let mut pt = (4337050.0, 2689150.0, 0.0);
        proj4rs::transform::transform(&laea, &ll, &mut pt).unwrap();
        let (lon, lat) = (pt.0.to_degrees(), pt.1.to_degrees());
        assert!((9.5..=11.0).contains(&lon), "lon {lon}");
        assert!((46.8..=48.2).contains(&lat), "lat {lat}");
        // Round-trip.
        let mut back = (pt.0, pt.1, 0.0);
        proj4rs::transform::transform(&ll, &laea, &mut back).unwrap();
        assert!((back.0 - 4337050.0).abs() < 0.5, "{}", back.0);
        assert!((back.1 - 2689150.0).abs() < 0.5, "{}", back.1);
    }

    #[test]
    fn population_accumulates_and_conserves() {
        // Identity "projection", 2×2 target of 100×100 m cells at res 100:
        // one zensus cell lands wholly in target cell 0.
        let csv = "GITTER_ID_100m;x_mp_100m;y_mp_100m;Einwohner\nA;50;50;40\nB;950;950;7\n";
        let mut pop = vec![0.0f32; 4];
        let total = accumulate_population(
            csv.as_bytes(),
            |x, y| Ok((x, y)),
            |x, y| {
                if (0.0..200.0).contains(&x) && (0.0..200.0).contains(&y) {
                    Some(((y / 100.0) as usize).min(1) * 2 + ((x / 100.0) as usize).min(1))
                } else {
                    None
                }
            },
            100.0,
            &mut pop,
        )
        .unwrap();
        assert_eq!(total, 40, "cell B is outside the region");
        assert!((pop[0] - 40.0).abs() < 1e-9, "{:?}", pop);
        assert_eq!(pop[1] + pop[2] + pop[3], 0.0);
    }
}
