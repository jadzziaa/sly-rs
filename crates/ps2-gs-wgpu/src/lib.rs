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

#![allow(
    clippy::cast_possible_truncation,
    reason = "a batch's counts and indices, far below 2^32"
)]

use std::sync::{Arc, Mutex};

use ps2_gs::export::{self, PRIM_WORDS, Write, flag, word};

/// Local memory, in words.
const WORDS: u64 = 1 << 20;
/// Tile size, in pixels.
const TILE: u32 = 16;
/// Tiles across (and down) the GS's 2048 × 2048 coordinate space.
const TILES: usize = 2048 / TILE as usize;
/// Workgroups a dispatch may have along one dimension.
const GROUPS_MAX: u32 = 65535;

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
        ("PRIM_WORDS", PRIM_WORDS),
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
    prims: Grown,
    cluts: Grown,
    tiles: Grown,
    order: Grown,
    writes: Grown,
    /// Each tile's primitives, while a batch is binned.
    lists: Vec<Vec<u32>>,
    /// The GPU's name and backend.
    pub name: String,
    /// Batches drawn.
    pub batches: u64,
    /// Primitives left out (kinds not drawn yet).
    pub skipped: u64,
}

impl GpuGs {
    /// Opens the GPU: the high-performance one unless `WGPU_POWER_PREF`
    /// says otherwise.
    ///
    /// # Errors
    ///
    /// If there is no GPU, or it has no 64-bit integers in shaders.
    pub fn new() -> Result<Self, String> {
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
        let mem = storage("local memory", WORDS * 4);
        let snap = storage("snapshot", WORDS * 4);
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
        let draw = pipeline(header() + include_str!("draw.wgsl"), "draw");
        let write = pipeline(include_str!("write.wgsl").to_owned(), "write");
        Ok(Self {
            device,
            queue,
            mem,
            snap,
            tables,
            draw,
            write,
            prims: Grown::new("primitives"),
            cluts: Grown::new("cluts"),
            tiles: Grown::new("tiles"),
            order: Grown::new("order"),
            writes: Grown::new("writes"),
            lists: vec![Vec::new(); TILES * TILES],
            name: format!("{} ({:?})", info.name, info.backend),
            batches: 0,
            skipped: 0,
        })
    }

    /// Local memory, once everything handed over is done.
    ///
    /// # Panics
    ///
    /// If the GPU cannot be read.
    #[must_use]
    pub fn read_memory(&self) -> Vec<u32> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: WORDS * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&self.mem, 0, &staging, 0, WORDS * 4);
        self.queue.submit([encoder.finish()]);
        let (sent, received) = std::sync::mpsc::channel();
        staging.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = sent.send(r);
        });
        let polled = self.device.poll(wgpu::PollType::wait_indefinitely());
        assert!(
            polled.is_ok() && matches!(received.recv(), Ok(Ok(()))),
            "reading the GPU's local memory back"
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

    fn dispatch(
        &self,
        pipeline: &wgpu::ComputePipeline,
        buffers: &[&wgpu::Buffer],
        groups: u32,
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
            encoder.copy_buffer_to_buffer(&self.mem, 0, &self.snap, 0, WORDS * 4);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(groups.min(GROUPS_MAX), groups.div_ceil(GROUPS_MAX), 1);
        }
        self.queue.submit([encoder.finish()]);
    }
}

impl export::Tap for GpuGs {
    fn memory(&mut self, words: &[u32]) {
        self.queue.write_buffer(&self.mem, 0, &bytes(words));
    }

    fn writes(&mut self, writes: &[Write]) {
        if writes.is_empty() {
            return;
        }
        let flat: Vec<u32> = writes.iter().flatten().copied().collect();
        let buffer = self.writes.fill(&self.device, &self.queue, &flat).clone();
        let groups = (writes.len() as u32).div_ceil(64);
        self.dispatch(&self.write, &[&self.mem, &buffer], groups, false);
    }

    fn batch(&mut self, batch: &export::Batch) {
        self.batches += 1;
        self.skipped += u64::from(batch.skipped);
        let count = batch.prims.len() / PRIM_WORDS;
        if count == 0 {
            return;
        }
        let mut used = Vec::new();
        let mut snapshot = false;
        for (i, prim) in batch.prims.as_chunks::<PRIM_WORDS>().0.iter().enumerate() {
            snapshot |= prim[word::KIND] & flag::SNAPSHOT != 0;
            let (bx, by) = (prim[word::BBOX_X], prim[word::BBOX_Y]);
            let (x0, x1, y0, y1) = (bx & 0xFFFF, bx >> 16, by & 0xFFFF, by >> 16);
            for ty in y0 / TILE..=(y1 / TILE).min(TILES as u32 - 1) {
                for tx in x0 / TILE..=(x1 / TILE).min(TILES as u32 - 1) {
                    let t = ty as usize * TILES + tx as usize;
                    if self.lists[t].is_empty() {
                        used.push(t);
                    }
                    self.lists[t].push(i as u32);
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
                order.len() as u32,
                list.len() as u32,
            ]);
            order.extend_from_slice(list);
            list.clear();
        }
        let (device, queue) = (&self.device, &self.queue);
        let prims = self.prims.fill(device, queue, &batch.prims).clone();
        let cluts = self.cluts.fill(device, queue, &batch.cluts).clone();
        let tile_buffer = self.tiles.fill(device, queue, &tiles).clone();
        let order_buffer = self.order.fill(device, queue, &order).clone();
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
            ],
            used.len() as u32,
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
}
