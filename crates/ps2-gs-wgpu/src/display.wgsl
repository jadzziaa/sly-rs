// The display read-out (`Gs::display`): the two read circuits merged as
// PMODE says over BGCOLOR, one invocation a pixel, RGB out. The
// parameters are `ps2_gs::display::gpu_display`'s.

@group(0) @binding(0) var<storage, read_write> mem: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read> tables: array<u32>;
@group(0) @binding(2) var<storage, read> params: array<u32>;
@group(0) @binding(3) var<storage, read_write> picture: array<u32>;

fn fetch(at: u32, x: u32, y: u32) -> u32 {
    let table = params[at];
    let w = params[at + 1u];
    let h = params[at + 2u];
    let page = (y >> h) * params[at + 3u] + (x >> w);
    let px = x & ((1u << w) - 1u);
    let py = y & ((1u << h) - 1u);
    let entry = tables[table + ((py << w) | px)];
    let word = ((params[at + 4u] + page * 32u) * 64u + (entry >> 5u)) & 0xFFFFFu;
    return (atomicLoad(&mem[word]) >> ((entry & 31u) + params[at + 6u])) & params[at + 5u];
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

@compute @workgroup_size(8, 8)
fn display(@builtin(global_invocation_id) id: vec3<u32>) {
    let width = params[0];
    if id.x >= width || id.y >= params[1] {
        return;
    }
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
    picture[id.y * width + id.x] = out.x | (out.y << 8u) | (out.z << 16u);
}
