//! Вывод на GPU: Vello рисует сцену в текстуру, а она копируется на поверхность окна.
//! Без окна та же текстура читается обратно в PNG: так кадры проверяются,
//! ничего не показывая на экране.

use std::path::Path;
use vello::util::{DeviceHandle, RenderContext, RenderSurface};
use vello::wgpu::{self, PresentMode};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, peniko::Color};

pub struct Gpu {
    pub context: RenderContext,
    device: usize,
    renderer: Renderer,
}

fn new_renderer(handle: &DeviceHandle) -> Renderer {
    let options = RendererOptions {
        antialiasing_support: AaSupport::area_only(),
        ..RendererOptions::default()
    };
    Renderer::new(&handle.device, options).expect("рендер не создан")
}

impl Gpu {
    /// Без окна: для проверок.
    pub fn headless() -> Result<Self, String> {
        let mut context = RenderContext::new();
        let device = pollster::block_on(context.device(None)).ok_or("нет подходящей видеокарты")?;
        let renderer = new_renderer(&context.devices[device]);
        Ok(Self { context, device, renderer })
    }

    pub fn for_surface(mut context: RenderContext, surface: &RenderSurface<'_>) -> Self {
        let device = surface.dev_id;
        let renderer = new_renderer(&context.devices[device]);
        let _ = &mut context;
        Self { context, device, renderer }
    }

    fn handle(&self) -> &DeviceHandle {
        &self.context.devices[self.device]
    }

    pub fn present(&mut self, surface: &RenderSurface<'_>, scene: &Scene, base: Color) -> Result<(), String> {
        let (width, height) = (surface.config.width, surface.config.height);
        let handle = &self.context.devices[self.device];
        let params = RenderParams { base_color: base, width, height, antialiasing_method: AaConfig::Area };
        self.renderer
            .render_to_texture(&handle.device, &handle.queue, scene, &surface.target_view, &params)
            .map_err(|e| e.to_string())?;
        let frame = surface.surface.get_current_texture();
        let frame = match frame {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            _ => return Err("поверхность окна недоступна".into()),
        };
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        let view = frame.texture.create_view(&Default::default());
        surface.blitter.copy(&handle.device, &mut encoder, &surface.target_view, &view);
        handle.queue.submit([encoder.finish()]);
        handle.queue.present(frame);
        let _ = handle.device.poll(wgpu::PollType::Poll);
        Ok(())
    }

    /// Рисует сцену в текстуру и сохраняет PNG.
    pub fn render_png(&mut self, scene: &Scene, width: u32, height: u32, base: Color, path: &Path) -> Result<(), String> {
        let handle = self.handle();
        let (device, queue) = (handle.device.clone(), handle.queue.clone());
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let params = RenderParams { base_color: base, width, height, antialiasing_method: AaConfig::Area };
        self.renderer.render_to_texture(&device, &queue, scene, &view, &params).map_err(|e| e.to_string())?;

        let row = width * 4;
        let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().map_err(|e| format!("{e:?}"))?;
        let mut pixels = Vec::with_capacity((row * height) as usize);
        for line in data.chunks(padded as usize) {
            pixels.extend_from_slice(&line[..row as usize]);
        }
        drop(data);
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&pixels).map_err(|e| e.to_string())
    }
}

pub fn create_surface<'w>(
    context: &mut RenderContext,
    window: std::sync::Arc<winit::window::Window>,
    width: u32,
    height: u32,
) -> Result<RenderSurface<'w>, String> {
    pollster::block_on(context.create_surface(window, width.max(1), height.max(1), PresentMode::AutoVsync))
        .map_err(|e| e.to_string())
}
