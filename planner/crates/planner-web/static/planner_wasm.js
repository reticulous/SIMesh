/* @ts-self-types="./planner_wasm.d.ts" */

/**
 * One region's layers, decoded once and reused for every frame.
 */
export class Tile {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        TileFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_tile_free(ptr, 0);
    }
    /**
     * World bounds of the loaded tile, so JS can tell when the view has
     * left the region and a new tile is genuinely needed.
     * @returns {Float64Array}
     */
    bounds() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.tile_bounds(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayF64FromWasm0(r0, r1).slice();
            wasm.__wbindgen_export(r0, r1 * 8, 8);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    clear_highlight() {
        wasm.tile_clear_highlight(this.__wbg_ptr);
    }
    clear_loss() {
        wasm.tile_clear_loss(this.__wbg_ptr);
    }
    clear_network() {
        wasm.tile_clear_network(this.__wbg_ptr);
    }
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
     * @param {number} layer
     * @param {boolean} show_coverage
     * @param {number} budget_db
     * @param {boolean} show_roads
     * @param {boolean} show_network
     * @param {boolean} feather
     * @returns {Uint8Array}
     */
    compose(layer, show_coverage, budget_db, show_roads, show_network, feather) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.tile_compose(retptr, this.__wbg_ptr, layer, show_coverage, budget_db, show_roads, show_network, feather);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayU8FromWasm0(r0, r1).slice();
            wasm.__wbindgen_export(r0, r1 * 1, 1);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
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
     * @param {number} layer
     * @param {boolean} show_roads
     * @param {boolean} feather
     * @returns {Uint8Array}
     */
    compose_base(layer, show_roads, feather) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.tile_compose_base(retptr, this.__wbg_ptr, layer, show_roads, feather);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayU8FromWasm0(r0, r1).slice();
            wasm.__wbindgen_export(r0, r1 * 1, 1);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * Marker drawing stays here so the canvas needs no separate overlay.
     * @param {Uint8Array} rgba
     * @param {number} min_x
     * @param {number} min_y
     * @param {number} max_x
     * @param {number} max_y
     * @param {number} width
     * @param {number} height
     * @param {number} at_x
     * @param {number} at_y
     */
    draw_marker(rgba, min_x, min_y, max_x, max_y, width, height, at_x, at_y) {
        var ptr0 = passArray8ToWasm0(rgba, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.tile_draw_marker(this.__wbg_ptr, ptr0, len0, addHeapObject(rgba), min_x, min_y, max_x, max_y, width, height, at_x, at_y);
    }
    /**
     * Build from typed arrays. Terrain arrives as i16 decimetres-above-zero
     * style scaling is the caller's business; we take plain f32 here and let
     * the server decide the wire format.
     * @param {number} origin_x
     * @param {number} origin_y
     * @param {number} res_x
     * @param {number} res_y
     * @param {number} width
     * @param {number} height
     * @param {Float32Array} terrain
     */
    constructor(origin_x, origin_y, res_x, res_y, width, height, terrain) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passArrayF32ToWasm0(terrain, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.tile_new(retptr, origin_x, origin_y, res_x, res_y, width, height, ptr0, len0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            this.__wbg_ptr = r0;
            TileFinalization.register(this, this.__wbg_ptr, this);
            return this;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * The rectangle `compose()`'s image actually covers, as
     * `[min_x, min_y, max_x, max_y]`.
     *
     * This is what the page needs to place the composed image with
     * `drawImage`, and it must be the SAME rectangle the bake was rendered
     * over — so it is derived from `bake_view` rather than restated in JS,
     * where the half-cell term would drift the moment either side changed.
     * @returns {Float64Array}
     */
    outer_bounds() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.tile_outer_bounds(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayF64FromWasm0(r0, r1).slice();
            wasm.__wbindgen_export(r0, r1 * 8, 8);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
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
     * @param {number} width
     * @param {number} height
     * @param {Uint8Array} rgba
     * @returns {boolean}
     */
    set_baked(width, height, rgba) {
        const ptr0 = passArray8ToWasm0(rgba, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.tile_set_baked(this.__wbg_ptr, width, height, ptr0, len0);
        return ret !== 0;
    }
    /**
     * @param {Float32Array} data
     */
    set_classes(data) {
        const ptr0 = passArrayF32ToWasm0(data, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.tile_set_classes(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {Float32Array} data
     */
    set_clutter(data) {
        const ptr0 = passArrayF32ToWasm0(data, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.tile_set_clutter(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Set the postal-area outline: [n, x,y × n] per ring, concatenated.
     * @param {Float32Array} flat
     */
    set_highlight(flat) {
        const ptr0 = passArrayF32ToWasm0(flat, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.tile_set_highlight(this.__wbg_ptr, ptr0, len0);
    }
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
     * @param {number} origin_x
     * @param {number} origin_y
     * @param {number} res_x
     * @param {number} res_y
     * @param {number} width
     * @param {number} height
     * @param {Float32Array} data
     */
    set_loss(origin_x, origin_y, res_x, res_y, width, height, data) {
        const ptr0 = passArrayF32ToWasm0(data, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.tile_set_loss(this.__wbg_ptr, origin_x, origin_y, res_x, res_y, width, height, ptr0, len0);
    }
    /**
     * Attach the deployed network's coverage census (see
     * `planner_render::overlay_network`). Counts, not losses.
     * @param {number} origin_x
     * @param {number} origin_y
     * @param {number} res_x
     * @param {number} res_y
     * @param {number} width
     * @param {number} height
     * @param {number} k_target
     * @param {Float32Array} data
     */
    set_network(origin_x, origin_y, res_x, res_y, width, height, k_target, data) {
        const ptr0 = passArrayF32ToWasm0(data, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.tile_set_network(this.__wbg_ptr, origin_x, origin_y, res_x, res_y, width, height, k_target, ptr0, len0);
    }
    /**
     * @param {Float32Array} data
     */
    set_population(data) {
        const ptr0 = passArrayF32ToWasm0(data, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.tile_set_population(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {Float32Array} flat
     */
    set_roads(flat) {
        const ptr0 = passArrayF32ToWasm0(flat, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.tile_set_roads(this.__wbg_ptr, ptr0, len0);
    }
}
if (Symbol.dispose) Tile.prototype[Symbol.dispose] = Tile.prototype.free;
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_copy_to_typed_array_c7f28e53671b41e8: function(arg0, arg1, arg2) {
            new Uint8Array(getObject(arg2).buffer, getObject(arg2).byteOffset, getObject(arg2).byteLength).set(getArrayU8FromWasm0(arg0, arg1));
        },
        __wbg___wbindgen_throw_bb96b2010945f0bc: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
    };
    return {
        __proto__: null,
        "./planner_wasm_bg.js": import0,
    };
}

const TileFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_tile_free(ptr, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayF64FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getFloat64ArrayMemory0().subarray(ptr / 8, ptr / 8 + len);
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

let cachedFloat32ArrayMemory0 = null;
function getFloat32ArrayMemory0() {
    if (cachedFloat32ArrayMemory0 === null || cachedFloat32ArrayMemory0.byteLength === 0) {
        cachedFloat32ArrayMemory0 = new Float32Array(wasm.memory.buffer);
    }
    return cachedFloat32ArrayMemory0;
}

let cachedFloat64ArrayMemory0 = null;
function getFloat64ArrayMemory0() {
    if (cachedFloat64ArrayMemory0 === null || cachedFloat64ArrayMemory0.byteLength === 0) {
        cachedFloat64ArrayMemory0 = new Float64Array(wasm.memory.buffer);
    }
    return cachedFloat64ArrayMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayF32ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 4, 4) >>> 0;
    getFloat32ArrayMemory0().set(arg, ptr / 4);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedFloat32ArrayMemory0 = null;
    cachedFloat64ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('planner_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
