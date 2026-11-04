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
    // field's are doubled).
    frame: vec2<u32>,
    lines: u32,
    // A picture drawn at a higher internal resolution: filtered (bilinear)
    // rather than taken pixel by pixel.
    filtered: u32,
}

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> place: Place;

// One triangle covering the window.
@vertex
fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
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
        let low = clamp(vec2<i32>(floor(at)), vec2<i32>(0), last);
        let high = min(low + 1, last);
        let w = clamp(at - floor(at), vec2<f32>(0.0), vec2<f32>(1.0));
        let top = mix(textureLoad(picture, low, 0), textureLoad(picture, vec2<i32>(high.x, low.y), 0), w.x);
        let bottom = mix(textureLoad(picture, vec2<i32>(low.x, high.y), 0), textureLoad(picture, high, 0), w.x);
        return vec4<f32>(mix(top, bottom, w.y).rgb, 1.0);
    }
    let line = p.y * place.lines / place.size.y;
    let row = min(line * place.frame.y / place.lines, place.frame.y - 1u);
    let column = p.x * place.frame.x / place.size.x;
    return vec4<f32>(textureLoad(picture, vec2<u32>(column, row), 0).rgb, 1.0);
}
";

/// The picture on the GPU.
struct Picture {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    rgba: Vec<u8>,
}

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

    /// Shows `frame` (or black), fitted to the window's size.
    pub(crate) fn draw(&mut self, frame: Option<&Frame>) -> Result<(), String> {
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
        self.painter.paint(&view, (size.width, size.height), frame);
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

/// Draws pictures into a texture of a given format.
struct Painter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    place: wgpu::Buffer,
    picture: Option<Picture>,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
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
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            device,
            queue,
            pipeline,
            layout,
            place,
            picture: None,
        })
    }

    /// Draws `frame` (or black) into `view`, `width` × `height`.
    fn paint(
        &mut self,
        view: &wgpu::TextureView,
        (width, height): (u32, u32),
        frame: Option<&Frame>,
    ) {
        let frame = frame.filter(|f| f.width > 0 && f.height > 0);
        if let Some(frame) = frame {
            self.upload(frame);
            self.queue
                .write_buffer(&self.place, 0, &place((width, height), frame));
        }
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
            if let (Some(_), Some(picture)) = (frame, &self.picture) {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &picture.bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Puts `frame` in the picture's texture, made again when its size
    /// changes.
    fn upload(&mut self, frame: &Frame) {
        let extent = wgpu::Extent3d {
            width: frame.width,
            height: frame.height,
            depth_or_array_layers: 1,
        };
        if self
            .picture
            .as_ref()
            .is_none_or(|p| p.texture.size() != extent)
        {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("picture"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("screen"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.place.as_entire_binding(),
                    },
                ],
            });
            self.picture = Some(Picture {
                texture,
                bind,
                rgba: Vec::new(),
            });
        }
        let Some(picture) = &mut self.picture else {
            return;
        };
        picture.rgba.clear();
        picture.rgba.extend(
            frame
                .rgb
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 0xFF]),
        );
        self.queue.write_texture(
            picture.texture.as_image_copy(),
            &picture.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: None,
            },
            extent,
        );
    }
}

/// Where `frame` goes in a window of `width` × `height`: at 4:3, centred,
/// as the shader's `Place`.
fn place((width, height): (u32, u32), frame: &Frame) -> [u8; 32] {
    let (tw, th) = fit((width, height));
    // A field is half the picture's lines; a frame-mode picture is whole.
    let lines = if frame.height / frame.factor() <= 256 {
        frame.height * 2
    } else {
        frame.height
    };
    let words = [
        (width - tw) / 2,
        (height - th) / 2,
        tw,
        th,
        frame.width,
        frame.height,
        lines,
        u32::from(frame.factor() > 1),
    ];
    let mut bytes = [0; 32];
    for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
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

    /// What the painter draws into a `width` × `height` texture, 0RGB.
    fn render(painter: &mut Painter, (width, height): (u32, u32), frame: &Frame) -> Vec<u32> {
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
        painter.paint(&view, (width, height), Some(frame));
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
    fn the_gpu_shows_what_the_cpu_painter_showed() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        // A machine with no GPU at all has nothing to compare.
        let Ok(adapter) = adapter(&instance, None) else {
            return;
        };
        let mut painter = Painter::new(&adapter, wgpu::TextureFormat::Rgba8Unorm).unwrap();
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut picture = |width: u32, height: u32| Frame {
            width,
            height,
            rgb: (0..width * height * 3)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed.to_le_bytes()[0]
                })
                .collect(),
            scale: 1,
        };
        // A field (its lines doubled), a frame-mode picture and a small one,
        // in windows wider, taller and exactly 4:3.
        let frames = [
            picture(640, 224),
            picture(640, 448),
            picture(512, 256),
            picture(2, 2),
        ];
        for frame in &frames {
            for size in [(1000, 613), (517, 800), (640, 480), (1920, 1080), (3, 2)] {
                let mut want = vec![0; (size.0 * size.1) as usize];
                paint(&mut want, size, frame);
                let got = render(&mut painter, size, frame);
                let wrong = want.iter().zip(&got).filter(|(a, b)| a != b).count();
                assert_eq!(
                    wrong, 0,
                    "{}×{} in {size:?}: {wrong} pixels differ",
                    frame.width, frame.height
                );
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
}
