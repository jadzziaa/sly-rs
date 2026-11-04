// The GS's pixel pipeline, as `ps2-gs`'s `draw.rs` runs it, for the
// primitives of one batch: one workgroup per 16 × 16 tile, one invocation
// per pixel, each pixel's primitives in order. The word indices (`W_*`)
// and flags (`F_*`) come from `ps2_gs::export`, prepended by the crate.
//
// Every value is computed as the software GS computes it: edge functions
// and weights in 64-bit integers, depth, colour and fog as exact
// quotients, texture coordinates in the shared integer arithmetic.

@group(0) @binding(0) var<storage, read_write> mem: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read> snap: array<u32>;
@group(0) @binding(2) var<storage, read> tables: array<u32>;
@group(0) @binding(3) var<storage, read> prims: array<u32>;
@group(0) @binding(4) var<storage, read> cluts: array<u32>;
// x, y (in tiles), the first entry in `order`, how many.
@group(0) @binding(5) var<storage, read> tiles: array<vec4<u32>>;
// Entries of three words: the primitive, its first fragment in this tile,
// how many.
@group(0) @binding(6) var<storage, read> order: array<u32>;
// Fragments walked on the CPU (`W_FRAG_WORDS` words each).
@group(0) @binding(7) var<storage, read> frags: array<u32>;

const NO_COVERAGE: u32 = 0xFFFFFFFFu;

// The sample plane this invocation draws (see `lib.rs`): its first word
// in `mem` and `snap`, and its sample's offset in the pixel (1/16 pixel).
var<private> base: u32;
var<private> offset: vec2<i32>;

const RATIO_MAX: u64 = 1073741824lu;

fn word(pb: u32, i: u32) -> u32 {
    return prims[pb + i];
}

fn sword(pb: u32, i: u32) -> i32 {
    return bitcast<i32>(prims[pb + i]);
}

fn has(pb: u32, f: u32) -> bool {
    return (prims[pb] & f) != 0u;
}

fn bits(v: u32, at: u32, n: u32) -> u32 {
    return (v >> at) & ((1u << n) - 1u);
}

// --- Local memory ---------------------------------------------------------

struct Pixel {
    word: u32,
    shift: u32,
    mask: u32,
}

// `Layout::locate`, from the layout's seven words at `pb + at`.
fn locate(pb: u32, at: u32, x: u32, y: u32) -> Pixel {
    let base = pb + at;
    let table = prims[base];
    let w = prims[base + 1u];
    let h = prims[base + 2u];
    let per_row = prims[base + 3u];
    let bp = prims[base + 4u];
    let page = (y >> h) * per_row + (x >> w);
    let px = x & ((1u << w) - 1u);
    let py = y & ((1u << h) - 1u);
    let entry = tables[table + ((py << w) | px)];
    let at_word = ((bp + page * 32u) * 64u + (entry >> 5u)) & 0xFFFFFu;
    return Pixel(at_word, (entry & 31u) + prims[base + 6u], prims[base + 5u]);
}

fn load(p: Pixel) -> u32 {
    return (atomicLoad(&mem[base + p.word]) >> p.shift) & p.mask;
}

fn store(p: Pixel, value: u32) {
    let at = base + p.word;
    if p.mask == 0xFFFFFFFFu {
        atomicStore(&mem[at], value);
        return;
    }
    // Other pixels may share the word: clear this one's bits, then set
    // them, which commutes with theirs.
    let m = p.mask << p.shift;
    atomicAnd(&mem[at], ~m);
    atomicOr(&mem[at], (value & p.mask) << p.shift);
}

// --- Texture ----------------------------------------------------------------

fn expand16(texa: u32, c: u32) -> u32 {
    let rgb = ((c & 0x1Fu) << 3u) | (((c >> 5u) & 0x1Fu) << 11u) | (((c >> 10u) & 0x1Fu) << 19u);
    var a = bits(texa, 0u, 8u);
    if (c & 0x8000u) != 0u {
        a = bits(texa, 16u, 8u);
    } else if bits(texa, 8u, 1u) != 0u && (c & 0x7FFFu) == 0u {
        a = 0u;
    }
    return rgb | (a << 24u);
}

fn expand24(texa: u32, c: u32) -> u32 {
    let rgb = c & 0x00FFFFFFu;
    var a = bits(texa, 0u, 8u);
    if bits(texa, 8u, 1u) != 0u && rgb == 0u {
        a = 0u;
    }
    return rgb | (a << 24u);
}

fn clut_color(pb: u32, index: u32) -> u32 {
    let func = word(pb, W_TEX_FUNC);
    let cpsm = bits(func, 4u, 4u);
    let csa = bits(func, 8u, 5u);
    let base = word(pb, W_CLUT) * 256u;
    if cpsm < 2u {
        return cluts[base + ((csa * 16u + index) & 0xFFu)];
    }
    let at = (csa * 16u + index) & 0x1FFu;
    let pair = cluts[base + (at & 0xFFu)];
    let half = select(pair & 0xFFFFu, pair >> 16u, at >= 256u);
    return expand16(word(pb, W_TEXA), half);
}

// The texel at (`u`, `v`), already wrapped, as RGBA8888.
fn texel(pb: u32, u: i32, v: i32) -> u32 {
    return texel_in(pb, base, u, v);
}

// The texel at (`u`, `v`) of the sample plane starting at `plane`.
fn texel_in(pb: u32, plane: u32, u: i32, v: i32) -> u32 {
    if !has(pb, F_TEX) {
        return 0u;
    }
    let p = locate(pb, W_TEX_LAYOUT, u32(u & 0x7FF), u32(v & 0x7FF));
    var raw: u32;
    if has(pb, F_SNAPSHOT) {
        raw = (snap[plane + p.word] >> p.shift) & p.mask;
    } else {
        raw = (atomicLoad(&mem[plane + p.word]) >> p.shift) & p.mask;
    }
    let texa = word(pb, W_TEXA);
    switch word(pb, W_TEX_PSM) {
        case 0x00u, 0x30u: {
            return raw;
        }
        case 0x01u, 0x31u: {
            return expand24(texa, raw);
        }
        case 0x02u, 0x0Au, 0x32u, 0x3Au: {
            return expand16(texa, raw);
        }
        default: {
            return clut_color(pb, raw);
        }
    }
}

fn channels(c: u32) -> vec4<i32> {
    return vec4<i32>(i32(c & 0xFFu), i32((c >> 8u) & 0xFFu), i32((c >> 16u) & 0xFFu), i32(c >> 24u));
}

// CLAMP on one axis: `mode`, the texture's `size`, MIN and MAX.
fn wrap(mode: u32, size: i32, lo: i32, hi: i32, c: i32) -> i32 {
    switch mode {
        case 0u: {
            return c & (size - 1);
        }
        case 1u: {
            return clamp(c, 0, size - 1);
        }
        case 2u: {
            return min(max(c, lo), hi);
        }
        default: {
            return (c & lo) | hi;
        }
    }
}

fn wrap_u(pb: u32, c: i32) -> i32 {
    let m = word(pb, W_WRAP_U);
    return wrap(bits(word(pb, W_WRAP), 0u, 2u), i32(word(pb, W_TEX_SIZE) & 0xFFFFu), i32(m & 0xFFFFu), i32(m >> 16u), c);
}

fn wrap_v(pb: u32, c: i32) -> i32 {
    let m = word(pb, W_WRAP_V);
    return wrap(bits(word(pb, W_WRAP), 2u, 2u), i32(word(pb, W_TEX_SIZE) >> 16u), i32(m & 0xFFFFu), i32(m >> 16u), c);
}

fn lerp(a: vec4<i32>, b: vec4<i32>, f: i32) -> vec4<i32> {
    return a + (((b - a) * f) >> vec4<u32>(4u));
}

// Texture coordinates in 1/16 texel: nearest takes `c >> 4`; bilinear
// with 4-bit weights, across then down, each step truncated.
fn sample(pb: u32, tex: vec2<i32>) -> vec4<i32> {
    if !has(pb, F_LINEAR) {
        return channels(texel(pb, wrap_u(pb, tex.x >> 4u), wrap_v(pb, tex.y >> 4u)));
    }
    let u = tex.x - 8;
    let v = tex.y - 8;
    let iu = u >> 4u;
    let iv = v >> 4u;
    let fu = u & 15;
    let fv = v & 15;
    let u0 = wrap_u(pb, iu);
    let u1 = wrap_u(pb, iu + 1);
    let v0 = wrap_v(pb, iv);
    let v1 = wrap_v(pb, iv + 1);
    let t00 = channels(texel(pb, u0, v0));
    let t10 = channels(texel(pb, u1, v0));
    let t01 = channels(texel(pb, u0, v1));
    let t11 = channels(texel(pb, u1, v1));
    return lerp(lerp(t00, t10, fu), lerp(t01, t11, fu), fv);
}

// A texture drawn into at a higher resolution, read at that resolution:
// texel (`u`, `v`) of the picture the sample planes make together
// (`SCALE` to a GS texel each way), from the plane that sampled it. CLAMP
// wraps the GS texel; one clamped to an edge takes that edge's samples.
fn fine_texel(pb: u32, u: i32, v: i32) -> vec4<i32> {
    let s = i32(SCALE);
    let su = ((u % s) + s) % s;
    let sv = ((v % s) + s) % s;
    let nu = (u - su) / s;
    let nv = (v - sv) / s;
    let wu = wrap_u(pb, nu);
    let wv = wrap_v(pb, nv);
    var iu = su;
    var iv = sv;
    if wu > nu {
        iu = 0;
    } else if wu < nu && bits(word(pb, W_WRAP), 0u, 2u) != 0u && bits(word(pb, W_WRAP), 0u, 2u) != 3u {
        iu = s - 1;
    }
    if wv > nv {
        iv = 0;
    } else if wv < nv && bits(word(pb, W_WRAP), 2u, 2u) != 0u && bits(word(pb, W_WRAP), 2u, 2u) != 3u {
        iv = s - 1;
    }
    return channels(texel_in(pb, u32(iv * s + iu) * WORDS, wu, wv));
}

// `sample` at the higher resolution (see `fine_texel`): coordinates `SCALE`
// times as fine, the same filter.
fn sample_fine(pb: u32, tex: vec2<i32>) -> vec4<i32> {
    let s = i32(SCALE);
    if !has(pb, F_LINEAR) {
        return fine_texel(pb, (tex.x * s) >> 4u, (tex.y * s) >> 4u);
    }
    let u = tex.x * s - 8;
    let v = tex.y * s - 8;
    let iu = u >> 4u;
    let iv = v >> 4u;
    let fu = u & 15;
    let fv = v & 15;
    let t00 = fine_texel(pb, iu, iv);
    let t10 = fine_texel(pb, iu + 1, iv);
    let t01 = fine_texel(pb, iu, iv + 1);
    let t11 = fine_texel(pb, iu + 1, iv + 1);
    return lerp(lerp(t00, t10, fu), lerp(t01, t11, fu), fv);
}

// --- The pixel pipeline -----------------------------------------------------

fn modulate(t: i32, v: i32) -> i32 {
    return min((t * v) >> 7u, 255);
}

fn pack16(c: u32) -> u32 {
    return ((c >> 3u) & 0x1Fu) | (((c >> 11u) & 0x1Fu) << 5u) | (((c >> 19u) & 0x1Fu) << 10u) | ((c >> 31u) << 15u);
}

fn fragment(pb: u32, x: u32, y: u32, z: u32, rgba: vec4<i32>, tex: vec2<i32>, fog: i32, coverage: u32) {
    let scanmsk = word(pb, W_SCANMSK);
    if (scanmsk == 2u && (y & 1u) == 0u) || (scanmsk == 3u && (y & 1u) == 1u) {
        return;
    }
    let t = word(pb, W_TEST);
    let fpsm = word(pb, W_FRAME_PSM);
    let at = locate(pb, W_FRAME_LAYOUT, x, y);
    let dst = load(at);
    // Destination alpha test.
    if bits(t, 14u, 1u) != 0u {
        var bit = bits(t, 15u, 1u);
        if fpsm == 0x00u {
            bit = dst >> 31u;
        } else if fpsm == 0x02u || fpsm == 0x0Au {
            bit = (dst >> 15u) & 1u;
        }
        if bit != bits(t, 15u, 1u) {
            return;
        }
    }
    // Depth test; the depth is written once the alpha test allows.
    var depth = false;
    var z_at: Pixel;
    var zz = z;
    if has(pb, F_ZBUF) {
        let zpsm = word(pb, W_ZBUF_PSM);
        var most = 0xFFFFFFFFu;
        if zpsm == 0x31u {
            most = 0x00FFFFFFu;
        } else if zpsm == 0x32u || zpsm == 0x3Au {
            most = 0xFFFFu;
        }
        zz = min(z, most);
        z_at = locate(pb, W_DEPTH_LAYOUT, x, y);
        let old = load(z_at);
        let ztst = bits(t, 17u, 2u);
        if ztst == 0u || (ztst == 2u && zz < old) || (ztst == 3u && zz <= old) {
            return;
        }
        depth = true;
    }
    var c = rgba;
    if has(pb, F_TME) {
        // The first plane samples as the GS does; the others read a texture
        // drawn into at their own resolution.
        var s: vec4<i32>;
        if base != 0u && has(pb, F_RENDERED) {
            s = sample_fine(pb, tex);
        } else {
            s = sample(pb, tex);
        }
        let tcc = has(pb, F_TCC);
        let va = c.w;
        let tfx = bits(word(pb, W_TEX_FUNC), 0u, 2u);
        if tfx == 0u {
            c = vec4<i32>(modulate(s.x, c.x), modulate(s.y, c.y), modulate(s.z, c.z), c.w);
            if tcc {
                c.w = modulate(s.w, va);
            }
        } else if tfx == 1u {
            c = vec4<i32>(s.xyz, c.w);
            if tcc {
                c.w = s.w;
            }
        } else {
            c = vec4<i32>(
                min(modulate(s.x, c.x) + va, 255),
                min(modulate(s.y, c.y) + va, 255),
                min(modulate(s.z, c.z) + va, 255),
                c.w,
            );
            if tcc {
                c.w = select(s.w, min(s.w + va, 255), tfx == 2u);
            }
        }
    }
    if has(pb, F_AA1) {
        // Coverage alpha: an edge's share of the pixel, else 0x80; it
        // replaces alpha, or with blending on only an alpha of 0x80.
        let a = select(i32(coverage >> 9u), 0x80, coverage == NO_COVERAGE);
        if !has(pb, F_ABE) || c.w == 0x80 {
            c.w = a;
        }
    }
    if has(pb, F_FGE) {
        let fc = channels(word(pb, W_FOGCOL));
        let mixed = (fog * c + (255 - fog) * fc) >> vec4<u32>(8u);
        c = vec4<i32>(mixed.xyz, c.w);
    }
    // Alpha test: which writes survive.
    var write_fb = true;
    // Antialiased edges test depth but never write it.
    var write_z = !has(pb, F_ZMSK) && coverage == NO_COVERAGE;
    var write_alpha = true;
    if bits(t, 0u, 1u) != 0u {
        let a = c.w;
        let aref = i32(bits(t, 4u, 8u));
        var passed = false;
        switch bits(t, 1u, 3u) {
            case 0u: { passed = false; }
            case 1u: { passed = true; }
            case 2u: { passed = a < aref; }
            case 3u: { passed = a <= aref; }
            case 4u: { passed = a == aref; }
            case 5u: { passed = a >= aref; }
            case 6u: { passed = a > aref; }
            default: { passed = a != aref; }
        }
        if !passed {
            switch bits(t, 12u, 2u) {
                case 0u: { return; }
                case 1u: { write_z = false; }
                case 2u: { write_fb = false; }
                default: {
                    write_z = false;
                    write_alpha = false;
                }
            }
        }
    }
    if write_z && depth {
        store(z_at, zz);
    }
    if !write_fb {
        return;
    }
    var d = dst;
    if fpsm == 0x02u || fpsm == 0x0Au {
        let e = expand16(word(pb, W_TEXA), dst & 0x7FFFu);
        d = (e & 0x00FFFFFFu) | select(0u, 0x80000000u, (dst & 0x8000u) != 0u);
    } else if fpsm == 0x01u {
        d = dst | 0x80000000u;
    }
    let dc = channels(d);
    // AA1 blends (with ALPHA) even without ABE.
    if (has(pb, F_ABE) || has(pb, F_AA1)) && (!has(pb, F_PABE) || (c.w & 0x80) != 0) {
        let al = word(pb, W_ALPHA);
        let cs = c;
        var coeff = i32(bits(al, 8u, 8u));
        let csel = bits(al, 4u, 2u);
        if csel == 0u {
            coeff = cs.w;
        } else if csel == 1u {
            coeff = dc.w;
        }
        let pa = pick(bits(al, 0u, 2u), cs, dc);
        let pbb = pick(bits(al, 2u, 2u), cs, dc);
        let pd = pick(bits(al, 6u, 2u), cs, dc);
        let blended = (((pa - pbb) * coeff) >> vec4<u32>(7u)) + pd;
        c = vec4<i32>(blended.xyz, c.w);
    }
    var rgb: vec3<i32>;
    if has(pb, F_COLCLAMP) {
        rgb = clamp(c.xyz, vec3<i32>(0), vec3<i32>(255));
    } else {
        rgb = c.xyz & vec3<i32>(0xFF);
    }
    var a = u32(c.w) & 0xFFu;
    if !write_alpha {
        a = u32(dc.w) & 0xFFu;
    }
    if has(pb, F_FBA) {
        a = a | 0x80u;
    }
    let out = u32(rgb.x) | (u32(rgb.y) << 8u) | (u32(rgb.z) << 16u) | (a << 24u);
    let fbmsk = word(pb, W_FBMSK);
    var new_value = out;
    var mask = fbmsk;
    if fpsm == 0x02u || fpsm == 0x0Au {
        new_value = pack16(out);
        mask = pack16(fbmsk);
    } else if fpsm == 0x01u {
        new_value = out & 0x00FFFFFFu;
        mask = fbmsk | 0xFF000000u;
    }
    store(at, (new_value & ~mask) | (dst & mask));
}

fn pick(sel: u32, cs: vec4<i32>, dc: vec4<i32>) -> vec4<i32> {
    if sel == 0u {
        return cs;
    } else if sel == 1u {
        return dc;
    }
    return vec4<i32>(0);
}

// --- Texture coordinates (the shared arithmetic, `fixed.rs`) ------------------

fn clz64(v: u64) -> u32 {
    let hi = u32(v >> 32u);
    if hi != 0u {
        return countLeadingZeros(hi);
    }
    return 32u + countLeadingZeros(u32(v & 0xFFFFFFFFlu));
}

fn abs64(v: i64) -> u64 {
    if v < 0li {
        return u64(-v);
    }
    return u64(v);
}

struct Reciprocal {
    negative: bool,
    zeros: u32,
    down: u64,
    up: u64,
}

fn reciprocal(d: i64) -> Reciprocal {
    let ad = max(abs64(d), 1lu);
    let zeros = clz64(ad);
    let top = (ad << zeros) >> 32u;
    let one = 1lu << 62u;
    let down = u64(floor_div(i64(one), i64(top)));
    let rest = one - down * top;
    return Reciprocal(d < 0li, zeros, down, down + select(0lu, 1lu, rest != 0lu));
}

fn ratio(r: Reciprocal, n: i64, k: i32) -> i32 {
    if n == 0li {
        return 0;
    }
    let negative = (n < 0li) != r.negative;
    let an = abs64(n);
    let zn = clz64(an);
    let top = (an << zn) >> 32u;
    let product = top * select(r.up, r.down, negative);
    let shift = 62 - k + i32(zn) - i32(r.zeros);
    var magnitude: u64;
    if shift >= 63 {
        magnitude = select(0lu, 1lu, negative && product != 0lu);
    } else if shift > 0 {
        let whole = product >> u32(shift);
        let dropped = product & ((1lu << u32(shift)) - 1lu);
        magnitude = whole + select(0lu, 1lu, negative && dropped != 0lu);
    } else if shift > -33 && product <= (RATIO_MAX >> u32(-shift)) {
        magnitude = product << u32(-shift);
    } else {
        magnitude = RATIO_MAX;
    }
    let m = i32(min(magnitude, RATIO_MAX));
    return select(m, -m, negative);
}

fn weighted(pb: u32, at: u32, w: vec3<i64>) -> i64 {
    return w.x * i64(sword(pb, at)) + w.y * i64(sword(pb, at + 1u)) + w.z * i64(sword(pb, at + 2u));
}

// `q` stepped until `n - q·d` lies in [0, d): `floor(n / d)` exactly,
// from an estimate off by a step or two.
fn settle(n: i64, d: i64, estimate: i64) -> i64 {
    var q = estimate;
    var r = n - q * d;
    loop {
        if r < 0li {
            q -= 1li;
            r += d;
        } else if r >= d {
            q += 1li;
            r -= d;
        } else {
            break;
        }
    }
    return q;
}

// `floor(n / d)` for `d > 0` and a quotient within 2^40, exactly, without
// 64-bit division (which GPUs emulate slowly): a float estimate, refined
// once from its remainder, then stepped until the remainder is in
// [0, d). The steps make it exact whatever the estimates; they are good
// enough that there are at most one or two.
fn floor_div(n: i64, d: i64) -> i64 {
    let fd = f32(d);
    var q = i64(floor(f32(n) / fd));
    var r = n - q * d;
    q += i64(floor(f32(r) / fd));
    r = n - q * d;
    loop {
        if r < 0li {
            q -= 1li;
            r += d;
        } else if r >= d {
            q += 1li;
            r -= d;
        } else {
            break;
        }
    }
    return q;
}

// --- Primitives ---------------------------------------------------------------

fn edge(ax: i32, ay: i32, bx: i32, by: i32, x: i64, y: i64) -> i64 {
    return i64(bx - ax) * (y - i64(ay)) - i64(by - ay) * (x - i64(ax));
}

// The exact quotient `floor(Σ wₖ·aₖ / area)`, estimated from the
// barycentric weights `b` (`wₖ / area` in floats; the values `aₖ` are
// within 2^24) and settled exactly.
fn exact(w: vec3<i64>, b: vec3<f32>, a: vec3<i64>, area: i64) -> i64 {
    let n = w.x * a.x + w.y * a.y + w.z * a.z;
    return settle(n, area, i64(floor(dot(b, vec3<f32>(a)))));
}

fn rgba_at(pb: u32, k: u32) -> vec4<i64> {
    let c = word(pb, W_RGBA + k);
    return vec4<i64>(i64(c & 0xFFu), i64((c >> 8u) & 0xFFu), i64((c >> 16u) & 0xFFu), i64(c >> 24u));
}

fn triangle(pb: u32, x: u32, y: u32) {
    let x0 = sword(pb, W_POS);
    let y0 = sword(pb, W_POS + 1u);
    let x1 = sword(pb, W_POS + 2u);
    let y1 = sword(pb, W_POS + 3u);
    let x2 = sword(pb, W_POS + 4u);
    let y2 = sword(pb, W_POS + 5u);
    let sx = i64(i32(x) * 16 + offset.x);
    let sy = i64(i32(y) * 16 + offset.y);
    let w = vec3<i64>(
        edge(x1, y1, x2, y2, sx, sy),
        edge(x2, y2, x0, y0, sx, sy),
        edge(x0, y0, x1, y1, sx, sy),
    );
    let kind = word(pb, W_KIND);
    for (var k = 0u; k < 3u; k++) {
        if w[k] < 0li || (w[k] == 0li && (kind & (F_OWNED << k)) == 0u) {
            return;
        }
    }
    let area = edge(x0, y0, x1, y1, i64(x2), i64(y2));
    // Floats only estimate; every value is settled exactly.
    let inv = 1.0 / f32(area);
    let b = vec3<f32>(f32(w.x), f32(w.y), f32(w.z)) * inv;
    var z = word(pb, W_Z);
    if has(pb, F_ZBUF) && !has(pb, F_FLAT_Z) {
        // Depth's sum can pass 64 bits: in high and low halves.
        let zs = vec3<u32>(word(pb, W_Z), word(pb, W_Z + 1u), word(pb, W_Z + 2u));
        let q = exact(w, b, vec3<i64>(i64(zs.x >> 16u), i64(zs.y >> 16u), i64(zs.z >> 16u)), area);
        let hi = w.x * i64(zs.x >> 16u) + w.y * i64(zs.y >> 16u) + w.z * i64(zs.z >> 16u);
        let lo = w.x * i64(zs.x & 0xFFFFu) + w.y * i64(zs.y & 0xFFFFu) + w.z * i64(zs.z & 0xFFFFu);
        let n = (hi - q * area) * 65536li + lo;
        z = u32(q * 65536li + settle(n, area, i64(floor(f32(n) * inv))));
    }
    var rgba: vec4<i32>;
    if has(pb, F_IIP) {
        let c0 = rgba_at(pb, 0u);
        let c1 = rgba_at(pb, 1u);
        let c2 = rgba_at(pb, 2u);
        for (var k = 0u; k < 4u; k++) {
            rgba[k] = i32(exact(w, b, vec3<i64>(c0[k], c1[k], c2[k]), area));
        }
    } else {
        rgba = channels(word(pb, W_FLAT_RGBA));
    }
    var fog = 0;
    if has(pb, F_FGE) {
        fog = i32(exact(w, b, vec3<i64>(i64(word(pb, W_FOG)), i64(word(pb, W_FOG + 1u)), i64(word(pb, W_FOG + 2u))), area));
    }
    var tex = vec2<i32>(0);
    if has(pb, F_TME) {
        if has(pb, F_STQ) {
            let q = reciprocal(weighted(pb, W_Q, w));
            tex = vec2<i32>(
                ratio(q, weighted(pb, W_U, w), sword(pb, W_SHIFT_U)),
                ratio(q, weighted(pb, W_V, w), sword(pb, W_SHIFT_V)),
            );
        } else {
            let u = vec3<i64>(i64(sword(pb, W_U)), i64(sword(pb, W_U + 1u)), i64(sword(pb, W_U + 2u)));
            let v = vec3<i64>(i64(sword(pb, W_V)), i64(sword(pb, W_V + 1u)), i64(sword(pb, W_V + 2u)));
            tex = vec2<i32>(i32(exact(w, b, u, area)), i32(exact(w, b, v, area)));
        }
    }
    fragment(pb, x, y, z, rgba, tex, fog, NO_COVERAGE);
}

// A sprite's texture coordinate at sample `s` between corners at `s0`
// and `s1` with coordinates `c0` and `c1` (1/16 texel).
fn sprite_coord(c0: i32, c1: i32, s0: i32, s1: i32, s: i32) -> i32 {
    let n = i64(c1 - c0) * i64(s - s0);
    let d = i64(s1 - s0);
    return i32(i64(c0) + settle(n, d, i64(floor(f32(n) / f32(d)))));
}

fn sprite(pb: u32, x: u32, y: u32) {
    // A sample is covered from the left and top corner up to, not
    // including, the right and bottom.
    let s = vec2<i32>(i32(x) * 16, i32(y) * 16) + offset;
    if s.x < sword(pb, W_POS) || s.x >= sword(pb, W_POS + 2u) || s.y < sword(pb, W_POS + 1u) || s.y >= sword(pb, W_POS + 3u) {
        return;
    }
    var tex = vec2<i32>(0);
    if has(pb, F_TME) {
        tex = vec2<i32>(
            sprite_coord(sword(pb, W_U), sword(pb, W_U + 1u), sword(pb, W_POS), sword(pb, W_POS + 2u), s.x),
            sprite_coord(sword(pb, W_V), sword(pb, W_V + 1u), sword(pb, W_POS + 1u), sword(pb, W_POS + 3u), s.y),
        );
    }
    fragment(pb, x, y, word(pb, W_Z), channels(word(pb, W_RGBA)), tex, i32(word(pb, W_FOG)), NO_COVERAGE);
}

// The box a primitive's samples may lie in: its pixels, and at a higher
// internal resolution one more to the left and top (where a sample off
// the pixel's corner can still be inside), within the scissor.
fn bounds(pb: u32) -> vec4<u32> {
    let bx = word(pb, W_BBOX_X);
    let by = word(pb, W_BBOX_Y);
    var b = vec4<u32>(bx & 0xFFFFu, bx >> 16u, by & 0xFFFFu, by >> 16u);
    if SCALE > 1u {
        b.x = max(max(b.x, 1u) - 1u, word(pb, W_SCISSOR_X) & 0xFFFFu);
        b.z = max(max(b.z, 1u) - 1u, word(pb, W_SCISSOR_Y) & 0xFFFFu);
    }
    return b;
}

@compute @workgroup_size(16, 16)
fn draw(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    base = group.z * WORDS;
    offset = vec2<i32>(i32(group.z % SCALE), i32(group.z / SCALE)) * 16 / i32(SCALE);
    let tile = tiles[group.x];
    let x = tile.x * 16u + local.x;
    let y = tile.y * 16u + local.y;
    for (var i = tile.z; i < tile.z + tile.w; i++) {
        let pb = order[3u * i] * W_PRIM_WORDS;
        let first = order[3u * i + 1u];
        let count = order[3u * i + 2u];
        let b = bounds(pb);
        if x < b.x || x > b.y || y < b.z || y > b.w {
            continue;
        }
        if !has(pb, F_LISTED) {
            let kind = word(pb, W_KIND) & 7u;
            if kind >= 6u {
                sprite(pb, x, y);
            } else {
                triangle(pb, x, y);
            }
        }
        // Fragments walked on the CPU: an antialiased triangle's edges
        // after its inside, or all of a point's, line's or STQ sprite's.
        let here = x | (y << 16u);
        for (var f = first; f < first + count; f++) {
            let at = f * W_FRAG_WORDS;
            if frags[at] != here || frags[at + 7u] != group.z {
                continue;
            }
            fragment(
                pb,
                x,
                y,
                frags[at + 1u],
                channels(frags[at + 2u]),
                vec2<i32>(bitcast<i32>(frags[at + 3u]), bitcast<i32>(frags[at + 4u])),
                bitcast<i32>(frags[at + 5u]),
                frags[at + 6u],
            );
        }
    }
}
