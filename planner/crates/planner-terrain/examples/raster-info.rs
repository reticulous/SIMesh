//! Report the on-disk shape of a pack raster: how it is chunked, and what one
//! chunk costs to decode.
//!
//! Written because a tile fetch was measured at 17 s cold and 3 s warm on a
//! city pack, and every explanation offered for it (strips spanned, region,
//! payload size) was contradicted by the next measurement. The decisive
//! numbers are the chunk dimensions -- a striped TIFF's "chunk" is a full-width
//! band, so asking for four rows of it decodes the whole band -- and they are
//! not visible from outside the reader.
//!
//! Usage:  cargo run -p planner-terrain --example raster-info -- <file.tif>...

use planner_terrain::cog::CogReader;
use planner_core::geo::Xy;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: raster-info <file.tif>...");
        std::process::exit(2);
    }
    for path in &args {
        let p = std::path::Path::new(path);
        let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut r = match CogReader::open(p) {
            Ok(r) => r,
            Err(e) => {
                println!("{name:<20} open failed: {e}");
                continue;
            }
        };
        let m = *r.meta();
        let chunk_cells = m.chunk_w as u64 * m.chunk_h as u64;
        let across = m.width.div_ceil(m.chunk_w);
        let down = m.height.div_ceil(m.chunk_h);
        println!("{name}");
        println!("  {} x {} cells at {} m", m.width, m.height, m.dx);
        println!(
            "  chunk {} x {} = {} cells, {:.1} MB decoded as f32",
            m.chunk_w,
            m.chunk_h,
            chunk_cells,
            chunk_cells as f64 * 4.0 / 1e6
        );
        println!("  {across} across x {down} down = {} chunks", across as u64 * down as u64);

        // Time one pixel in a fresh chunk. This is the unit of work a window
        // pays for every chunk it touches, however few of its cells it wants.
        let mid_x = m.origin.x + m.dx * (m.width as f64 * 0.5);
        let mid_y = m.origin.y + m.dy * (m.height as f64 * 0.5);
        let t0 = std::time::Instant::now();
        let one = r.window(Xy { x: mid_x, y: mid_y }, Xy { x: mid_x + m.dx, y: mid_y + m.dy });
        let cold = t0.elapsed();
        let t1 = std::time::Instant::now();
        let _ = r.window(Xy { x: mid_x, y: mid_y }, Xy { x: mid_x + m.dx, y: mid_y + m.dy });
        let warm = t1.elapsed();
        match one {
            Ok(_) => println!(
                "  2x2-cell window: {:.0} ms cold, {:.1} ms warm",
                cold.as_secs_f64() * 1e3,
                warm.as_secs_f64() * 1e3
            ),
            Err(e) => println!("  2x2-cell window failed: {e}"),
        }
    }
}
