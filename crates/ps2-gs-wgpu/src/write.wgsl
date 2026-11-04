// A transfer's pixels as writes to words of local memory: how many, then
// for each the word, the bits written, their new value. Each word comes at
// most once. (The buffer is reused: words past the count are stale.)
// Each sample plane (`id.z`) gets them all; `WORDS` is prepended.

@group(0) @binding(0) var<storage, read_write> mem: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read> writes: array<u32>;

@compute @workgroup_size(64)
fn write(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = id.y * 65535u * 64u + id.x;
    if n >= writes[0] {
        return;
    }
    let i = 1u + n * 3u;
    let at = id.z * WORDS + writes[i];
    let mask = writes[i + 1u];
    let value = writes[i + 2u];
    if mask == 0xFFFFFFFFu {
        atomicStore(&mem[at], value);
    } else {
        atomicAnd(&mem[at], ~mask);
        atomicOr(&mem[at], value & mask);
    }
}
