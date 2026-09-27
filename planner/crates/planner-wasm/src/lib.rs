//! Browser-side rendering.
//!
//! THE ASYMMETRY: the server currently re-renders and re-encodes a ~1 MB PNG
//! for every pan, zoom, layer toggle and budget tweak. But the underlying
//! DATA for a region is small and static — a 25×18 km window at 30 m is
//! ~530 k cells, about 2 MB as compact typed arrays, and it compresses.
//! Ship that once and every subsequent frame is local: no request, no PNG
//! encode, no decode, no server CPU. Cost is paid once per region instead of
//! once per interaction.
//!
//! The same consequence matters for the field: a browser holding the tile
//! keeps working when the link to the server does not.
//!
//! This crate is deliberately thin — it reuses `planner-render` unchanged
//! (built without rayon), so the browser and the server draw identical maps
//! from identical code.

use planner_core::geo::Xy;
use planner_render::{BaseLayer, RenderOpts, RoadLine, ViewRect};
use planner_terrain::Grid;
use wasm_bindgen::prelude::*;

/// One region's layers, decoded once and reused for every frame.
#[wasm_bindgen]
pub struct Tile {
    terrain: Grid,
    classes: Option<Grid>,
    population: Option<Grid>,
    clutter: Option<Grid>,
    /// Flattened road geometry: [class, n, x0,y0, x1,y1, ...] repeated.
    roads: Vec<f32>,
    /// Loss raster from the last server-side P.1812 run, if any.
    loss: Option<Grid>,
    /// Baked base image at TILE resolution: hillshade + land cover + tint +
    /// roads. None of that depends on the camera, so it is computed once per
    /// tile and every frame becomes a resample instead of a re-render.
    /// This is the difference between ~600 ms and ~15 ms per frame in a
    /// single-threaded WASM context.
    baked: Vec<u8>,
    baked_layer: u8,
    baked_roads: bool,
    /// The base image was supplied by the server; do not bake locally.
    baked_external: bool,
    /// Deployed-network coverage census: per cell, how many repeaters reach
    /// it. Its own raster because the sweep window is not the tile window.
    network: Option<Grid>,
    network_k: u8,
    /// Postal-area outline to highlight: [ring_len, x,y, x,y, ...] repeated.
    /// Kept out of the bake so selecting an area costs one frame, not a
    /// re-render of the whole tile.
    highlight: Vec<f32>,
}

fn grid_from(
    origin_x: f64,
    origin_y: f64,
    res_x: f64,
    res_y: f64,
    w: usize,
    h: usize,
    data: Vec<f32>,
) -> Grid {
    Grid {
        origin: Xy { x: origin_x, y: origin_y },
        dx_m: res_x,
        dy_m: -res_y,
        width: w,
        height: h,
        data,
    }
}

#[wasm_bindgen]
impl Tile {
    /// Build from typed arrays. Terrain arrives as i16 decimetres-above-zero
    /// style scaling is the caller's business; we take plain f32 here and let
    /// the server decide the wire format.
    #[wasm_bindgen(constructor)]
    pub fn new(
        origin_x: f64,
        origin_y: f64,
        res_x: f64,
        res_y: f64,
        width: usize,
        height: usize,
        terrain: Vec<f32>,
    ) -> Result<Tile, JsValue> {
        if terrain.len() != width * height {
            return Err(JsValue::from_str("terrain length does not match width*height"));
        }
        Ok(Tile {
            terrain: grid_from(origin_x, origin_y, res_x, res_y, width, height, terrain),
            classes: None,
            population: None,
            clutter: None,
            roads: Vec::new(),
            loss: None,
            baked: Vec::new(),
            baked_layer: 255,
            baked_roads: false,
            baked_external: false,
            network: None,
            network_k: 1,
            highlight: Vec::new(),
        })
    }

    pub fn set_classes(&mut self, data: Vec<f32>) {
        self.classes = self.same_shape(data);
    }
    pub fn set_population(&mut self, data: Vec<f32>) {
        self.population = self.same_shape(data);
    }
    pub fn set_clutter(&mut self, data: Vec<f32>) {
        self.clutter = self.same_shape(data);
    }
    pub fn set_roads(&mut self, flat: Vec<f32>) {
        self.roads = flat;
    }

    /// Attach a loss raster (its own origin/size — the sweep window is not
    /// the tile window). Re-compositing it locally is what makes the budget
    /// slider instant.
    /// Attach the loss raster for the CURRENT VIEW.
    ///
    /// Both axes, independently. One square `res` derived from the width
    /// stretches the raster whenever the view aspect and the cell counts
    /// disagree — which they do as soon as either axis hits its own cap — and
    /// a stretched coverage overlay slides off the base map it is supposed to
    /// be explaining. `tile_bin` was fixed for exactly this; the overlay
    /// rasters were still carrying the single-`res` form.
    pub fn set_loss(
        &mut self,
        origin_x: f64,
        origin_y: f64,
        res_x: f64,
        res_y: f64,
        width: usize,
        height: usize,
        data: Vec<f32>,
    ) {
        if data.len() == width * height {
            self.loss = Some(grid_from(origin_x, origin_y, res_x, res_y, width, height, data));
        }
    }

    pub fn clear_loss(&mut self) {
        self.loss = None;
    }

    /// Set the postal-area outline: [n, x,y × n] per ring, concatenated.
    pub fn set_highlight(&mut self, flat: Vec<f32>) {
        self.highlight = flat;
    }
    pub fn clear_highlight(&mut self) {
        self.highlight.clear();
    }

    /// Attach the deployed network's coverage census (see
    /// `planner_render::overlay_network`). Counts, not losses.
    pub fn set_network(
        &mut self,
        origin_x: f64,
        origin_y: f64,
        res_x: f64,
        res_y: f64,
        width: usize,
        height: usize,
        k_target: u8,
        data: Vec<f32>,
    ) {
        if data.len() == width * height {
            self.network =
                Some(grid_from(origin_x, origin_y, res_x, res_y, width, height, data));
            self.network_k = k_target.max(1);
        }
    }
    pub fn clear_network(&mut self) {
        self.network = None;
    }

    /// Install a base image baked elsewhere, and stop baking locally.
    ///
    /// The bake is the largest single piece of work in the map — measured at
    /// 1075 ms without roads and 1351 ms with them on a 1997×1600 tile — and
    /// it runs on the one thread the browser also uses for input, so it lands
    /// as a stall in the middle of a drag. It is also embarrassingly parallel
    /// over rows and depends on nothing the browser knows that the server does
    /// not. So the server renders it across every core (355 ms on this pack)
    /// and the browser's share becomes this copy.
    ///
    /// Rejected unless the image is exactly the tile's cell grid: a mismatch
    /// would place the base map against the wrong ground rather than looking
    /// broken, and the caller can always fall back to the local bake.
    pub fn set_baked(&mut self, width: usize, height: usize, rgba: Vec<u8>) -> bool {
        let (w, h) = (self.terrain.width, self.terrain.height);
        if width != w || height != h || rgba.len() != w * h * 4 {
            return false;
        }
        self.baked = rgba;
        self.baked_external = true;
        true
    }

    fn same_shape(&self, data: Vec<f32>) -> Option<Grid> {
        let (w, h) = (self.terrain.width, self.terrain.height);
        if data.len() != w * h {
            return None;
        }
        Some(grid_from(
            self.terrain.origin.x,
            self.terrain.origin.y,
            self.terrain.dx_m,
            -self.terrain.dy_m,
            w,
            h,
            data,
        ))
    }

    /// World bounds of the loaded tile, so JS can tell when the view has
    /// left the region and a new tile is genuinely needed.
    pub fn bounds(&self) -> Vec<f64> {
        let g = &self.terrain;
        let x1 = g.origin.x + g.dx_m * (g.width as f64 - 1.0);
        let y1 = g.origin.y + g.dy_m * (g.height as f64 - 1.0);
        vec![g.origin.x.min(x1), y1.min(g.origin.y), g.origin.x.max(x1), g.origin.y.max(y1)]
    }

    /// The rectangle `compose()`'s image actually covers, as
    /// `[min_x, min_y, max_x, max_y]`.
    ///
    /// This is what the page needs to place the composed image with
    /// `drawImage`, and it must be the SAME rectangle the bake was rendered
    /// over — so it is derived from `bake_view` rather than restated in JS,
    /// where the half-cell term would drift the moment either side changed.
    pub fn outer_bounds(&self) -> Vec<f64> {
        let v = self.bake_view();
        vec![v.min_x, v.min_y, v.max_x, v.max_y]
    }

    /// The tile's own full extent as a view.
    fn tile_view(&self) -> ViewRect {
        let g = &self.terrain;
        let x1 = g.origin.x + g.dx_m * (g.width as f64 - 1.0);
        let y1 = g.origin.y + g.dy_m * (g.height as f64 - 1.0);
        ViewRect {
            min_x: g.origin.x.min(x1),
            max_x: g.origin.x.max(x1),
            min_y: y1.min(g.origin.y),
            max_y: g.origin.y.max(y1),
        }
    }

    /// The tile's OUTER extent: cell centres pushed out by half a cell.
    ///
    /// `ViewRect::px_to_world` returns pixel CENTRES, so a render whose rect
    /// spans cell CENTRES samples a grid slightly inside itself. Measured on a
    /// real tile — 1997 cells over a 25.6 km view, 12.82 m each — the bake was
    /// asking for a point 6.4 m EAST of cell 0 at the left edge and 6.4 m WEST
    /// of the last cell at the right edge: the whole image stretched by one
    /// cell about its centre.
    ///
    /// That is self-consistent with the roads baked into it, so it never looked
    /// broken. It is NOT consistent with the coverage overlay, which is drawn
    /// against the camera rect and is therefore right — so the base map and the
    /// answer painted on top of it disagreed by up to a cell about which ground
    /// they meant. On a map whose purpose is "does THIS building hear it", that
    /// is the one disagreement not worth having.
    ///
    /// Landing on exact cell centres also makes every bilinear tap in the bake
    /// degenerate to the cell's own value, which is what it always intended.
    fn bake_view(&self) -> ViewRect {
        let g = &self.terrain;
        let (hx, hy) = (g.dx_m.abs() * 0.5, g.dy_m.abs() * 0.5);
        let v = self.tile_view();
        ViewRect {
            min_x: v.min_x - hx,
            max_x: v.max_x + hx,
            min_y: v.min_y - hy,
            max_y: v.max_y + hy,
        }
    }

    /// Render the camera-independent parts once at tile resolution.
    fn bake(&mut self, layer: u8, show_roads: bool) {
        // A server-supplied base is authoritative: the caller refetches it
        // when the layer or the roads toggle changes, so there is nothing to
        // recompute and the 1.1-1.5 s local bake never runs.
        if self.baked_external && !self.baked.is_empty() {
            return;
        }
        if self.baked_layer == layer && self.baked_roads == show_roads && !self.baked.is_empty() {
            return;
        }
        let (w, h) = (self.terrain.width as u32, self.terrain.height as u32);
        // The OUTER extent, not the centre-to-centre one: see `bake_view`.
        // `render()` maps the camera into this image as a fractional column
        // index over `tile_view()`, i.e. it assumes baked pixel N *is* cell N.
        // Only a bake over the outer extent actually makes that true.
        let view = self.bake_view();
        let (base, tint) = match layer {
            1 => (BaseLayer::HillshadePopulation, self.population.as_ref()),
            2 => (BaseLayer::HillshadeClutter, self.clutter.as_ref()),
            _ => (BaseLayer::Hillshade, None),
        };
        let opts = RenderOpts { width: w, height: h, base, ..Default::default() };
        let mut rgba = planner_render::render_base_with_classes(
            &self.terrain,
            tint,
            self.classes.as_ref(),
            &view,
            &opts,
        );
        if show_roads && !self.roads.is_empty() {
            let mut pts: Vec<Vec<(f32, f32)>> = Vec::new();
            let mut classes = Vec::new();
            let mut i = 0usize;
            while i + 1 < self.roads.len() {
                let class = self.roads[i] as u8;
                let n = self.roads[i + 1] as usize;
                i += 2;
                let mut way = Vec::with_capacity(n);
                for k in 0..n {
                    if i + k * 2 + 1 >= self.roads.len() {
                        break;
                    }
                    way.push((self.roads[i + k * 2], self.roads[i + k * 2 + 1]));
                }
                i += n * 2;
                pts.push(way);
                classes.push(class);
            }
            let refs: Vec<RoadLine> = classes
                .iter()
                .zip(&pts)
                .map(|(c, p)| RoadLine { class: *c, points: p })
                .collect();
            planner_render::draw_roads(&mut rgba, &view, &opts, &refs);
        }
        self.baked = rgba;
        self.baked_layer = layer;
        self.baked_roads = show_roads;
    }

    /// Composite the tile at ITS OWN resolution: baked base plus every overlay.
    ///
    /// WHY THIS REPLACED A PER-FRAME `render`. The old entry point resampled
    /// the baked image to the CAMERA every frame and composited the overlays
    /// per screen pixel. Measured on a 1997x1600 tile against a 1248x1000
    /// canvas: 48.5 ms a frame for the base and 129.5 ms with the census on,
    /// and `draw()` runs it for the overview AND the detail tile. Panning
    /// dispatches one of those per pointermove, so the main thread never got
    /// back to the input queue -- which is precisely the state a browser
    /// reports as "this page is slow, wait or exit".
    ///
    /// Nothing in that work depended on the camera except the resample, and
    /// resampling an image to a rectangle is what the 2D canvas does in
    /// hardware. So this composes ONCE per change -- new tile, layer switch,
    /// new sweep band, budget move -- and the browser's `drawImage` does every
    /// camera move for free.
    ///
    /// `feather` fades the outer cells' alpha so a detail tile laid over the
    /// coarse overview does not end in a hard rectangle: a sharp-edged square
    /// of detail reads as a DATA boundary, and this pack genuinely has those.
    /// It is baked in cell space rather than screen space because it is now
    /// computed once per tile instead of once per frame; the visible width
    /// still tracks zoom, because the whole image is scaled by `drawImage`.
    pub fn compose(
        &mut self,
        layer: u8,
        show_coverage: bool,
        budget_db: f32,
        show_roads: bool,
        show_network: bool,
        feather: bool,
    ) -> Vec<u8> {
        self.bake(layer, show_roads);
        let (w, h) = (self.terrain.width as u32, self.terrain.height as u32);
        let mut rgba = self.baked.clone();
        let view = self.bake_view();
        let opts = RenderOpts { width: w, height: h, ..Default::default() };

        // Order matches what the old per-frame path drew: census under
        // coverage, highlight on top. The two coverage layers are never both
        // on -- `draw()` picks one from the mode -- but the order is fixed
        // here rather than left to the caller.
        if show_network {
            if let Some(net) = &self.network {
                planner_render::overlay_network(&mut rgba, net, &view, &opts, self.network_k);
            }
        }
        if show_coverage {
            if let Some(loss) = &self.loss {
                planner_render::overlay_coverage(&mut rgba, loss, &view, &opts, budget_db);
            }
        }
        if !self.highlight.is_empty() {
            let mut rings: Vec<Vec<(f32, f32)>> = Vec::new();
            let mut i = 0usize;
            while i < self.highlight.len() {
                let n = self.highlight[i] as usize;
                i += 1;
                let mut ring = Vec::with_capacity(n + 1);
                for k in 0..n {
                    if i + k * 2 + 1 >= self.highlight.len() {
                        break;
                    }
                    ring.push((self.highlight[i + k * 2], self.highlight[i + k * 2 + 1]));
                }
                i += n * 2;
                // Deliberately NOT closed: a postal boundary arrives as the
                // relation's member ways, which already tile into a closed
                // loop. Closing each one individually draws chords across the
                // district.
                if ring.len() >= 2 {
                    rings.push(ring);
                }
            }
            let lines: Vec<RoadLine> =
                rings.iter().map(|r| RoadLine { class: 200, points: r }).collect();
            planner_render::draw_roads(&mut rgba, &view, &opts, &lines);
        }

        if feather {
            // Twelve cells, or a quarter of the smaller side on a tiny tile.
            let f = 12.0f32.min(w.min(h) as f32 / 4.0);
            if f >= 1.0 {
                // Only the border band is touched. A full-image pass to fade
                // twelve cells reads 3.2 M pixels to modify 96 k of them, and
                // this runs inside the one task per tile that is already the
                // largest thing left on the main thread.
                let (fw, fh) = (f as usize, f as usize);
                let (wu, hu) = (w as usize, h as usize);
                for row in 0..hu {
                    let dy = row.min(hu - 1 - row);
                    let edge_row = dy < fh;
                    // Interior rows only need their left and right margins;
                    // the top and bottom bands need every column.
                    let cols: &mut dyn Iterator<Item = usize> = if edge_row {
                        &mut (0..wu)
                    } else {
                        &mut (0..fw).chain(wu.saturating_sub(fw)..wu)
                    };
                    for col in cols {
                        let d = col.min(wu - 1 - col).min(dy) as f32;
                        if d < f {
                            let i = (row * wu + col) * 4 + 3;
                            rgba[i] = (rgba[i] as f32 * (d / f)) as u8;
                        }
                    }
                }
            }
        }
        rgba
    }

    /// Everything `compose` draws EXCEPT the two coverage overlays.
    ///
    /// The page draws the coverage ramp and the census in a WebGL2 fragment
    /// shader at SCREEN resolution instead, so the tile surface must carry
    /// only what is genuinely camera-independent. It still carries the
    /// highlight ring and the feather: neither is an overlay, both belong to
    /// the tile, and leaving either out of the base would delete it from the
    /// map rather than move it to the GPU.
    ///
    /// Deliberately delegates to `compose` rather than repeating the
    /// bake/highlight/feather sequence. A second copy of that order is a
    /// second thing to keep in step, and the one guarantee this entry point
    /// owes the caller — same pixels as `compose` with both overlays off — is
    /// then true by construction instead of by review.
    pub fn compose_base(&mut self, layer: u8, show_roads: bool, feather: bool) -> Vec<u8> {
        // `budget_db` is unreachable with `show_coverage` false.
        self.compose(layer, false, 0.0, show_roads, false, feather)
    }

    /// Marker drawing stays here so the canvas needs no separate overlay.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_marker(
        &self,
        rgba: &mut [u8],
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
        width: u32,
        height: u32,
        at_x: f64,
        at_y: f64,
    ) {
        let view = ViewRect { min_x, min_y, max_x, max_y };
        let opts = RenderOpts { width, height, ..Default::default() };
        planner_render::draw_marker(rgba, &view, &opts, Xy { x: at_x, y: at_y }, 5, [200, 30, 45]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tile carrying terrain, BOTH overlay rasters and a highlight ring, so
    /// a composite that drops any one of them is visibly different from one
    /// that does not.
    fn tile_with_everything() -> Tile {
        let (w, h) = (40usize, 32usize);
        // A ramp, so the hillshade has something to shade.
        let terrain: Vec<f32> =
            (0..w * h).map(|i| ((i % w) as f32) * 0.7 + ((i / w) as f32) * 0.3).collect();
        let mut t = Tile::new(1000.0, 2000.0, 10.0, 10.0, w, h, terrain).unwrap();
        // Loss straddling the 150 dB budget used below: the left columns are
        // inside it and take a ramp colour, the right ones are beyond it and
        // must leave the base untouched.
        let loss: Vec<f32> = (0..w * h).map(|i| 120.0 + (i % w) as f32 * 1.5).collect();
        t.set_loss(1000.0, 2000.0, 10.0, 10.0, w, h, loss);
        // 0 / 1 / 2 repeaters: one cell of each palette entry at k = 2.
        let served: Vec<f32> = (0..w * h).map(|i| (i % 3) as f32).collect();
        t.set_network(1000.0, 2000.0, 10.0, 10.0, w, h, 2, served);
        // One ring well inside the tile, in the form `set_highlight` takes:
        // [n, x, y, x, y, ...].
        t.set_highlight(vec![
            4.0, 1100.0, 1900.0, 1300.0, 1900.0, 1300.0, 1800.0, 1100.0, 1800.0,
        ]);
        t
    }

    /// `compose_base` must be `compose` with both overlays off — and with
    /// nothing else off.
    ///
    /// The failure this pins is not a rounding step. It is a base-only entry
    /// point that also quietly drops the highlight ring or the feather,
    /// because both are applied by the same function as the overlays and are
    /// easy to lose when that function is split. The GL path draws NEITHER of
    /// those, so anything missing here is missing from the map.
    ///
    /// Be clear about which assertion does the work. While `compose_base`
    /// delegates to `compose` the byte-equality below CANNOT fail; it states
    /// the contract. The four assertions after it are the check -- they are
    /// what a future inlining of `compose_base` has to keep true.
    #[test]
    fn compose_base_is_compose_with_the_overlays_off_and_nothing_else_off() {
        let mut t = tile_with_everything();
        let base = t.compose_base(0, true, true);
        let both_off = t.compose(0, false, 0.0, true, false, true);
        assert_eq!(base, both_off, "compose_base differs from compose with both overlays off");

        // Not vacuous: the overlays this tile carries DO change the picture,
        // so the equality above is a statement about what was skipped rather
        // than about an empty tile.
        let with_loss = t.compose(0, true, 150.0, true, false, true);
        assert_ne!(base, with_loss, "the coverage overlay changed nothing — the test proves nothing");
        let with_net = t.compose(0, false, 0.0, true, true, true);
        assert_ne!(base, with_net, "the census overlay changed nothing — the test proves nothing");

        // The feather is still applied...
        let unfeathered = t.compose_base(0, true, false);
        assert_ne!(base, unfeathered, "compose_base ignored the feather flag");
        // ...and so is the highlight ring.
        t.clear_highlight();
        let unhighlighted = t.compose_base(0, true, true);
        assert_ne!(base, unhighlighted, "compose_base dropped the highlight ring");
    }
}
