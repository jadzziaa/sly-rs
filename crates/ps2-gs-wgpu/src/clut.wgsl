// CLUT loads (`ps2_gs::export::Clut::Load`), in order: each makes a CLUT
// version from the one before, reading local memory. One workgroup;
// invocation `i` owns word `i` of every slot (CLUT halfwords `i` and
// `256 + i`), so the loads need no barriers between them. `SLOTS` is
// prepended by the crate.

@group(0) @binding(0) var<storage, read_write> mem: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read_write> cluts: array<u32>;
@group(0) @binding(2) var<storage, read> tables: array<u32>;
// How many loads, then 16 words each.
@group(0) @binding(3) var<storage, read> loads: array<u32>;

// `Layout::locate` and the pixel's value, from the layout at `loads[at]`.
fn fetch(at: u32, x: u32, y: u32) -> u32 {
    let table = loads[at];
    let w = loads[at + 1u];
    let h = loads[at + 2u];
    let page = (y >> h) * loads[at + 3u] + (x >> w);
    let px = x & ((1u << w) - 1u);
    let py = y & ((1u << h) - 1u);
    let entry = tables[table + ((py << w) | px)];
    let word = ((loads[at + 4u] + page * 32u) * 64u + (entry >> 5u)) & 0xFFFFFu;
    return (atomicLoad(&mem[word]) >> ((entry & 31u) + loads[at + 6u])) & loads[at + 5u];
}

// Entry `j`'s pixel.
fn entry_at(base: u32, j: u32) -> vec2<u32> {
    let entries = loads[base + 1u];
    if (loads[base + 3u] & 2u) != 0u {
        return vec2<u32>(loads[base + 4u] * 16u + j, loads[base + 5u]);
    }
    if entries == 256u {
        let k = (j & 0xE7u) | ((j & 8u) << 1u) | ((j & 0x10u) >> 1u);
        return vec2<u32>(k % 16u, k / 16u);
    }
    return vec2<u32>(j % 8u, j / 8u);
}

fn read_entry(base: u32, j: u32) -> u32 {
    let at = entry_at(base, j);
    return fetch(base + 8u, at.x, at.y);
}

@compute @workgroup_size(256)
fn load(@builtin(local_invocation_index) i: u32) {
    let n = loads[0];
    for (var k = 0u; k < n; k++) {
        let base = 1u + k * 16u;
        let version = loads[base];
        let entries = loads[base + 1u];
        let csa16 = loads[base + 2u] * 16u;
        var word = cluts[((version - 1u) % SLOTS) * 256u + i];
        if (loads[base + 3u] & 1u) != 0u {
            // 32-bit entries: halves at `(CSA·16 + j) & 0xFF` and 256 on.
            let j = (i - csa16) & 0xFFu;
            if j < entries {
                word = read_entry(base, j);
            }
        } else {
            let low = (i - csa16) & 0x1FFu;
            if low < entries {
                word = (word & 0xFFFF0000u) | (read_entry(base, low) & 0xFFFFu);
            }
            let high = (i + 256u - csa16) & 0x1FFu;
            if high < entries {
                word = (word & 0xFFFFu) | ((read_entry(base, high) & 0xFFFFu) << 16u);
            }
        }
        cluts[(version % SLOTS) * 256u + i] = word;
    }
}
