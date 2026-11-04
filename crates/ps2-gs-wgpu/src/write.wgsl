// A transfer's pixels as writes to words of local memory: the word, the
// bits written, their new value. Each word comes at most once.

@group(0) @binding(0) var<storage, read_write> mem: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read> writes: array<u32>;

@compute @workgroup_size(64)
fn write(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = (id.y * 65535u * 64u + id.x) * 3u;
    if i + 2u >= arrayLength(&writes) {
        return;
    }
    let at = writes[i];
    let mask = writes[i + 1u];
    let value = writes[i + 2u];
    if mask == 0xFFFFFFFFu {
        atomicStore(&mem[at], value);
    } else {
        atomicAnd(&mem[at], ~mask);
        atomicOr(&mem[at], value & mask);
    }
}
