/* tslint:disable */
/* eslint-disable */

/**
 * One region's layers, decoded once and reused for every frame.
 */
export class Tile {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * World bounds of the loaded tile, so JS can tell when the view has
     * left the region and a new tile is genuinely needed.
     */
    bounds(): Float64Array;
    clear_highlight(): void;
    clear_loss(): void;
    clear_network(): void;
    /**
     * Composite the tile at ITS OWN resolution: baked base plus every overlay.
     *
     * WHY THIS REPLACED A PER-FRAME `render`. The old entry point resampled
     * the baked image to the CAMERA every frame and composited the overlays
     * per screen pixel. Measured on a 1997x1600 tile against a 1248x1000
     * canvas: 48.5 ms a frame for the base and 129.5 ms with the census on,
     * and `draw()` runs it for the overview AND the detail tile. Panning
     * dispatches one of those per pointermove, so the main thread never got
     * back to the input queue -- which is precisely the state a browser
     * reports as "this page is slow, wait or exit".
     *
     * Nothing in that work depended on the camera except the resample, and
     * resampling an image to a rectangle is what the 2D canvas does in
     * hardware. So this composes ONCE per change -- new tile, layer switch,
     * new sweep band, budget move -- and the browser's `drawImage` does every
     * camera move for free.
     *
     * `feather` fades the outer cells' alpha so a detail tile laid over the
     * coarse overview does not end in a hard rectangle: a sharp-edged square
     * of detail reads as a DATA boundary, and this pack genuinely has those.
     * It is baked in cell space rather than screen space because it is now
     * computed once per tile instead of once per frame; the visible width
     * still tracks zoom, because the whole image is scaled by `drawImage`.
     */
    compose(layer: number, show_coverage: boolean, budget_db: number, show_roads: boolean, show_network: boolean, feather: boolean): Uint8Array;
    /**
     * Everything `compose` draws EXCEPT the two coverage overlays.
     *
     * The page draws the coverage ramp and the census in a WebGL2 fragment
     * shader at SCREEN resolution instead, so the tile surface must carry
     * only what is genuinely camera-independent. It still carries the
     * highlight ring and the feather: neither is an overlay, both belong to
     * the tile, and leaving either out of the base would delete it from the
     * map rather than move it to the GPU.
     *
     * Deliberately delegates to `compose` rather than repeating the
     * bake/highlight/feather sequence. A second copy of that order is a
     * second thing to keep in step, and the one guarantee this entry point
     * owes the caller — same pixels as `compose` with both overlays off — is
     * then true by construction instead of by review.
     */
    compose_base(layer: number, show_roads: boolean, feather: boolean): Uint8Array;
    /**
     * Marker drawing stays here so the canvas needs no separate overlay.
     */
    draw_marker(rgba: Uint8Array, min_x: number, min_y: number, max_x: number, max_y: number, width: number, height: number, at_x: number, at_y: number): void;
    /**
     * Build from typed arrays. Terrain arrives as i16 decimetres-above-zero
     * style scaling is the caller's business; we take plain f32 here and let
     * the server decide the wire format.
     */
    constructor(origin_x: number, origin_y: number, res_x: number, res_y: number, width: number, height: number, terrain: Float32Array);
    /**
     * The rectangle `compose()`'s image actually covers, as
     * `[min_x, min_y, max_x, max_y]`.
     *
     * This is what the page needs to place the composed image with
     * `drawImage`, and it must be the SAME rectangle the bake was rendered
     * over — so it is derived from `bake_view` rather than restated in JS,
     * where the half-cell term would drift the moment either side changed.
     */
    outer_bounds(): Float64Array;
    /**
     * Install a base image baked elsewhere, and stop baking locally.
     *
     * The bake is the largest single piece of work in the map — measured at
     * 1075 ms without roads and 1351 ms with them on a 1997×1600 tile — and
     * it runs on the one thread the browser also uses for input, so it lands
     * as a stall in the middle of a drag. It is also embarrassingly parallel
     * over rows and depends on nothing the browser knows that the server does
     * not. So the server renders it across every core (355 ms on this pack)
     * and the browser's share becomes this copy.
     *
     * Rejected unless the image is exactly the tile's cell grid: a mismatch
     * would place the base map against the wrong ground rather than looking
     * broken, and the caller can always fall back to the local bake.
     */
    set_baked(width: number, height: number, rgba: Uint8Array): boolean;
    set_classes(data: Float32Array): void;
    set_clutter(data: Float32Array): void;
    /**
     * Set the postal-area outline: [n, x,y × n] per ring, concatenated.
     */
    set_highlight(flat: Float32Array): void;
    /**
     * Attach a loss raster (its own origin/size — the sweep window is not
     * the tile window). Re-compositing it locally is what makes the budget
     * slider instant.
     * Attach the loss raster for the CURRENT VIEW.
     *
     * Both axes, independently. One square `res` derived from the width
     * stretches the raster whenever the view aspect and the cell counts
     * disagree — which they do as soon as either axis hits its own cap — and
     * a stretched coverage overlay slides off the base map it is supposed to
     * be explaining. `tile_bin` was fixed for exactly this; the overlay
     * rasters were still carrying the single-`res` form.
     */
    set_loss(origin_x: number, origin_y: number, res_x: number, res_y: number, width: number, height: number, data: Float32Array): void;
    /**
     * Attach the deployed network's coverage census (see
     * `planner_render::overlay_network`). Counts, not losses.
     */
    set_network(origin_x: number, origin_y: number, res_x: number, res_y: number, width: number, height: number, k_target: number, data: Float32Array): void;
    set_population(data: Float32Array): void;
    set_roads(flat: Float32Array): void;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_tile_free: (a: number, b: number) => void;
    readonly tile_bounds: (a: number, b: number) => void;
    readonly tile_clear_highlight: (a: number) => void;
    readonly tile_clear_loss: (a: number) => void;
    readonly tile_clear_network: (a: number) => void;
    readonly tile_compose: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly tile_compose_base: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly tile_draw_marker: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => void;
    readonly tile_new: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly tile_outer_bounds: (a: number, b: number) => void;
    readonly tile_set_baked: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly tile_set_classes: (a: number, b: number, c: number) => void;
    readonly tile_set_clutter: (a: number, b: number, c: number) => void;
    readonly tile_set_highlight: (a: number, b: number, c: number) => void;
    readonly tile_set_loss: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly tile_set_network: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly tile_set_population: (a: number, b: number, c: number) => void;
    readonly tile_set_roads: (a: number, b: number, c: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export2: (a: number, b: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
