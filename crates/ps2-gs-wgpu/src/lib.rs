//! The GS on a GPU (`docs/research/gpu-gs.md`): local memory in a GPU
//! buffer, the pixel pipeline in wgpu compute shaders, fed the software
//! GS's work through its tap ([`ps2_gs::export`]) and drawing the same
//! pixels.
//!
//! A batch is binned into 16 × 16 tiles on the CPU; one workgroup draws a
//! tile, one invocation a pixel, each pixel's primitives in order
//! (`draw.wgsl`). A transfer's pixels are written as words
//! (`write.wgsl`). Drawing needs 64-bit integers in shaders
//! (`SHADER_INT64`: Vulkan, DirectX 12, Metal).
//!
//! At a higher internal resolution (`scale` S above 1) local memory is
//! kept S² times, in sample planes: plane `j·S + i` is drawn with every
//! sample moved by `(i, j)/S` pixel, reading its textures from itself.
//! Plane 0 samples where the GS does: it is local memory exactly, which
//! CLUT loads and read-backs use. Transfers go to every plane; the
//! display reads each GS pixel as S × S pixels, one from each plane.
//! Points, lines and antialiased edges, walked on the CPU, are the same
//! in every plane.

#![allow(
    clippy::cast_possible_truncation,
    reason = "a batch's counts and indices, far below 2^32"
)]

use std::sync::{Arc, Mutex};

use ps2_gs::export::{self, CLUT_SLOTS, Clut, FRAG_WORDS, PRIM_WORDS, Write, flag, word};
use ps2_gs::{Frame, Privileged};

/// Local memory, in words.
const WORDS: u64 = 1 << 20;
/// Tile size, in pixels.
const TILE: u32 = 16;
/// Tiles across (and down) the GS's 2048 × 2048 coordinate space.
const TILES: usize = 2048 / TILE as usize;
/// Workgroups a dispatch may have along one dimension.
const GROUPS_MAX: u32 = 65535;

/// The shaders' constants: the internal resolution and a plane's size.
fn constants(scale: u32) -> String {
    format!("const SCALE: u32 = {scale}u;\nconst WORDS: u32 = {WORDS}u;\n")
}

/// The shader's constants: the encoding's word indices and flags.
fn header() -> String {
    let words = [
        ("KIND", word::KIND),
        ("BBOX_X", word::BBOX_X),
        ("BBOX_Y", word::BBOX_Y),
        ("FRAME_PSM", word::FRAME_PSM),
        ("FBMSK", word::FBMSK),
        ("ZBUF_PSM", word::ZBUF_PSM),
        ("TEST", word::TEST),
        ("ALPHA", word::ALPHA),
        ("TEXA", word::TEXA),
        ("FOGCOL", word::FOGCOL),
        ("TEX_PSM", word::TEX_PSM),
        ("TEX_FUNC", word::TEX_FUNC),
        ("WRAP", word::WRAP),
        ("WRAP_U", word::WRAP_U),
        ("WRAP_V", word::WRAP_V),
        ("TEX_SIZE", word::TEX_SIZE),
        ("CLUT", word::CLUT),
        ("SCANMSK", word::SCANMSK),
        ("SHIFT_U", word::SHIFT_U),
        ("SHIFT_V", word::SHIFT_V),
        ("FRAME_LAYOUT", word::FRAME_LAYOUT),
        ("DEPTH_LAYOUT", word::DEPTH_LAYOUT),
        ("TEX_LAYOUT", word::TEX_LAYOUT),
        ("POS", word::POS),
        ("Z", word::Z),
        ("RGBA", word::RGBA),
        ("FOG", word::FOG),
        ("U", word::U),
        ("V", word::V),
        ("Q", word::Q),
        ("FLAT_RGBA", word::FLAT_RGBA),
        ("SCISSOR_X", word::SCISSOR_X),
        ("SCISSOR_Y", word::SCISSOR_Y),
        ("FRAGS", word::FRAGS),
        ("FRAG_COUNT", word::FRAG_COUNT),
        ("PRIM_WORDS", PRIM_WORDS),
        ("FRAG_WORDS", FRAG_WORDS),
    ];
    let flags = [
        ("TME", flag::TME),
        ("FGE", flag::FGE),
        ("ABE", flag::ABE),
        ("FST", flag::FST),
        ("IIP", flag::IIP),
        ("ZMSK", flag::ZMSK),
        ("FBA", flag::FBA),
        ("COLCLAMP", flag::COLCLAMP),
        ("PABE", flag::PABE),
        ("LINEAR", flag::LINEAR),
        ("TCC", flag::TCC),
        ("ZBUF", flag::ZBUF),
        ("TEX", flag::TEX),
        ("SNAPSHOT", flag::SNAPSHOT),
        ("OWNED", flag::OWNED),
        ("FLAT_Z", flag::FLAT_Z),
        ("STQ", flag::STQ),
        ("AA1", flag::AA1),
        ("LISTED", flag::LISTED),
        ("RENDERED", flag::RENDERED),
    ];
    words
        .iter()
        .map(|(name, at)| format!("const W_{name}: u32 = {at}u;\n"))
        .chain(
            flags
                .iter()
                .map(|(name, bit)| format!("const F_{name}: u32 = {bit}u;\n")),
        )
        .collect()
}

/// A storage buffer that grows as needed.
struct Grown {
    label: &'static str,
    buffer: Option<wgpu::Buffer>,
}

impl Grown {
    const fn new(label: &'static str) -> Self {
        Self {
            label,
            buffer: None,
        }
    }

    /// The buffer, holding `data` (at least 16 bytes long).
    fn fill(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[u32]) -> &wgpu::Buffer {
        let size = ((data.len() * 4) as u64).max(16);
        if self.buffer.as_ref().is_none_or(|b| b.size() < size) {
            self.buffer = None;
        }
        let buffer = self.buffer.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: size.next_power_of_two(),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        if !data.is_empty() {
            queue.write_buffer(buffer, 0, &bytes(data));
        }
        buffer
    }
}

fn bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The GS's local memory and pixel pipeline on a GPU.
pub struct GpuGs {
    device: wgpu::Device,
    queue: wgpu::Queue,
    mem: wgpu::Buffer,
    snap: wgpu::Buffer,
    tables: wgpu::Buffer,
    draw: wgpu::ComputePipeline,
    write: wgpu::ComputePipeline,
    clut: wgpu::ComputePipeline,
    display: wgpu::ComputePipeline,
    /// The CLUT's versions, [`CLUT_SLOTS`] of them.
    cluts: wgpu::Buffer,
    prims: Grown,
    loads: Grown,
    params: Grown,
    tiles: Grown,
    order: Grown,
    frags: Grown,
    writes: Grown,
    /// Each tile's entries (primitive, first fragment, fragments), while a
    /// batch is binned.
    lists: Vec<Vec<[u32; 3]>>,
    /// The GPU's name and backend.
    pub name: String,
    /// The internal resolution: GS pixels are drawn as `scale` × `scale`.
    pub scale: u32,
    /// Batches drawn.
    pub batches: u64,
}

impl GpuGs {
    /// Opens the GPU: the high-performance one unless `WGPU_POWER_PREF`
    /// says otherwise.
    ///
    /// # Errors
    ///
    /// If there is no GPU, or it has no 64-bit integers in shaders.
    pub fn new() -> Result<Self, String> {
        Self::with_scale(1)
    }

    /// Opens the GPU, drawing at `scale` times the GS's resolution (1 to
    /// 4).
    ///
    /// # Errors
    ///
    /// If there is no GPU, or it has no 64-bit integers in shaders, or
    /// `scale` is not 1 to 4.
    pub fn with_scale(scale: u32) -> Result<Self, String> {
        if !(1..=4).contains(&scale) {
            return Err(format!("an internal resolution of {scale}: 1 to 4"));
        }
        let planes = u64::from(scale * scale);
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::from_env()
                        .unwrap_or(wgpu::PowerPreference::HighPerformance),
                    ..Default::default()
                }),
            )
            .map_err(|e| format!("finding a GPU: {e}"))?;
        let info = adapter.get_info();
        if !adapter.features().contains(wgpu::Features::SHADER_INT64) {
            return Err(format!(
                "{} has no 64-bit integers in shaders, which the GS needs",
                info.name
            ));
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gs"),
            required_features: wgpu::Features::SHADER_INT64,
            // Sixteen planes of local memory (64 MB) fit the defaults.
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .map_err(|e| format!("opening {}: {e}", info.name))?;
        let storage = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let mem = storage("local memory", WORDS * 4 * planes);
        let snap = storage("snapshot", WORDS * 4 * planes);
        let flat = ps2_gs::mem::page_tables_flat();
        let tables = storage("page tables", (flat.len() * 4) as u64);
        queue.write_buffer(&tables, 0, &bytes(&flat));
        let pipeline = |source: String, entry: &str| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(entry),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let draw = pipeline(
            constants(scale) + &header() + include_str!("draw.wgsl"),
            "draw",
        );
        let write = pipeline(constants(scale) + include_str!("write.wgsl"), "write");
        let clut = pipeline(
            format!("const SLOTS: u32 = {CLUT_SLOTS}u;\n") + include_str!("clut.wgsl"),
            "load",
        );
        let display = pipeline(constants(scale) + include_str!("display.wgsl"), "display");
        let cluts = storage("cluts", u64::from(CLUT_SLOTS) * 256 * 4);
        Ok(Self {
            device,
            queue,
            mem,
            snap,
            tables,
            draw,
            write,
            clut,
            display,
            cluts,
            prims: Grown::new("primitives"),
            loads: Grown::new("clut loads"),
            params: Grown::new("display"),
            tiles: Grown::new("tiles"),
            order: Grown::new("order"),
            frags: Grown::new("fragments"),
            writes: Grown::new("writes"),
            lists: vec![Vec::new(); TILES * TILES],
            name: format!("{} ({:?})", info.name, info.backend),
            scale,
            batches: 0,
        })
    }

    /// Local memory, once everything handed over is done.
    ///
    /// # Panics
    ///
    /// If the GPU cannot be read.
    #[must_use]
    pub fn read_memory(&self) -> Vec<u32> {
        self.read_back(&self.mem, WORDS * 4)
    }

    /// The first `size` bytes of `buffer`, once everything submitted is
    /// done.
    fn read_back(&self, buffer: &wgpu::Buffer, size: u64) -> Vec<u32> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let (sent, received) = std::sync::mpsc::channel();
        staging.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = sent.send(r);
        });
        let polled = self.device.poll(wgpu::PollType::wait_indefinitely());
        assert!(
            polled.is_ok() && matches!(received.recv(), Ok(Ok(()))),
            "reading back from the GPU"
        );
        let view = staging.get_mapped_range(..);
        let words = match &view {
            Ok(view) => view
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| u32::from_le_bytes(*b))
                .collect(),
            Err(_) => Vec::new(),
        };
        drop(view);
        staging.unmap();
        words
    }

    /// CLUT loads (a count, then 16 words each), in one dispatch.
    fn load_cluts(&mut self, loads: &[u32]) {
        if loads.first().is_none_or(|&n| n == 0) {
            return;
        }
        let buffer = self.loads.fill(&self.device, &self.queue, loads).clone();
        self.dispatch(
            &self.clut,
            &[&self.mem, &self.cluts, &self.tables, &buffer],
            [1, 1, 1],
            false,
        );
    }

    /// Runs `pipeline` with `buffers` bound in order: `groups` workgroups
    /// (along x, or x and y when the second is more than 1, a large x
    /// spread over y; and z), after copying local memory to the snapshot.
    fn dispatch(
        &self,
        pipeline: &wgpu::ComputePipeline,
        buffers: &[&wgpu::Buffer],
        groups: [u32; 3],
        snapshot: bool,
    ) {
        let layout = pipeline.get_bind_group_layout(0);
        let entries: Vec<wgpu::BindGroupEntry<'_>> = buffers
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &entries,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        if snapshot {
            encoder.copy_buffer_to_buffer(&self.mem, 0, &self.snap, 0, self.mem.size());
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            if groups[1] > 1 {
                pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
            } else {
                pass.dispatch_workgroups(
                    groups[0].min(GROUPS_MAX),
                    groups[0].div_ceil(GROUPS_MAX),
                    groups[2],
                );
            }
        }
        self.queue.submit([encoder.finish()]);
    }
}

impl export::Tap for GpuGs {
    fn memory(&mut self, words: &[u32]) {
        let data = bytes(words);
        for plane in 0..u64::from(self.scale * self.scale) {
            self.queue.write_buffer(&self.mem, plane * WORDS * 4, &data);
        }
    }

    fn writes(&mut self, writes: &[Write]) {
        if writes.is_empty() {
            return;
        }
        let flat: Vec<u32> = std::iter::once(writes.len() as u32)
            .chain(writes.iter().flatten().copied())
            .collect();
        let buffer = self.writes.fill(&self.device, &self.queue, &flat).clone();
        let groups = (writes.len() as u32).div_ceil(64);
        self.dispatch(
            &self.write,
            &[&self.mem, &buffer],
            [groups, 1, self.scale * self.scale],
            false,
        );
    }

    fn cluts(&mut self, cluts: &[Clut]) {
        let mut loads = vec![0];
        for change in cluts {
            match change {
                Clut::Load(words) => {
                    loads.extend_from_slice(words);
                    loads[0] += 1;
                }
                Clut::Set { version, halves } => {
                    self.load_cluts(&loads);
                    loads.truncate(1);
                    loads[0] = 0;
                    let words: Vec<u32> = (0..256)
                        .map(|i| u32::from(halves[i]) | u32::from(halves[256 + i]) << 16)
                        .collect();
                    let at = u64::from(version % CLUT_SLOTS) * 256 * 4;
                    self.queue.write_buffer(&self.cluts, at, &bytes(&words));
                }
            }
        }
        self.load_cluts(&loads);
    }

    fn display(&mut self, regs: &Privileged) -> Frame {
        let Some(params) = ps2_gs::display::gpu_display(regs) else {
            return Frame::default();
        };
        let (width, height) = (params[0] * self.scale, params[1] * self.scale);
        if width == 0 || height == 0 {
            return Frame::default();
        }
        let size = u64::from(width * height) * 4;
        let picture = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("picture"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = self.params.fill(&self.device, &self.queue, &params).clone();
        self.dispatch(
            &self.display,
            &[&self.mem, &self.tables, &params, &picture],
            [width.div_ceil(8), height.div_ceil(8), 1],
            false,
        );
        let words = self.read_back(&picture, size);
        Frame {
            width,
            height,
            scale: self.scale,
            rgb: words
                .iter()
                .flat_map(|w| [*w as u8, (w >> 8) as u8, (w >> 16) as u8])
                .collect(),
        }
    }

    fn read(&mut self) -> Vec<u32> {
        self.read_memory()
    }

    fn scale(&self) -> u32 {
        self.scale
    }

    fn batch(&mut self, batch: &export::Batch) {
        self.batches += 1;
        let count = batch.prims.len() / PRIM_WORDS;
        if count == 0 {
            return;
        }
        let mut used = Vec::new();
        let mut snapshot = false;
        // Fragments, each primitive's grouped by tile (in order within one).
        let mut binned: Vec<u32> = Vec::with_capacity(batch.frags.len());
        let mut by_tile: Vec<(usize, usize)> = Vec::new();
        for (i, prim) in batch.prims.as_chunks::<PRIM_WORDS>().0.iter().enumerate() {
            snapshot |= prim[word::KIND] & flag::SNAPSHOT != 0;
            let tile_of = |x: u32, y: u32| (y / TILE) as usize * TILES + (x / TILE) as usize;
            let mut entry =
                |t: usize, first: usize, count: usize, lists: &mut Vec<Vec<[u32; 3]>>| {
                    if lists[t].is_empty() {
                        used.push(t);
                    }
                    lists[t].push([i as u32, first as u32, count as u32]);
                };
            // Its fragments, by tile.
            let (first, count) = (prim[word::FRAGS] as usize, prim[word::FRAG_COUNT] as usize);
            by_tile.clear();
            for f in first..first + count {
                let xy = batch.frags[f * FRAG_WORDS];
                by_tile.push((tile_of(xy & 0xFFFF, xy >> 16), f));
            }
            by_tile.sort_by_key(|&(t, _)| t);
            let mut runs: Vec<(usize, usize, usize)> = Vec::new();
            for &(t, f) in &by_tile {
                let at = binned.len() / FRAG_WORDS;
                binned.extend_from_slice(&batch.frags[f * FRAG_WORDS..(f + 1) * FRAG_WORDS]);
                match runs.last_mut() {
                    Some(run) if run.0 == t => run.2 += 1,
                    _ => runs.push((t, at, 1)),
                }
            }
            if prim[word::KIND] & flag::LISTED != 0 {
                for (t, at, n) in runs {
                    entry(t, at, n, &mut self.lists);
                }
                continue;
            }
            // Its inside: every tile of its box (as `bounds` in the shader
            // makes it), with the fragments there.
            let (bx, by) = (prim[word::BBOX_X], prim[word::BBOX_Y]);
            let (mut x0, x1, mut y0, y1) = (bx & 0xFFFF, bx >> 16, by & 0xFFFF, by >> 16);
            if self.scale > 1 {
                x0 = x0.saturating_sub(1).max(prim[word::SCISSOR_X] & 0xFFFF);
                y0 = y0.saturating_sub(1).max(prim[word::SCISSOR_Y] & 0xFFFF);
            }
            let mut runs = runs.into_iter().peekable();
            for ty in y0 / TILE..=(y1 / TILE).min(TILES as u32 - 1) {
                for tx in x0 / TILE..=(x1 / TILE).min(TILES as u32 - 1) {
                    let t = ty as usize * TILES + tx as usize;
                    let (at, n) = match runs.peek() {
                        Some(&(rt, at, n)) if rt == t => {
                            runs.next();
                            (at, n)
                        }
                        _ => (0, 0),
                    };
                    entry(t, at, n, &mut self.lists);
                }
            }
        }
        let mut tiles = Vec::with_capacity(used.len() * 4);
        let mut order = Vec::new();
        for &t in &used {
            let list = &mut self.lists[t];
            tiles.extend([
                (t % TILES) as u32,
                (t / TILES) as u32,
                (order.len() / 3) as u32,
                list.len() as u32,
            ]);
            order.extend(list.iter().flatten());
            list.clear();
        }
        let (device, queue) = (&self.device, &self.queue);
        let prims = self.prims.fill(device, queue, &batch.prims).clone();
        let cluts = self.cluts.clone();
        let tile_buffer = self.tiles.fill(device, queue, &tiles).clone();
        let order_buffer = self.order.fill(device, queue, &order).clone();
        let frag_buffer = self.frags.fill(device, queue, &binned).clone();
        self.dispatch(
            &self.draw,
            &[
                &self.mem,
                &self.snap,
                &self.tables,
                &prims,
                &cluts,
                &tile_buffer,
                &order_buffer,
                &frag_buffer,
            ],
            [used.len() as u32, 1, self.scale * self.scale],
            snapshot,
        );
    }
}

/// A [`GpuGs`] shared between the GS (as its tap) and whoever reads it.
#[derive(Clone)]
pub struct Shared(pub Arc<Mutex<GpuGs>>);

impl export::Tap for Shared {
    fn memory(&mut self, words: &[u32]) {
        if let Ok(mut gpu) = self.0.lock() {
            export::Tap::memory(&mut *gpu, words);
        }
    }

    fn writes(&mut self, writes: &[Write]) {
        if let Ok(mut gpu) = self.0.lock() {
            gpu.writes(writes);
        }
    }

    fn batch(&mut self, batch: &export::Batch) {
        if let Ok(mut gpu) = self.0.lock() {
            gpu.batch(batch);
        }
    }

    fn cluts(&mut self, cluts: &[Clut]) {
        if let Ok(mut gpu) = self.0.lock() {
            gpu.cluts(cluts);
        }
    }

    fn display(&mut self, regs: &Privileged) -> Frame {
        self.0
            .lock()
            .map(|mut gpu| export::Tap::display(&mut *gpu, regs))
            .unwrap_or_default()
    }

    fn read(&mut self) -> Vec<u32> {
        self.0
            .lock()
            .map(|gpu| gpu.read_memory())
            .unwrap_or_default()
    }

    fn scale(&self) -> u32 {
        self.0.lock().map_or(1, |gpu| gpu.scale)
    }
}
