//! Rendering pack layers to RGBA images for the UI.
//!
//! Design note: the PACK IS THE BASEMAP. Terrain hillshade drawn from the
//! DTM gives an orientable map with zero external tiles, which is what an
//! offline-first tool needs — no PMTiles, no network, no basemap licence to
//! ship. A real cartographic basemap can layer in later without changing
//! any of this.
//!
//! All rendering works in the pack's own metric CRS (north-up), so there is
//! no reprojection in the interactive path.

use planner_core::geo::Xy;
use planner_terrain::Grid;

/// World-space rectangle to draw, in pack CRS meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl ViewRect {
    pub fn width_m(&self) -> f64 {
        self.max_x - self.min_x
    }
    pub fn height_m(&self) -> f64 {
        self.max_y - self.min_y
    }

    /// The whole extent of a grid.
    pub fn of(grid: &Grid) -> Self {
        let x0 = grid.origin.x;
        let x1 = grid.origin.x + grid.dx_m * (grid.width as f64 - 1.0);
        let y0 = grid.origin.y;
        let y1 = grid.origin.y + grid.dy_m * (grid.height as f64 - 1.0);
        Self {
            min_x: x0.min(x1),
            min_y: y0.min(y1),
            max_x: x0.max(x1),
            max_y: y0.max(y1),
        }
    }

    /// Pixel (col,row) in an image of `w`×`h` → world coordinate (centre).
    pub fn px_to_world(&self, col: u32, row: u32, w: u32, h: u32) -> Xy {
        let fx = (col as f64 + 0.5) / w as f64;
        let fy = (row as f64 + 0.5) / h as f64;
        Xy {
            x: self.min_x + fx * self.width_m(),
            // Row 0 is the TOP of the view = max_y (north up).
            y: self.max_y - fy * self.height_m(),
        }
    }

    /// World coordinate → fractional pixel position in a `w`×`h` image.
    pub fn world_to_px(&self, p: Xy, w: u32, h: u32) -> (f64, f64) {
        let fx = (p.x - self.min_x) / self.width_m();
        let fy = (self.max_y - p.y) / self.height_m();
        (fx * w as f64, fy * h as f64)
    }

    /// Zoom about a world anchor by `factor` (<1 zooms in).
    pub fn zoomed(&self, factor: f64, anchor: Xy) -> Self {
        let f = factor.clamp(0.01, 100.0);
        Self {
            min_x: anchor.x + (self.min_x - anchor.x) * f,
            max_x: anchor.x + (self.max_x - anchor.x) * f,
            min_y: anchor.y + (self.min_y - anchor.y) * f,
            max_y: anchor.y + (self.max_y - anchor.y) * f,
        }
    }

    pub fn panned(&self, dx: f64, dy: f64) -> Self {
        Self {
            min_x: self.min_x + dx,
            max_x: self.max_x + dx,
            min_y: self.min_y + dy,
            max_y: self.max_y + dy,
        }
    }
}

/// What to draw under the overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseLayer {
    /// Shaded relief from the terrain model.
    Hillshade,
    /// Shaded relief tinted by clutter height (buildings/vegetation).
    HillshadeClutter,
    /// Shaded relief tinted by residents per cell.
    HillshadePopulation,
}

pub struct RenderOpts {
    pub width: u32,
    pub height: u32,
    pub base: BaseLayer,
    /// Sun azimuth/altitude for the hillshade, degrees.
    pub sun_azimuth_deg: f64,
    pub sun_altitude_deg: f64,
    /// Vertical exaggeration — Brandenburg is flat; without this the relief
    /// is invisible.
    pub z_factor: f64,
    /// Light cartographic ground (paper-like) instead of neutral grey.
    /// Default: a map should read like a map.
    pub light: bool,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            width: 1024,
            height: 768,
            base: BaseLayer::Hillshade,
            sun_azimuth_deg: 315.0,
            sun_altitude_deg: 45.0,
            z_factor: 6.0,
            light: true,
        }
    }
}

/// Land-cover tint from the pack's ClutterClass codes. Water and woodland
/// are what make a map orientable at a glance, and the pack already carries
/// them — no extra data needed.
fn class_colour(code: u8) -> Option<[f64; 3]> {
    use planner_core::profile::ClutterClass as C;
    let c = C::from_code(code)?;
    Some(match c {
        C::Water => [150.0, 186.0, 214.0],
        C::Forest => [176.0, 199.0, 164.0],
        C::LowVegetation => [214.0, 224.0, 199.0],
        C::Open => [232.0, 231.0, 222.0],
        C::Suburban => [225.0, 216.0, 209.0],
        C::Urban => [216.0, 205.0, 199.0],
        C::DenseUrban => [206.0, 193.0, 188.0],
        C::Industrial => [214.0, 208.0, 214.0],
    })
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Apply `f(row_index, row_slice)` over an RGBA buffer's rows — in parallel
/// natively, serially under wasm. One call site, one behaviour difference.
#[cfg(feature = "parallel")]
fn for_each_row<F>(buf: &mut [u8], width: u32, f: F)
where
    F: Fn(usize, &mut [u8]) + Send + Sync,
{
    use rayon::prelude::*;
    buf.par_chunks_mut((width * 4) as usize)
        .enumerate()
        .for_each(|(row, line)| f(row, line));
}

#[cfg(not(feature = "parallel"))]
fn for_each_row<F>(buf: &mut [u8], width: u32, f: F)
where
    F: Fn(usize, &mut [u8]),
{
    for (row, line) in buf.chunks_mut((width * 4) as usize).enumerate() {
        f(row, line);
    }
}

/// The sun, reduced to the four numbers a pixel actually needs.
///
/// `sun_azimuth_deg` and `sun_altitude_deg` are constant for a whole render,
/// but the per-pixel formula recomputed `to_radians`, `sin` and `cos` of both
/// for every pixel of every tile.
#[derive(Clone, Copy)]
struct Sun {
    sin_alt: f64,
    cos_alt: f64,
    cos_az: f64,
    sin_az: f64,
}

impl Sun {
    fn new(opts: &RenderOpts) -> Self {
        let az = (360.0 - opts.sun_azimuth_deg + 90.0).to_radians();
        let alt = opts.sun_altitude_deg.to_radians();
        Sun { sin_alt: alt.sin(), cos_alt: alt.cos(), cos_az: az.cos(), sin_az: az.sin() }
    }

    /// Horn hillshade from the two slope components, with no trigonometry.
    ///
    /// The textbook form is
    ///
    ///   v = sin(alt)·cos(slope) + cos(alt)·sin(slope)·cos(az − aspect)
    ///
    /// with `slope = atan(L)`, `L = hypot(p, q)` and `aspect = atan2(−q, p)`.
    /// Written that way each pixel pays `sqrt`, `atan`, `atan2`, two more
    /// `cos`, a `sin` and two `to_radians` — and the whole tile is baked at
    /// full resolution, so on a 1997×1600 tile that is ~3.2 M of each.
    ///
    /// Substituting `cos(aspect) = p/L`, `sin(aspect) = −q/L`,
    /// `cos(slope) = 1/√(1+L²)` and `sin(slope) = L/√(1+L²)` makes the `L` in
    /// the second term cancel:
    ///
    ///   v = [ sin(alt) + cos(alt)·(cos(az)·p − sin(az)·q) ] / √(1 + p² + q²)
    ///
    /// One square root and one divide. This is an identity, not an
    /// approximation — including at `L = 0`, where the cancelled `L` leaves
    /// `sin(alt)`, which is the flat-ground answer the original gives too.
    #[inline]
    fn shade(&self, p: f64, q: f64) -> f64 {
        let v = (self.sin_alt + self.cos_alt * (self.cos_az * p - self.sin_az * q))
            / (1.0 + p * p + q * q).sqrt();
        v.clamp(0.0, 1.0)
    }
}

/// Horn hillshade at one world point, sampling the grid at ±one view pixel
/// so relief detail matches the zoom level rather than the raster step.
fn shade_at(dtm: &Grid, p: Xy, step_m: f64, opts: &RenderOpts, sun: &Sun) -> Option<f64> {
    let s = step_m.max(dtm.dx_m.abs());
    // `sample_bilinear` returns None only OFF the raster; a nodata cell INSIDE
    // it comes back as Some(NaN). Without the finite check the NaN runs
    // through slope/aspect, survives `clamp` (which propagates NaN), and
    // reaches the caller as a real shade value — where `lerp(...) as u8` turns
    // it into 0 and the map paints BLACK over ground that simply has no data.
    // Measured: the whole area outside the pack extent rendered black with the
    // road overlay drawn on top of it. Same failure family as the sweep
    // walking over nodata terrain and inventing a 91 dB loss: NaN must be
    // detected, never arithmetically absorbed.
    let z = |dx: f64, dy: f64| {
        dtm.sample_bilinear(Xy { x: p.x + dx, y: p.y + dy }).filter(|v| v.is_finite())
    };
    let (zl, zr) = (z(-s, 0.0)?, z(s, 0.0)?);
    let (zd, zu) = (z(0.0, -s)?, z(0.0, s)?);
    let dzdx = (zr - zl) as f64 / (2.0 * s) * opts.z_factor;
    let dzdy = (zu - zd) as f64 / (2.0 * s) * opts.z_factor;
    Some(sun.shade(dzdx, dzdy))
}

/// Render the base map: shaded relief, optionally tinted.
pub fn render_base(
    dtm: &Grid,
    tint: Option<&Grid>,
    view: &ViewRect,
    opts: &RenderOpts,
) -> Vec<u8> {
    render_base_with_classes(dtm, tint, None, view, opts)
}

/// As [`render_base`], with a ClutterClass grid supplying land-cover colour
/// (water, woodland, built-up) under the shading.
pub fn render_base_with_classes(
    dtm: &Grid,
    tint: Option<&Grid>,
    classes: Option<&Grid>,
    view: &ViewRect,
    opts: &RenderOpts,
) -> Vec<u8> {
    let (w, h) = (opts.width, opts.height);
    let mut out = vec![0u8; (w * h * 4) as usize];
    let step_m = view.width_m() / w as f64;
    let void = if opts.light { [238, 236, 231, 255] } else { [24, 26, 30, 255] };
    // The sun is constant for the whole image; only the terrain under each
    // pixel is not.
    let sun = Sun::new(opts);
    // Every pixel is independent — render rows in parallel where threads
    // exist. The wasm build takes the same closure serially.
    for_each_row(&mut out, w, |row, line| {
        let row = row as u32;
        for col in 0..w {
            let p = view.px_to_world(col, row, w, h);
            let i = (col * 4) as usize;
            let out = &mut *line;
            let Some(shade) = shade_at(dtm, p, step_m, opts, &sun) else {
                out[i..i + 4].copy_from_slice(&void);
                continue;
            };
            // Land-cover colour where available, else a paper/grey ground,
            // modulated by relief rather than replaced by it.
            let ground = classes
                .and_then(|c| c.sample_bilinear(p))
                .filter(|v| v.is_finite())
                .and_then(|v| class_colour(v.round() as u8));
            let (mut r, mut g, mut b) = match (ground, opts.light) {
                (Some(c), _) => {
                    let k = lerp(0.72, 1.12, shade); // relief as shading, not grey
                    (
                        (c[0] * k).min(255.0),
                        (c[1] * k).min(255.0),
                        (c[2] * k).min(255.0),
                    )
                }
                (None, true) => {
                    let base = lerp(176.0, 252.0, shade);
                    (base, base * 0.995, base * 0.975)
                }
                (None, false) => {
                    let base = lerp(70.0, 245.0, shade);
                    (base, base, base)
                }
            };
            if let Some(t) = tint {
                let v = t.sample_bilinear(p).unwrap_or(0.0) as f64;
                match opts.base {
                    BaseLayer::HillshadeClutter => {
                        // 0..25 m of clutter → warm ochre.
                        let f = (v / 25.0).clamp(0.0, 1.0);
                        r = lerp(r, 198.0, f * 0.8);
                        g = lerp(g, 132.0, f * 0.8);
                        b = lerp(b, 58.0, f * 0.8);
                    }
                    BaseLayer::HillshadePopulation => {
                        // Residents per cell, log-scaled — density spans
                        // orders of magnitude.
                        let f = ((v.max(0.0) + 1.0).ln() / 6.0_f64.ln()).clamp(0.0, 1.0);
                        r = lerp(r, 168.0, f * 0.92);
                        g = lerp(g, 30.0, f * 0.92);
                        b = lerp(b, 96.0, f * 0.72);
                    }
                    BaseLayer::Hillshade => {}
                }
            }
            out[i] = r as u8;
            out[i + 1] = g as u8;
            out[i + 2] = b as u8;
            out[i + 3] = 255;
        }
    });
    out
}

/// Colour ramp for basic transmission loss, green (strong) → red (marginal),
/// transparent beyond the budget.
fn coverage_colour(lb_db: f32, budget_db: f32) -> Option<[u8; 4]> {
    if !lb_db.is_finite() || lb_db > budget_db {
        return None;
    }
    // 30 dB of headroom spans the ramp.
    let t = ((budget_db - lb_db) / 30.0).clamp(0.0, 1.0) as f64;
    let (r, g, b) = if t > 0.5 {
        let u = (t - 0.5) * 2.0;
        (lerp(240.0, 40.0, u), lerp(200.0, 170.0, u), lerp(60.0, 90.0, u))
    } else {
        let u = t * 2.0;
        (lerp(200.0, 240.0, u), lerp(50.0, 200.0, u), lerp(50.0, 60.0, u))
    };
    Some([r as u8, g as u8, b as u8, 165])
}

/// Render a coverage loss raster as a translucent overlay, composited over
/// `base` (which must be the same size).
pub fn overlay_coverage(
    base: &mut [u8],
    loss: &Grid,
    view: &ViewRect,
    opts: &RenderOpts,
    budget_db: f32,
) {
    let (w, h) = (opts.width, opts.height);
    for_each_row(base, w, |row, line| {
        let row = row as u32;
        for col in 0..w {
            let p = view.px_to_world(col, row, w, h);
            let Some(lb) = loss.sample_bilinear(p) else { continue };
            let Some(c) = coverage_colour(lb, budget_db) else { continue };
            let i = (col * 4) as usize;
            let a = c[3] as f64 / 255.0;
            for k in 0..3 {
                line[i + k] = (c[k] as f64 * a + line[i + k] as f64 * (1.0 - a)) as u8;
            }
        }
    });
}

/// Overlay the DEPLOYED NETWORK's coverage: how many repeaters reach each
/// cell, as produced by `planner_coverage::gaps`.
///
/// Deliberately a different question, and a different palette, from
/// `overlay_coverage`. That one shows signal strength from ONE transmitter and
/// ramps green-to-red with the link budget. This one is a census of an existing
/// network, where the only distinctions that matter operationally are: nobody
/// reaches this, exactly one repeater reaches it (works until that repeater
/// fails), and two or more do (survives a loss). Reusing the loss ramp for it
/// would invite reading a redundancy count as a signal level.
///
/// NaN means "not evaluated" — outside every sweep window, or off the pack —
/// and is left transparent rather than being coloured as a gap. "We did not
/// look" and "nothing is there" must not render the same.
pub fn overlay_network(
    base: &mut [u8],
    served: &Grid,
    view: &ViewRect,
    opts: &RenderOpts,
    k_target: u8,
) {
    let (w, h) = (opts.width, opts.height);
    let k = k_target.max(1) as f32;
    for_each_row(base, w, |row, line| {
        let row = row as u32;
        for col in 0..w {
            let p = view.px_to_world(col, row, w, h);
            // Nearest, not bilinear: these are integer counts, and averaging
            // 0 and 2 into "1" would invent a redundancy tier that no site
            // actually provides.
            let Some(n) = served.sample_nearest(p) else { continue };
            if !n.is_finite() {
                continue;
            }
            let c: [u8; 4] = if n <= 0.0 {
                [200, 40, 50, 150] // reached by nobody
            } else if n < k {
                [235, 170, 45, 135] // reached, but below the redundancy target
            } else {
                [40, 150, 90, 120] // reached by k or more
            };
            let i = (col * 4) as usize;
            let a = c[3] as f64 / 255.0;
            for j in 0..3 {
                line[i + j] = (c[j] as f64 * a + line[i + j] as f64 * (1.0 - a)) as u8;
            }
        }
    });
}

/// One polyline in pack-CRS metres, with a style class.
pub struct RoadLine<'a> {
    pub class: u8,
    pub points: &'a [(f32, f32)],
}

/// Draw road/rail lines. Widths scale with zoom so the network reads as
/// structure when zoomed out and as streets when zoomed in; minor classes
/// drop out entirely at small scales rather than turning the map into mush.
pub fn draw_roads(buf: &mut [u8], view: &ViewRect, opts: &RenderOpts, roads: &[RoadLine]) {
    let m_per_px = view.width_m() / opts.width as f64;
    for r in roads {
        // class: 0 motorway, 1 trunk, 2 primary, 3 secondary, 4 rail,
        // 200 selection outline (a postal-area boundary — always drawn, since
        // it exists precisely because the operator asked to see it)
        let (base_w, colour, min_m_per_px) = match r.class {
            0 => (2.6, [128.0, 106.0, 74.0], 400.0),
            1 => (2.2, [146.0, 116.0, 78.0], 260.0),
            2 => (1.8, [158.0, 132.0, 88.0], 150.0),
            3 => (1.2, [170.0, 152.0, 116.0], 60.0),
            200 => (2.4, [190.0, 30.0, 120.0], f64::INFINITY),
            _ => (1.0, [120.0, 124.0, 132.0], 120.0),
        };
        if m_per_px > min_m_per_px {
            continue; // too zoomed out for this class to be legible
        }
        let width = (base_w * (1.0 + (60.0 / m_per_px.max(4.0)).min(2.2))).min(7.0);
        for pair in r.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let p0 = view.world_to_px(Xy { x: a.0 as f64, y: a.1 as f64 }, opts.width, opts.height);
            let p1 = view.world_to_px(Xy { x: b.0 as f64, y: b.1 as f64 }, opts.width, opts.height);
            draw_line(buf, opts, p0, p1, width, colour);
        }
    }
}

fn draw_line(
    buf: &mut [u8],
    opts: &RenderOpts,
    p0: (f64, f64),
    p1: (f64, f64),
    width: f64,
    colour: [f64; 3],
) {
    let (w, h) = (opts.width as i64, opts.height as i64);
    // Cheap reject: both ends far outside the frame.
    let pad = width + 2.0;
    if (p0.0 < -pad && p1.0 < -pad)
        || (p0.1 < -pad && p1.1 < -pad)
        || (p0.0 > w as f64 + pad && p1.0 > w as f64 + pad)
        || (p0.1 > h as f64 + pad && p1.1 > h as f64 + pad)
    {
        return;
    }
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len = (dx * dx + dy * dy).sqrt();
    if !len.is_finite() || len > 100_000.0 {
        return;
    }
    let steps = len.ceil().max(1.0) as i64;
    let half = width / 2.0;
    let r = half.ceil() as i64;
    for s in 0..=steps {
        let t = s as f64 / steps as f64;
        let (cx, cy) = (p0.0 + dx * t, p0.1 + dy * t);
        let (ix, iy) = (cx.round() as i64, cy.round() as i64);
        for oy in -r..=r {
            for ox in -r..=r {
                let (x, y) = (ix + ox, iy + oy);
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let d = (((x as f64 - cx).powi(2)) + ((y as f64 - cy).powi(2))).sqrt();
                // Soft edge: full colour to half-width, fading one pixel out.
                let a = if d <= half - 0.5 {
                    1.0
                } else if d <= half + 0.5 {
                    (half + 0.5 - d).clamp(0.0, 1.0)
                } else {
                    continue;
                };
                let i = ((y * w + x) * 4) as usize;
                for k in 0..3 {
                    buf[i + k] = (colour[k] * a + buf[i + k] as f64 * (1.0 - a)) as u8;
                }
            }
        }
    }
}

/// Draw a filled marker with a dark outline at a world position.
pub fn draw_marker(
    buf: &mut [u8],
    view: &ViewRect,
    opts: &RenderOpts,
    at: Xy,
    radius_px: i32,
    colour: [u8; 3],
) {
    let (w, h) = (opts.width as i32, opts.height as i32);
    let (fx, fy) = view.world_to_px(at, opts.width, opts.height);
    let (cx, cy) = (fx as i32, fy as i32);
    for dy in -radius_px - 1..=radius_px + 1 {
        for dx in -radius_px - 1..=radius_px + 1 {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x >= w || y >= h {
                continue;
            }
            let d = ((dx * dx + dy * dy) as f64).sqrt();
            let i = ((y * w + x) * 4) as usize;
            if d <= radius_px as f64 {
                buf[i] = colour[0];
                buf[i + 1] = colour[1];
                buf[i + 2] = colour[2];
                buf[i + 3] = 255;
            } else if d <= radius_px as f64 + 1.2 {
                buf[i] = 20;
                buf[i + 1] = 20;
                buf[i + 2] = 24;
                buf[i + 3] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp_grid(n: usize, res: f64) -> Grid {
        // Terrain rising to the east: hillshade must respond to it.
        let data: Vec<f32> = (0..n * n).map(|i| (i % n) as f32 * 2.0).collect();
        Grid::with_axes(Xy { x: 0.0, y: 0.0 }, res, -res, n, n, data).unwrap()
    }

    #[test]
    fn view_pixel_world_roundtrip() {
        let v = ViewRect { min_x: 100.0, min_y: -500.0, max_x: 1100.0, max_y: 300.0 };
        let (w, h) = (200, 160);
        for (col, row) in [(0u32, 0u32), (199, 159), (100, 80)] {
            let p = v.px_to_world(col, row, w, h);
            let (fx, fy) = v.world_to_px(p, w, h);
            assert!((fx - (col as f64 + 0.5)).abs() < 1e-6, "col {col}: {fx}");
            assert!((fy - (row as f64 + 0.5)).abs() < 1e-6, "row {row}: {fy}");
        }
        // Row 0 is north (max_y), not min_y.
        assert!(v.px_to_world(0, 0, w, h).y > v.px_to_world(0, h - 1, w, h).y);
    }

    #[test]
    fn zoom_keeps_the_anchor_fixed() {
        let v = ViewRect { min_x: 0.0, min_y: 0.0, max_x: 1000.0, max_y: 1000.0 };
        let anchor = Xy { x: 250.0, y: 750.0 };
        let z = v.zoomed(0.5, anchor);
        let (a0, a1) = (v.world_to_px(anchor, 100, 100), z.world_to_px(anchor, 100, 100));
        assert!((a0.0 - a1.0).abs() < 1e-9 && (a0.1 - a1.1).abs() < 1e-9);
        assert!((z.width_m() - 500.0).abs() < 1e-9);
    }

    #[test]
    fn hillshade_is_lit_and_directional() {
        let g = ramp_grid(64, 30.0);
        let view = ViewRect::of(&g);
        let opts = RenderOpts { width: 64, height: 64, ..Default::default() };
        let img = render_base(&g, None, &view, &opts);
        assert_eq!(img.len(), 64 * 64 * 4);
        // Fully opaque, and not a flat fill.
        assert!(img.chunks(4).all(|p| p[3] == 255));
        let vals: Vec<u8> = img.chunks(4).map(|p| p[0]).collect();
        let (lo, hi) = (*vals.iter().min().unwrap(), *vals.iter().max().unwrap());
        assert!(hi > lo, "hillshade produced a flat image");
        // A NW sun on an east-facing slope leaves it darker than a NE sun.
        let nw = render_base(&g, None, &view, &RenderOpts { sun_azimuth_deg: 315.0, width: 64, height: 64, ..Default::default() });
        let ne = render_base(&g, None, &view, &RenderOpts { sun_azimuth_deg: 45.0, width: 64, height: 64, ..Default::default() });
        let mean = |v: &[u8]| v.chunks(4).map(|p| p[0] as u64).sum::<u64>();
        assert_ne!(mean(&nw), mean(&ne));
    }

    #[test]
    fn outside_the_pack_is_void_not_terrain() {
        let g = ramp_grid(16, 30.0);
        // View far to the east of the grid.
        let view = ViewRect { min_x: 100_000.0, min_y: 0.0, max_x: 101_000.0, max_y: 1000.0 };
        // Light default: a paper-coloured void.
        let light = RenderOpts { width: 8, height: 8, ..Default::default() };
        let img = render_base(&g, None, &view, &light);
        assert!(img.chunks(4).all(|p| p[0] == 238 && p[3] == 255));
        // Dark theme still available and distinct.
        let dark = RenderOpts { width: 8, height: 8, light: false, ..Default::default() };
        let img = render_base(&g, None, &view, &dark);
        assert!(img.chunks(4).all(|p| p[0] == 24 && p[3] == 255));
    }

    #[test]
    fn roads_draw_and_thin_out_when_zoomed_far_out() {
        let g = ramp_grid(64, 30.0);
        let view = ViewRect::of(&g);
        let opts = RenderOpts { width: 64, height: 64, ..Default::default() };
        // A line straight across the middle of the view.
        let y = ((view.min_y + view.max_y) / 2.0) as f32;
        let pts = [(view.min_x as f32, y), (view.max_x as f32, y)];
        let secondary = [RoadLine { class: 3, points: &pts }];

        let mut img = render_base(&g, None, &view, &opts);
        let before = img.clone();
        draw_roads(&mut img, &view, &opts, &secondary);
        assert_ne!(img, before, "a secondary road should draw at this scale");

        // Same road, but a view 500 km wide: minor classes must drop out.
        let wide = ViewRect { min_x: 0.0, min_y: 0.0, max_x: 500_000.0, max_y: 500_000.0 };
        let mut img2 = render_base(&g, None, &wide, &opts);
        let before2 = img2.clone();
        draw_roads(&mut img2, &wide, &opts, &secondary);
        assert_eq!(img2, before2, "secondary roads must not clutter a 500 km view");
    }

    #[test]
    fn coverage_overlay_only_paints_within_budget() {
        let n = 32;
        let g = ramp_grid(n, 30.0);
        let view = ViewRect::of(&g);
        let opts = RenderOpts { width: 32, height: 32, ..Default::default() };
        let mut img = render_base(&g, None, &view, &opts);
        let before = img.clone();
        // Half the raster inside budget, half far outside.
        let loss: Vec<f32> = (0..n * n)
            .map(|i| if (i % n) < n / 2 { 120.0 } else { 200.0 })
            .collect();
        let lg = Grid::with_axes(g.origin, g.dx_m, g.dy_m, n, n, loss).unwrap();
        overlay_coverage(&mut img, &lg, &view, &opts, 147.5);
        let changed: Vec<bool> = img
            .chunks(4)
            .zip(before.chunks(4))
            .map(|(a, b)| a[..3] != b[..3])
            .collect();
        let left = changed[..16].iter().filter(|c| **c).count();
        let right = changed[16..32].iter().filter(|c| **c).count();
        assert!(left > 12, "in-budget half should be tinted ({left}/16)");
        assert_eq!(right, 0, "over-budget half must stay untouched");
    }

    #[test]
    fn marker_lands_at_the_right_pixel() {
        let g = ramp_grid(32, 30.0);
        let view = ViewRect::of(&g);
        let opts = RenderOpts { width: 64, height: 64, ..Default::default() };
        let mut img = render_base(&g, None, &view, &opts);
        let centre = Xy {
            x: (view.min_x + view.max_x) / 2.0,
            y: (view.min_y + view.max_y) / 2.0,
        };
        draw_marker(&mut img, &view, &opts, centre, 3, [220, 40, 40]);
        let i = ((32 * 64 + 32) * 4) as usize;
        assert_eq!(&img[i..i + 3], &[220, 40, 40]);
        // A corner far from the marker is untouched by it.
        assert_ne!(&img[0..3], &[220, 40, 40]);
    }

    /// Nodata INSIDE the raster must render as the void colour, not as black.
    ///
    /// `sample_bilinear` returns Some(NaN) there (None is only for off-raster),
    /// and NaN survives `clamp`, so the shade reached the colour maths and
    /// `lerp(..) as u8` produced 0 — an opaque black field, with the road
    /// overlay drawn on top of it, over ground that simply has no data.
    #[test]
    fn nodata_inside_the_raster_renders_as_void_not_black() {
        let n = 32usize;
        let mut data = vec![100.0f32; n * n];
        // A nodata patch in the middle of otherwise valid terrain.
        for row in 8..24 {
            for col in 8..24 {
                data[row * n + col] = f32::NAN;
            }
        }
        let g = Grid::with_axes(Xy { x: 0.0, y: 0.0 }, 10.0, -10.0, n, n, data).unwrap();
        let view = ViewRect { min_x: 0.0, max_x: 310.0, min_y: -310.0, max_y: 0.0 };
        let opts = RenderOpts { width: 64, height: 64, ..Default::default() };
        let img = render_base_with_classes(&g, None, None, &view, &opts);

        let px = |col: usize, row: usize| {
            let i = (row * 64 + col) * 4;
            [img[i], img[i + 1], img[i + 2]]
        };
        let void = [238u8, 236, 231];
        assert_eq!(px(32, 32), void, "nodata centre must be void");
        // And nothing anywhere may be pure black, which is what NaN produced.
        let black = img
            .chunks_exact(4)
            .filter(|p| p[0] == 0 && p[1] == 0 && p[2] == 0)
            .count();
        assert_eq!(black, 0, "{black} pixels rendered black from NaN terrain");
        // The valid corner still renders terrain, not void.
        assert_ne!(px(2, 2), void, "valid terrain must not render as void");
    }

    /// The trig-free hillshade is the SAME function, not a cheaper lookalike.
    ///
    /// It is an algebraic identity, so it is testable as one: the textbook
    /// form and the closed form must agree to floating-point noise over the
    /// whole domain, including the degenerate flat-ground case where the
    /// substitution divides by a slope length of zero before cancelling.
    #[test]
    fn the_trig_free_hillshade_equals_the_textbook_one() {
        let textbook = |p: f64, q: f64, opts: &RenderOpts| {
            let slope = (p * p + q * q).sqrt().atan();
            let aspect = (-q).atan2(p);
            let az = (360.0 - opts.sun_azimuth_deg + 90.0).to_radians();
            let alt = opts.sun_altitude_deg.to_radians();
            (alt.sin() * slope.cos() + alt.cos() * slope.sin() * (az - aspect).cos())
                .clamp(0.0, 1.0)
        };
        let mut worst = 0.0f64;
        for &(azi, alti) in &[(315.0, 45.0), (0.0, 10.0), (135.0, 80.0), (225.0, 30.0)] {
            let opts = RenderOpts {
                sun_azimuth_deg: azi,
                sun_altitude_deg: alti,
                ..Default::default()
            };
            let sun = Sun::new(&opts);
            // Slopes from dead flat to a cliff, in every direction.
            for i in -40..=40 {
                for j in -40..=40 {
                    let (p, q) = (i as f64 * 0.25, j as f64 * 0.25);
                    let d = (sun.shade(p, q) - textbook(p, q, &opts)).abs();
                    worst = worst.max(d);
                }
            }
            // Flat ground is where the algebra cancels an L that is zero.
            assert!(
                (sun.shade(0.0, 0.0) - alti.to_radians().sin()).abs() < 1e-12,
                "flat ground must be sin(alt)"
            );
        }
        assert!(worst < 1e-12, "closed form drifts from the textbook one by {worst}");
    }

    /// Rendering a grid at its OWN size lands on its own cells — but only over
    /// the OUTER extent.
    ///
    /// `px_to_world` returns pixel centres, so a rect that spans cell CENTRES
    /// is one cell narrower than the image it is being rendered into, and the
    /// whole picture stretches by a cell about its middle. Measured on a real
    /// tile (1997 cells, 12.82 m each, 25.6 km view) that put the left edge
    /// 6.4 m east and the right edge 6.4 m west of the ground they claimed.
    ///
    /// It is invisible in isolation — the hillshade and the roads baked with it
    /// shift together — and shows up only against a layer drawn from the camera
    /// rect, such as the coverage overlay. So the invariant is asserted here
    /// rather than left to be noticed as "the coverage looks a street off".
    #[test]
    fn a_render_at_the_grids_own_size_samples_its_own_cells() {
        let (n, res) = (16usize, 10.0);
        let g = Grid::with_axes(
            Xy { x: 1000.0, y: 2000.0 },
            res,
            -res,
            n,
            n,
            (0..n * n).map(|i| i as f32).collect(),
        )
        .unwrap();
        let outer = ViewRect {
            min_x: g.origin.x - res / 2.0,
            max_x: g.origin.x + (n as f64 - 0.5) * res,
            min_y: g.origin.y - (n as f64 - 0.5) * res,
            max_y: g.origin.y + res / 2.0,
        };
        for row in 0..n {
            for col in 0..n {
                let p = outer.px_to_world(col as u32, row as u32, n as u32, n as u32);
                let want = g.data[row * n + col];
                let got = g.sample_bilinear(p).expect("inside the grid");
                assert!(
                    (got - want).abs() < 1e-4,
                    "outer extent, cell {col},{row}: sampled {got}, cell holds {want}"
                );
            }
        }
        // And the centre-to-centre rect does NOT: that is the defect.
        let centres = ViewRect {
            min_x: g.origin.x,
            max_x: g.origin.x + (n as f64 - 1.0) * res,
            min_y: g.origin.y - (n as f64 - 1.0) * res,
            max_y: g.origin.y,
        };
        let p = centres.px_to_world(0, 0, n as u32, n as u32);
        let off = p.x - g.origin.x;
        assert!(
            off > res * 0.4,
            "the centre-to-centre rect should miss cell 0 by ~half a cell, missed by {off}"
        );
    }
}
