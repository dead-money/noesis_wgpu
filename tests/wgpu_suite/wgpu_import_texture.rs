//! `import_texture` wraps a texture the host keeps rendering into, with no
//! copy: a draw after the host's next write shows the new contents.
//!
//! The device's reference outlives the host's own, a texture Noesis can't
//! sample is refused, and `update_texture` leaves an imported texture alone.

use std::ffi::c_void;

use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    Tile, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderTargetDesc, TextureHandle, TextureRect};
use noesis_wgpu::{BatchTextures, WgpuRenderDevice};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];
const OPACITY: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

#[test]
fn imported_texture_is_sampled_live() {
    pollster::block_on(run_test());
}

async fn run_test() {
    let instance = crate::instance();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            apply_limit_buckets: false,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("no wgpu adapter");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis_wgpu import texture test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    // The host's live target, such as a paperdoll it renders each frame.
    let host_texture = |usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("host target"),
            size: wgpu::Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        })
    };
    let live =
        host_texture(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
    let unsampled = host_texture(wgpu::TextureUsages::RENDER_ATTACHMENT);
    assert!(
        rd.import_texture(&unsampled).is_none(),
        "a texture without TEXTURE_BINDING is refused"
    );

    let binding = rd.import_texture(&live).expect("live target imports");
    assert_eq!((binding.width, binding.height), (2, 2));
    assert!(!binding.has_mipmaps);

    // Skipped with a warning: the host writes imported textures.
    let rect = TextureRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    rd.update_texture(binding.handle, 0, rect, &[0; 4]);

    clear(&device, &queue, &live, wgpu::Color::RED);
    let pixel = draw(&device, &queue, &mut rd, binding.handle).await;
    assert_eq!(pixel, [255, 0, 0, 255], "the host's first write");

    clear(&device, &queue, &live, wgpu::Color::GREEN);
    let pixel = draw(&device, &queue, &mut rd, binding.handle).await;
    assert_eq!(pixel, [0, 255, 0, 255], "the host's next write, uncopied");

    // The device's reference keeps the texture alive past the host's.
    drop(live);
    let pixel = draw(&device, &queue, &mut rd, binding.handle).await;
    assert_eq!(pixel, [0, 255, 0, 255], "after the host drops its copy");

    rd.drop_texture(binding.handle);
    assert!(rd.texture(binding.handle).is_none());
}

fn clear(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, color: wgpu::Color) {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("host write"),
    });
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("host write"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(color),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    queue.submit(Some(enc.finish()));
}

/// Draws `pattern` over a fresh target and returns pixel (1, 1).
async fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &mut WgpuRenderDevice,
    pattern: TextureHandle,
) -> [u8; 4] {
    let rt = rd.create_render_target(RenderTargetDesc {
        label: "import rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });
    let tile = Tile {
        x: 0,
        y: 0,
        width: RT_SIZE,
        height: RT_SIZE,
    };
    // PosTex0 full-screen quad.
    let corners = [
        [-1.0f32, -1.0, 0.0, 1.0],
        [1.0, -1.0, 1.0, 1.0],
        [-1.0, 1.0, 0.0, 0.0],
        [-1.0, 1.0, 0.0, 0.0],
        [1.0, -1.0, 1.0, 1.0],
        [1.0, 1.0, 1.0, 0.0],
    ];
    let vb: Vec<u8> = corners
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let ib: Vec<u8> = (0u16..6).flat_map(u16::to_le_bytes).collect();
    let textures = BatchTextures {
        pattern: Some(pattern),
        ..BatchTextures::default()
    };

    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);
    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt.handle, tile);
    rd.draw_batch_with(&batch(), textures);
    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[tile]);
    rd.end_offscreen_render();

    let pixel = read_pixel(device, queue, rd, rt.resolve_texture.handle).await;
    rd.drop_render_target(rt.handle);
    rd.drop_texture(rt.resolve_texture.handle);
    pixel
}

fn batch() -> Batch {
    let nearest = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );
    Batch {
        shader: Shader::PATH_PATTERN,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 6,
        start_index: 0,
        num_indices: 6,
        pattern: std::ptr::null_mut(),
        ramps: std::ptr::null_mut(),
        image: std::ptr::null_mut(),
        glyphs: std::ptr::null_mut(),
        shadow: std::ptr::null_mut(),
        pattern_sampler: nearest,
        ramps_sampler: SamplerState::default(),
        image_sampler: SamplerState::default(),
        glyphs_sampler: SamplerState::default(),
        shadow_sampler: SamplerState::default(),
        vertex_uniforms: [
            UniformData {
                values: IDENTITY.as_ptr().cast::<c_void>(),
                num_dwords: 16,
                hash: 1,
            },
            UniformData::default(),
        ],
        pixel_uniforms: [
            UniformData {
                values: OPACITY.as_ptr().cast::<c_void>(),
                num_dwords: 4,
                hash: 2,
            },
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}

/// Pixel (1, 1) of the target.
async fn read_pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &WgpuRenderDevice,
    handle: TextureHandle,
) -> [u8; 4] {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(RT_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback"),
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: rd.texture(handle).expect("resolve registered"),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(BYTES_PER_ROW),
                rows_per_image: Some(RT_SIZE),
            },
        },
        wgpu::Extent3d {
            width: RT_SIZE,
            height: RT_SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(enc.finish()));

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        sender.send(r).expect("send");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver.recv().expect("recv").expect("map");
    let data = slice.get_mapped_range().expect("mapped range");
    let o = (BYTES_PER_ROW + 4) as usize;
    [data[o], data[o + 1], data[o + 2], data[o + 3]]
}
