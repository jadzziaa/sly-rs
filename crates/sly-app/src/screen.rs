//! The window's pixels, drawn with wgpu: the latest picture uploaded as a
//! texture and shown at 4:3, centred, black around it. A shader maps
//! window pixels to picture pixels with the same integer arithmetic the
//! CPU painter used (nearest, a field's lines doubled), so the window
//! shows exactly the picture's pixels, scaled; a picture drawn at a
//! higher internal resolution is filtered instead.
//!
//! The adapter is the high-performance one unless `WGPU_POWER_PREF=low`
//! (or `WGPU_BACKEND`) says otherwise.

use std::sync::Arc;

use ps2_gs::display::Frame;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

const SHADER: &str = r"
struct Place {
    // The picture's rectangle in the window, in pixels.
    origin: vec2<u32>,
    size: vec2<u32>,
    // The picture's width and height, and the lines it stands for (a
    // small picture's are doubled).
    frame: vec2<u32>,
    lines: u32,
    // A picture drawn at a higher internal resolution: filtered (bilinear)
    // rather than taken pixel by pixel.
    filtered: u32,
    // The latest field is the whole picture (frame mode).
    whole: u32,
    // The latest field is the odd one: its lines sit one line lower.
    odd: u32,
    // Rows to a field line (the internal resolution), and its lines.
    scale: u32,
    field_lines: u32,
    // Bit 0: the field before (the other parity) can be woven in; bit 1:
    // the one before that (this parity) tells where the picture is still.
    have: u32,
}

@group(0) @binding(0) var latest: texture_2d<f32>;
@group(0) @binding(1) var before: texture_2d<f32>;
@group(0) @binding(2) var earlier: texture_2d<f32>;
@group(0) @binding(3) var<uniform> place: Place;

// One triangle covering the window.
@vertex
fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn texel(t: texture_2d<f32>, column: u32, row: u32) -> vec3<u32> {
    return vec3<u32>(round(textureLoad(t, vec2<u32>(column, row), 0).rgb * 255.0));
}

// The picture's pixel at `column`, `row`, the fields deinterlaced as the
// CPU did (`deinterlace` in the tests): the latest field's lines in their
// rows; each row between woven in from the field before where the
// picture is still, else the latest field's rows above and below
// averaged (rounded down). Still: those rows differ from the field two
// before by at most 24, summed over red, green and blue.
fn pixel(column: u32, row: u32) -> vec3<u32> {
    if place.whole != 0u {
        return texel(latest, column, row);
    }
    let s = place.scale;
    let line = row / s;
    let sub = row % s;
    let index = line / 2u;
    if line % 2u == place.odd {
        return texel(latest, column, index * s + sub);
    }
    var up = index;
    var down = min(index + 1u, place.field_lines - 1u);
    if place.odd != 0u {
        up = max(index, 1u) - 1u;
        down = index;
    }
    let above = texel(latest, column, up * s + sub);
    let below = texel(latest, column, down * s + sub);
    if place.have == 3u {
        let a = texel(earlier, column, up * s + sub);
        let b = texel(earlier, column, down * s + sub);
        let d = max(above, a) - min(above, a) + max(below, b) - min(below, b);
        if d.x + d.y + d.z <= 24u {
            return texel(before, column, index * s + sub);
        }
    }
    return (above + below) / 2u;
}

fn colour(column: u32, row: u32) -> vec4<f32> {
    return vec4<f32>(vec3<f32>(pixel(column, row)) / 255.0, 1.0);
}

@fragment
fn fragment(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<u32>(at.xy);
    if any(pixel < place.origin) || any(pixel >= place.origin + place.size) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let p = pixel - place.origin;
    if place.filtered != 0u {
        let at = (vec2<f32>(p) + 0.5) * vec2<f32>(place.frame) / vec2<f32>(place.size) - 0.5;
        let last = vec2<i32>(place.frame) - 1;
        let low = vec2<u32>(clamp(vec2<i32>(floor(at)), vec2<i32>(0), last));
        let high = min(low + 1u, vec2<u32>(last));
        let w = clamp(at - floor(at), vec2<f32>(0.0), vec2<f32>(1.0));
        let top = mix(colour(low.x, low.y), colour(high.x, low.y), w.x);
        let bottom = mix(colour(low.x, high.y), colour(high.x, high.y), w.x);
        return vec4<f32>(mix(top, bottom, w.y).rgb, 1.0);
    }
    let line = p.y * place.lines / place.size.y;
    let row = min(line * place.frame.y / place.lines, place.frame.y - 1u);
    let column = p.x * place.frame.x / place.size.x;
    return colour(column, row);
}
";

/// A field on the GPU.
struct Slot {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Its picture's size and internal resolution.
    width: u32,
    height: u32,
    scale: u32,
    /// The odd field.
    odd: bool,
}

/// Fields the window keeps: the latest and the two before it.
const KEPT: usize = 3;

/// The window's surface and what draws into it.
pub(crate) struct Screen {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    painter: Painter,
}

impl Screen {
    /// Opens the GPU for `window`.
    pub(crate) fn new(event_loop: &ActiveEventLoop, window: Arc<Window>) -> Result<Self, String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
                Box::new(event_loop.owned_display_handle()),
            ));
        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|e| format!("making the window's surface: {e}"))?;
        let adapter = adapter(&instance, Some(&surface))?;
        let capabilities = surface.get_capabilities(&adapter);
        // A format that stores the picture's values as they are: a shader
        // writing to an sRGB one would encode them.
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| capabilities.formats.first().copied())
            .ok_or("the window's surface has no formats")?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            view_formats: Vec::new(),
            ..surface
                .get_default_config(&adapter, size.width.max(1), size.height.max(1))
                .ok_or("the GPU cannot draw into the window")?
        };
        let painter = Painter::new(&adapter, format)?;
        surface.configure(&painter.device, &config);
        Ok(Self {
            window,
            surface,
            config,
            painter,
        })
    }

    /// The window.
    pub(crate) fn window(&self) -> &Window {
        &self.window
    }

    /// Keeps a field's picture (`odd`: the odd field) for the pictures
    /// drawn from now on.
    pub(crate) fn add_field(&mut self, frame: &Frame, odd: bool) {
        self.painter.add_field(frame, odd);
    }

    /// Shows the picture the latest fields make (or black), fitted to the
    /// window's size.
    pub(crate) fn draw(&mut self) -> Result<(), String> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        if (size.width, size.height) != (self.config.width, self.config.height) {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.painter.device, &self.config);
        }
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                // Drawn this once; the next draw configures again.
                self.config.width = 0;
                t
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.painter.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("the window's surface failed validation".to_owned());
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.painter.paint(&view, (size.width, size.height));
        self.window.pre_present_notify();
        self.painter.queue.present(output);
        Ok(())
    }
}

/// The adapter: the high-performance one unless the environment says
/// otherwise.
fn adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, String> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference:
            wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
        compatible_surface: surface,
        ..Default::default()
    }))
    .map_err(|e| format!("finding a GPU: {e}"))?;
    let info = adapter.get_info();
    tracing::info!("GPU: {} ({:?}, {})", info.name, info.backend, info.driver);
    Ok(adapter)
}

/// Draws the picture fields make into a texture of a given format.
struct Painter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    place: wgpu::Buffer,
    /// The latest fields, newest last.
    fields: std::collections::VecDeque<Slot>,
    /// A field's picture as RGBA, a buffer kept between uploads.
    rgba: Vec<u8>,
}

impl Painter {
    fn new(adapter: &wgpu::Adapter, format: wgpu::TextureFormat) -> Result<Self, String> {
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("sly-rs"),
            required_limits:
                wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| format!("opening the GPU: {e}"))?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("screen"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("screen"),
            entries: &[
                field_entry(0),
                field_entry(1),
                field_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("screen"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("screen"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let place = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("place"),
            size: PLACE_WORDS as u64 * 4,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            device,
            queue,
            pipeline,
            layout,
            place,
            fields: std::collections::VecDeque::new(),
            rgba: Vec::new(),
        })
    }

    /// Draws the picture the latest fields make (or black) into `view`,
    /// `width` × `height`.
    fn paint(&mut self, view: &wgpu::TextureView, (width, height): (u32, u32)) {
        let shown = self.picture((width, height)).map(|shown| {
            let bytes: Vec<u8> = shown.words.iter().flat_map(|w| w.to_le_bytes()).collect();
            self.queue.write_buffer(&self.place, 0, &bytes);
            let view = |k: Option<usize>| &self.fields[k.unwrap_or(shown.latest)].view;
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("screen"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view(None)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view(shown.before)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(view(shown.earlier)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.place.as_entire_binding(),
                    },
                ],
            })
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if let Some(bind) = &shown {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    /// The shader's `Place` for the latest fields in a window of `size`,
    /// and the slots of the latest, the field before and the one before
    /// that (when they can be used, as the CPU deinterlacer decided);
    /// `None` with no picture.
    fn picture(&self, size: (u32, u32)) -> Option<Shown> {
        let n = self.fields.len();
        let latest = self.fields.back()?;
        let scale = latest.scale.max(1);
        let field_lines = latest.height / scale;
        if latest.width == 0 || field_lines == 0 {
            return None;
        }
        // Frame mode: the field is the whole picture.
        let whole = field_lines > 256;
        let same = |k: usize| {
            n.checked_sub(k).filter(|&i| {
                let f = &self.fields[i];
                f.width == latest.width && f.height == latest.height
            })
        };
        let before = same(2).filter(|&i| self.fields[i].odd != latest.odd);
        let earlier = same(3).filter(|&i| before.is_some() && self.fields[i].odd == latest.odd);
        let picture = (
            latest.width,
            if whole {
                latest.height
            } else {
                latest.height * 2
            },
            scale,
        );
        let mut words = place(size, picture);
        words[8] = u32::from(whole);
        words[9] = u32::from(latest.odd);
        words[10] = scale;
        words[11] = field_lines;
        words[12] = u32::from(before.is_some()) | u32::from(earlier.is_some()) << 1;
        Some(Shown {
            words,
            latest: n - 1,
            before,
            earlier,
        })
    }

    /// Keeps `frame` as the latest field, in the texture of the oldest one
    /// when there are [`KEPT`] and it is the same size.
    fn add_field(&mut self, frame: &Frame, odd: bool) {
        let (width, height) = (frame.width, frame.height);
        let oldest = (self.fields.len() == KEPT)
            .then(|| self.fields.pop_front())
            .flatten();
        let extent = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };
        let texture = match oldest {
            Some(slot) if slot.texture.size() == extent => slot.texture,
            _ => self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("field"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }),
        };
        if width > 0 && height > 0 {
            self.rgba.clear();
            self.rgba.extend(
                frame
                    .rgb
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2], 0xFF]),
            );
            self.queue.write_texture(
                texture.as_image_copy(),
                &self.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: None,
                },
                extent,
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.fields.push_back(Slot {
            texture,
            view,
            width,
            height,
            scale: frame.factor(),
            odd,
        });
    }
}

/// A texture the shader reads a field from.
const fn field_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// What a picture is drawn from: the shader's `Place`, and the fields'
/// slots.
struct Shown {
    words: [u32; PLACE_WORDS],
    latest: usize,
    /// The field before, to weave in, and the one before that, to tell
    /// where the picture is still (when they can be used).
    before: Option<usize>,
    earlier: Option<usize>,
}

/// Words of the shader's `Place`.
const PLACE_WORDS: usize = 16;

/// Where a picture of `width` × `height` drawn at internal resolution
/// `scale` goes in a window of `size`: at 4:3, centred, as the first words
/// of the shader's `Place`.
fn place(size: (u32, u32), (width, height, scale): (u32, u32, u32)) -> [u32; PLACE_WORDS] {
    let (tw, th) = fit(size);
    // A small picture (a field's worth) has its lines doubled.
    let lines = if height / scale <= 256 {
        height * 2
    } else {
        height
    };
    let mut words = [0; PLACE_WORDS];
    words[..8].copy_from_slice(&[
        (size.0 - tw) / 2,
        (size.1 - th) / 2,
        tw,
        th,
        width,
        height,
        lines,
        u32::from(scale > 1),
    ]);
    words
}

/// The largest 4:3 rectangle in a window of `width` × `height`.
fn fit((width, height): (u32, u32)) -> (u32, u32) {
    if u64::from(width) * 3 >= u64::from(height) * 4 {
        (height * 4 / 3, height)
    } else {
        (width, width * 3 / 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field as the display showed it.
    struct Field {
        frame: Frame,
        /// The odd field: its lines sit one line lower in the picture.
        odd: bool,
    }

    /// Row `index` of a picture.
    fn row(frame: &Frame, index: usize) -> &[u8] {
        let stride = frame.width as usize * 3;
        &frame.rgb[index * stride..(index + 1) * stride]
    }

    /// The difference (summed over red, green and blue, above and below)
    /// under which a pixel counts as unchanged between two fields of the same
    /// parity.
    const STILL: u32 = 24;

    /// The picture the latest of `fields` (newest last) makes, deinterlaced
    /// adaptively, roughly as PCSX2's default does: the latest field's lines
    /// in their rows (the odd field's one row lower); each row between them
    /// woven in from the field before where the picture is still, and
    /// interpolated from the latest field's rows above and below where it
    /// moves. Still means the latest field's rows above and below match the
    /// field two before (the same parity). Still 2D then does not bob, still
    /// 3D shows all 448 lines, and moving edges do not comb. A frame-mode
    /// picture is already whole. A picture drawn at a higher internal
    /// resolution has several rows to a line; they go together.
    fn deinterlace(fields: &[Field]) -> Frame {
        let Some(latest) = fields.last() else {
            return Frame::default();
        };
        let scale = latest.frame.factor() as usize;
        let (width, height) = (
            latest.frame.width as usize,
            latest.frame.height as usize / scale,
        );
        if height > 256 || width == 0 || height == 0 {
            return latest.frame.clone();
        }
        let same = |f: &Field| {
            f.frame.width == latest.frame.width && f.frame.height == latest.frame.height
        };
        let back = |k: usize| {
            fields
                .len()
                .checked_sub(k)
                .map(|k| &fields[k])
                .filter(|f| same(f))
        };
        // The field before (the other parity) and the one before that (this
        // parity again).
        let before = back(2).filter(|f| f.odd != latest.odd);
        let earlier = back(3).filter(|f| before.is_some() && f.odd == latest.odd);
        let own = usize::from(latest.odd);
        let mut rgb = Vec::with_capacity(width * height * scale * 2 * 3);
        for (line, sub) in (0..height * 2).flat_map(|line| (0..scale).map(move |sub| (line, sub))) {
            let index = line / 2;
            // Row `sub` of field line `i`.
            let at = |i: usize| i * scale + sub;
            if line % 2 == own {
                rgb.extend_from_slice(row(&latest.frame, at(index)));
                continue;
            }
            // The latest field's lines above and below this line.
            let (up, down) = if latest.odd {
                (index.saturating_sub(1), index)
            } else {
                (index, (index + 1).min(height - 1))
            };
            let (up, down) = (at(up), at(down));
            let (above, below) = (row(&latest.frame, up), row(&latest.frame, down));
            let was = earlier.map(|f| (row(&f.frame, up), row(&f.frame, down)));
            let woven = before.map(|f| row(&f.frame, at(index)));
            for x in 0..width {
                let px = 3 * x..3 * x + 3;
                let differ = |a: &[u8], b: &[u8]| -> u32 {
                    a[px.clone()]
                        .iter()
                        .zip(&b[px.clone()])
                        .map(|(&p, &q)| u32::from(p.abs_diff(q)))
                        .sum()
                };
                let still = was.is_some_and(|(a, b)| differ(above, a) + differ(below, b) <= STILL);
                match woven {
                    Some(n) if still => rgb.extend_from_slice(&n[px]),
                    _ => rgb.extend(
                        above[px.clone()]
                            .iter()
                            .zip(&below[px])
                            .map(|(&a, &b)| a.midpoint(b)),
                    ),
                }
            }
        }
        Frame {
            width: latest.frame.width,
            height: latest.frame.height * 2,
            rgb,
            scale: latest.frame.scale,
        }
    }

    /// The CPU painter `play` used before wgpu: `frame` into `buffer`
    /// (`width` × `height`, 0RGB) at 4:3, centred; a field's lines doubled.
    fn paint(buffer: &mut [u32], (width, height): (u32, u32), frame: &Frame) {
        let (fw, fh) = (frame.width as usize, frame.height as usize);
        let (width, height) = (width as usize, height as usize);
        let lines = if fh <= 256 { fh * 2 } else { fh };
        let (tw, th) = if width * 3 >= height * 4 {
            (height * 4 / 3, height)
        } else {
            (width, width * 3 / 4)
        };
        let (ox, oy) = ((width - tw) / 2, (height - th) / 2);
        for y in 0..th {
            let line = y * lines / th;
            let row = (line * fh / lines).min(fh - 1);
            let source = &frame.rgb[row * fw * 3..(row + 1) * fw * 3];
            let target = &mut buffer[(oy + y) * width + ox..(oy + y) * width + ox + tw];
            for (x, pixel) in target.iter_mut().enumerate() {
                let s = (x * fw / tw) * 3;
                *pixel = u32::from(source[s]) << 16
                    | u32::from(source[s + 1]) << 8
                    | u32::from(source[s + 2]);
            }
        }
    }

    /// [`paint`] for a picture drawn at a higher internal resolution:
    /// filtered as the shader filters it, in floats.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the shader's arithmetic"
    )]
    fn paint_filtered(buffer: &mut [u32], (width, height): (u32, u32), frame: &Frame) {
        let (tw, th) = fit((width, height));
        let (ox, oy) = ((width - tw) / 2, (height - th) / 2);
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let at = |x: i64, y: i64, k: usize| -> f32 {
            let (x, y) = (x as usize, y as usize);
            f32::from(frame.rgb[(y * frame.width as usize + x) * 3 + k]) / 255.0
        };
        for y in 0..th {
            for x in 0..tw {
                let sx = (x as f32 + 0.5) * fw / tw as f32 - 0.5;
                let sy = (y as f32 + 0.5) * fh / th as f32 - 0.5;
                let (lx, ly) = (
                    (sx.floor() as i64).clamp(0, i64::from(frame.width) - 1),
                    (sy.floor() as i64).clamp(0, i64::from(frame.height) - 1),
                );
                let (hx, hy) = (
                    (lx + 1).min(i64::from(frame.width) - 1),
                    (ly + 1).min(i64::from(frame.height) - 1),
                );
                let (wx, wy) = (
                    (sx - sx.floor()).clamp(0.0, 1.0),
                    (sy - sy.floor()).clamp(0.0, 1.0),
                );
                let mix = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
                let mut pixel = 0;
                for k in 0..3 {
                    let top = mix(at(lx, ly, k), at(hx, ly, k), wx);
                    let bottom = mix(at(lx, hy, k), at(hx, hy, k), wx);
                    let v = (mix(top, bottom, wy) * 255.0).round() as u32;
                    pixel |= v << (8 * (2 - k));
                }
                buffer[((oy + y) * width + ox + x) as usize] = pixel;
            }
        }
    }

    /// What the painter draws into a `width` × `height` texture, 0RGB.
    fn render(painter: &mut Painter, (width, height): (u32, u32)) -> Vec<u32> {
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = painter.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        painter.paint(&view, (width, height));
        let stride = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = painter.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(stride * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = painter
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            extent,
        );
        painter.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
        painter
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let bytes = buffer.get_mapped_range(..).unwrap();
        (0..height as usize)
            .flat_map(|y| {
                let row = &bytes[y * stride as usize..][..width as usize * 4];
                row.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn the_gpu_deinterlaces_and_shows_what_the_cpu_did() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        // A machine with no GPU at all has nothing to compare.
        let Ok(adapter) = adapter(&instance, None) else {
            return;
        };
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // A picture, and fields of it that change in places: some pixels
        // still between fields of one parity, some not.
        let base = |random: &mut dyn FnMut() -> u64, width: u32, height: u32| -> Vec<u8> {
            (0..width * height * 3)
                .map(|_| random().to_le_bytes()[0])
                .collect()
        };
        let field = |random: &mut dyn FnMut() -> u64, picture: &[u8], width, height, scale, odd| {
            let rgb = picture
                .iter()
                .map(|&v| match random() % 8 {
                    0 => random().to_le_bytes()[1],
                    1 => v.saturating_add(3),
                    _ => v,
                })
                .collect();
            Field {
                frame: Frame {
                    width,
                    height,
                    rgb,
                    scale,
                },
                odd,
            }
        };
        // Fields at 1× (the window's integer path) and at 3× (filtered),
        // a frame-mode picture, and a change of size.
        let cases: [(u32, u32, u32); 4] = [(40, 24, 1), (33, 300, 1), (12, 6 * 3, 3), (7, 3, 1)];
        for (case, &(width, height, scale)) in cases.iter().enumerate() {
            for first_odd in [false, true] {
                let mut painter = Painter::new(&adapter, wgpu::TextureFormat::Rgba8Unorm).unwrap();
                let picture = base(&mut random, width, height);
                let mut fields: Vec<Field> = Vec::new();
                for k in 0..5 {
                    let odd = first_odd ^ (k % 2 == 1);
                    // Once, a field of another size in between.
                    let f = if case == 0 && k == 2 {
                        let other = base(&mut random, 8, 4);
                        field(&mut random, &other, 8, 4, 1, odd)
                    } else {
                        field(&mut random, &picture, width, height, scale, odd)
                    };
                    painter.add_field(&f.frame, f.odd);
                    fields.push(f);
                    let kept = &fields[fields.len().saturating_sub(KEPT)..];
                    let frame = deinterlace(kept);
                    for size in [(100, 61), (51, 80), (64, 48)] {
                        let mut want = vec![0; (size.0 * size.1) as usize];
                        // At a higher resolution the picture is filtered:
                        // floats, so within one step.
                        let step = if scale > 1 {
                            paint_filtered(&mut want, size, &frame);
                            1
                        } else {
                            paint(&mut want, size, &frame);
                            0
                        };
                        let got = render(&mut painter, size);
                        let near = |a: u32, b: u32| {
                            (0..3).all(|k| {
                                ((a >> (8 * k)) & 0xFF).abs_diff((b >> (8 * k)) & 0xFF) <= step
                            })
                        };
                        let wrong = want
                            .iter()
                            .zip(&got)
                            .filter(|&(&a, &b)| !near(a, b))
                            .count();
                        assert_eq!(
                            wrong, 0,
                            "{width}×{height} at {scale}×, field {k} (odd {odd}), in {size:?}: {wrong} pixels differ"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fields_are_line_doubled_at_four_by_three() {
        let frame = Frame {
            width: 2,
            height: 2,
            rgb: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
            scale: 1,
        };
        let mut buffer = vec![0u32; 8 * 4];
        paint(&mut buffer, (8, 4), &frame);
        // 4:3 in 8 × 4 is 5 × 4 (rounded down), starting at column 1.
        assert_eq!(buffer[1], 0xFF_0000);
        assert_eq!(buffer[4 * 8 - 1 - 2], 0xFF_FFFF);
        assert_eq!(buffer[0], 0);
    }

    fn field(rows: &[u8], odd: bool) -> Field {
        Field {
            frame: Frame {
                width: 1,
                height: u32::try_from(rows.len()).unwrap_or(0),
                rgb: rows.iter().flat_map(|&v| [v, v, v]).collect(),
                scale: 1,
            },
            odd,
        }
    }

    fn rows(frame: &Frame) -> Vec<u8> {
        frame.rgb.iter().step_by(3).copied().collect()
    }

    #[test]
    fn still_fields_weave_and_do_not_bob() {
        // The same picture in every field: the odd field's rows land one
        // line lower, and the rows between come from the other field, so
        // the picture is identical whichever field is the latest.
        let still = [10, 200, 30];
        let even = deinterlace(&[
            field(&still, false),
            field(&still, true),
            field(&still, false),
        ]);
        let odd = deinterlace(&[
            field(&still, true),
            field(&still, false),
            field(&still, true),
        ]);
        assert_eq!(rows(&even), [10, 10, 200, 200, 30, 30]);
        assert_eq!(rows(&odd), rows(&even));
    }

    #[test]
    fn moving_fields_interpolate_instead_of_combing() {
        // The latest field differs from the one two before: the field
        // before is not woven in; the latest field's neighbours are
        // averaged.
        let latest = field(&[0, 100, 200], false);
        let picture = deinterlace(&[field(&[9, 9, 9], false), field(&[50, 50, 50], true), latest]);
        assert_eq!(rows(&picture), [0, 50, 100, 150, 200, 200]);
        // With no history, the same.
        let alone = deinterlace(&[field(&[0, 100, 200], true)]);
        assert_eq!(rows(&alone), [0, 0, 50, 100, 150, 200]);
    }
}
