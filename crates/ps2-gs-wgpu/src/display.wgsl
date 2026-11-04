// The display read-out (`Gs::display`): the two read circuits merged as
// PMODE says over BGCOLOR, one invocation a pixel, RGB out. The
// parameters are `ps2_gs::display::gpu_display`'s. At a higher internal
// resolution (`SCALE`, prepended) each GS pixel is `SCALE` × `SCALE`
// pixels, each from its own sample plane.

@group(0) @binding(0) var<storage, read_write> mem: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read> tables: array<u32>;
@group(0) @binding(2) var<storage, read> params: array<u32>;
@group(0) @binding(3) var<storage, read_write> picture: array<u32>;
// The picture for the window, when it is presented rather than read out.
@group(0) @binding(4) var shown: texture_storage_2d<rgba8unorm, write>;

var<private> base: u32;

fn fetch(at: u32, x: u32, y: u32) -> u32 {
    let table = params[at];
    let w = params[at + 1u];
    let h = params[at + 2u];
    let page = (y >> h) * params[at + 3u] + (x >> w);
    let px = x & ((1u << w) - 1u);
    let py = y & ((1u << h) - 1u);
    let entry = tables[table + ((py << w) | px)];
    let word = ((params[at + 4u] + page * 32u) * 64u + (entry >> 5u)) & 0xFFFFFu;
    return (atomicLoad(&mem[base + word]) >> ((entry & 31u) + params[at + 6u])) & params[at + 5u];
}

struct Read {
    hit: bool,
    p: vec4<u32>,
}

// Circuit `c`'s pixel at (`x`, `y`) of the picture, if it covers it.
fn circuit(c: u32, x: u32, y: u32) -> Read {
    let base = 4u + 16u * c;
    let ox = params[base + 11u];
    let oy = params[base + 12u];
    if params[base] == 0u || x < ox || y < oy {
        return Read(false, vec4<u32>(0u));
    }
    let cx = x - ox;
    let cy = y - oy;
    if cx >= params[base + 13u] || cy >= params[base + 14u] {
        return Read(false, vec4<u32>(0u));
    }
    let fx = (params[base + 9u] + cx) & 0x7FFu;
    let fy = (params[base + 10u] + cy) & 0x7FFu;
    let v = fetch(base + 2u, fx, fy);
    let psm = params[base + 1u];
    if psm == 0x02u || psm == 0x0Au {
        return Read(true, vec4<u32>(
            (v & 0x1Fu) << 3u,
            ((v >> 5u) & 0x1Fu) << 3u,
            ((v >> 10u) & 0x1Fu) << 3u,
            select(0u, 0x80u, (v & 0x8000u) != 0u),
        ));
    }
    return Read(true, vec4<u32>(v & 0xFFu, (v >> 8u) & 0xFFu, (v >> 16u) & 0xFFu, v >> 24u));
}

// The picture's pixel at `at`.
fn read_out(at: vec2<u32>) -> vec3<u32> {
    base = ((at.y % SCALE) * SCALE + at.x % SCALE) * WORDS;
    let id = at.xy / SCALE;
    let bgcolor = params[2];
    let bg = vec3<u32>(bgcolor & 0xFFu, (bgcolor >> 8u) & 0xFFu, (bgcolor >> 16u) & 0xFFu);
    let mmod = (params[3] & 1u) != 0u;
    let slbg = (params[3] & 2u) != 0u;
    let alp = params[3] >> 8u;
    var back = bg;
    if !slbg {
        let second = circuit(1u, id.x, id.y);
        if second.hit {
            back = second.p.xyz;
        }
    }
    var out = back;
    let first = circuit(0u, id.x, id.y);
    if first.hit {
        let a = select(min(first.p.w * 2u, 255u), alp, mmod);
        out = (first.p.xyz * a + back * (255u - a)) / 255u;
    }
    return out;
}

@compute @workgroup_size(8, 8)
fn display(@builtin(global_invocation_id) at: vec3<u32>) {
    let width = params[0] * SCALE;
    if at.x >= width || at.y >= params[1] * SCALE {
        return;
    }
    let out = read_out(at.xy);
    picture[at.y * width + at.x] = out.x | (out.y << 8u) | (out.z << 16u);
}

// The same picture into a texture the window shows: each value `v` as
// `v / 255`, which the texture stores as `v`.
@compute @workgroup_size(8, 8)
fn present(@builtin(global_invocation_id) at: vec3<u32>) {
    if at.x >= params[0] * SCALE || at.y >= params[1] * SCALE {
        return;
    }
    textureStore(shown, at.xy, vec4<f32>(vec3<f32>(read_out(at.xy)) / 255.0, 1.0));
}
