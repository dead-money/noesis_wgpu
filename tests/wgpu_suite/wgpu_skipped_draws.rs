//! Bad draws are skipped and counted instead of panicking, and a good draw in
//! the same phase still renders.
//!
//! Covers a draw outside a phase, a paint-texture shader with no texture, an
//! unknown texture handle, an unknown render target, geometry past the mapped
//! buffers, and shaders the device doesn't implement. Texture updates that
//! leave the texture or lack data are skipped too, and a tile larger than the
//! target is clipped to it.

use std::ffi::c_void;
use std::num::NonZeroU64;

use noesis_runtime::render_device::types::{
    Batch, BlendMode, RenderState, SamplerState, Shader, StencilMode, TextureFormat, Tile,
    UniformData,
};
use noesis_runtime::render_device::{
    RenderTargetDesc, RenderTargetHandle, TextureDesc, TextureHandle, TextureRect,
};
use noesis_wgpu::{BatchTextures, DeviceStats, WgpuRenderDevice};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

#[test]
fn bad_draws_are_skipped_and_counted() {
    pollster::block_on(run_test());
}

async fn run_test() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
            label: Some("noesis_wgpu skipped-draws test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    let rt = rd.create_render_target(RenderTargetDesc {
        label: "skipped-draws rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });
    let unknown_texture = TextureHandle(NonZeroU64::new(9999).expect("nonzero"));
    let unknown_rt = RenderTargetHandle(NonZeroU64::new(9998).expect("nonzero"));

    let vb = fullscreen_quad();
    let ib = quad_indices();
    let rgba = batch(Shader::RGBA, 6);

    // Outside any phase.
    rd.draw_batch_with(&rgba, BatchTextures::default());

    // A rect outside a 1x1 texture, then a rect with too little data.
    let tex = rd.create_texture(TextureDesc {
        label: "skipped-draws texture",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: None,
    });
    let rect = |width, height| TextureRect {
        x: 0,
        y: 0,
        width,
        height,
    };
    rd.update_texture(tex.handle, 0, rect(2, 2), &[0; 16]);
    rd.update_texture(tex.handle, 0, rect(1, 1), &[0; 2]);

    rd.begin_offscreen_render();
    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    // No render target bound yet, then an unknown one.
    rd.draw_batch_with(&rgba, BatchTextures::default());
    rd.set_render_target(unknown_rt);
    rd.draw_batch_with(&rgba, BatchTextures::default());

    rd.set_render_target(rt.handle);
    // Twice the target's size; the scissor is clipped to the target.
    rd.begin_tile(rt.handle, oversized_tile());
    // A pattern shader without a pattern texture, then with an unknown one.
    rd.draw_batch_with(&batch(Shader::PATH_PATTERN, 6), BatchTextures::default());
    let unknown = BatchTextures {
        pattern: Some(unknown_texture),
        ..BatchTextures::default()
    };
    rd.draw_batch_with(&batch(Shader::PATH_PATTERN, 6), unknown);
    // More indices than were mapped.
    rd.draw_batch_with(&batch(Shader::RGBA, 600), BatchTextures::default());
    // A custom effect, and a shader id past the SDK's table.
    rd.draw_batch_with(&batch(Shader(52), 6), BatchTextures::default());
    rd.draw_batch_with(&batch(Shader(200), 6), BatchTextures::default());
    // The one good draw.
    rd.draw_batch_with(&rgba, BatchTextures::default());
    rd.end_tile(rt.handle);
    rd.end_offscreen_render();

    assert_eq!(
        rd.stats(),
        DeviceStats {
            draws: 1,
            dropped_draws: 6,
            unsupported_shader_draws: 2,
            pipelines: 1,
        },
    );
    let pixel = read_pixel(&device, &queue, &rd, rt.resolve_texture.handle).await;
    assert_eq!(pixel, [255, 0, 0, 255], "the good draw still renders");
}

fn oversized_tile() -> Tile {
    Tile {
        x: 0,
        y: 0,
        width: RT_SIZE * 2,
        height: RT_SIZE * 2,
    }
}

/// Two triangles covering clip space, as `Pos` vertices (8 bytes each).
fn fullscreen_quad() -> Vec<u8> {
    let mut vb = Vec::with_capacity(48);
    for v in [
        [-1.0f32, -1.0],
        [1.0, -1.0],
        [-1.0, 1.0],
        [-1.0, 1.0],
        [1.0, -1.0],
        [1.0, 1.0],
    ] {
        vb.extend_from_slice(&v[0].to_le_bytes());
        vb.extend_from_slice(&v[1].to_le_bytes());
    }
    vb
}

fn quad_indices() -> Vec<u8> {
    let mut ib = Vec::with_capacity(12);
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    ib
}

/// A batch drawing `num_indices` indices with `shader`, colored [`RED`] by
/// the `RGBA` shader's pixel uniform.
fn batch(shader: Shader, num_indices: u32) -> Batch {
    Batch {
        shader,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 6,
        start_index: 0,
        num_indices,
        pattern: std::ptr::null_mut(),
        ramps: std::ptr::null_mut(),
        image: std::ptr::null_mut(),
        glyphs: std::ptr::null_mut(),
        shadow: std::ptr::null_mut(),
        pattern_sampler: SamplerState::default(),
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
                values: RED.as_ptr().cast::<c_void>(),
                num_dwords: 4,
                hash: 2,
            },
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}

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
    [data[0], data[1], data[2], data[3]]
}
