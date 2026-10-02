//! MSAA render targets: draws render multisampled, and
//! `resolve_render_target` resolves them into the texture Noesis samples.
//!
//! A triangle fills the lower-left half of a 4x4 target, its hypotenuse
//! through the centers of the diagonal pixels. Single-sampled, a diagonal
//! pixel is all or nothing; with 4x MSAA it resolves to partial coverage. A
//! clone of the target is multisampled too.

use std::ffi::c_void;

use noesis_runtime::render_device::types::{
    Batch, BlendMode, RenderState, SamplerState, Shader, StencilMode, Tile, UniformData,
};
use noesis_runtime::render_device::{RenderTargetBinding, RenderTargetDesc, TextureHandle};
use noesis_wgpu::{BatchTextures, WgpuRenderDevice};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

#[test]
fn msaa_target_resolves_partial_coverage() {
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
            label: Some("noesis_wgpu msaa test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    // 8 samples isn't supported everywhere, so the device uses 4.
    let msaa = rd.create_render_target(RenderTargetDesc {
        label: "msaa rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 8,
        needs_stencil: true,
    });
    let clone = rd.clone_render_target("msaa clone", msaa.handle);

    for (rt, what) in [(msaa, "msaa target"), (clone, "its clone")] {
        draw_triangle(&mut rd, rt);
        let red = |x, y| read_pixel(&device, &queue, &rd, rt.resolve_texture.handle, x, y);
        assert_eq!(red(0, 3).await[0], 255, "{what}: inside the triangle");
        assert_eq!(red(3, 0).await[0], 0, "{what}: outside the triangle");
        let edge = red(1, 1).await[0];
        assert!(
            (1..255).contains(&edge),
            "{what}: an edge pixel resolves to partial coverage, got {edge}"
        );
    }
    assert_eq!(rd.stats().draws, 2);
}

fn draw_triangle(rd: &mut WgpuRenderDevice, rt: RenderTargetBinding) {
    let tile = Tile {
        x: 0,
        y: 0,
        width: RT_SIZE,
        height: RT_SIZE,
    };
    // PosColor: bottom-left, bottom-right and top-left corners, opaque red.
    let mut vb = Vec::new();
    for pos in [[-1.0f32, -1.0], [1.0, -1.0], [-1.0, 1.0]] {
        vb.extend_from_slice(&pos[0].to_le_bytes());
        vb.extend_from_slice(&pos[1].to_le_bytes());
        vb.extend_from_slice(&[255, 0, 0, 255]);
    }
    let ib: Vec<u8> = (0u16..3).flat_map(u16::to_le_bytes).collect();

    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);
    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt.handle, tile);
    rd.draw_batch_with(&batch(), BatchTextures::default());
    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[tile]);
    rd.end_offscreen_render();
}

fn batch() -> Batch {
    Batch {
        shader: Shader::PATH_SOLID,
        render_state: RenderState::new(true, BlendMode::SrcOver, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 3,
        start_index: 0,
        num_indices: 3,
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
        pixel_uniforms: [UniformData::default(), UniformData::default()],
        pixel_shader: std::ptr::null_mut(),
    }
}

async fn read_pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &WgpuRenderDevice,
    handle: TextureHandle,
    x: u32,
    y: u32,
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
    let o = (y * BYTES_PER_ROW + x * 4) as usize;
    [data[o], data[o + 1], data[o + 2], data[o + 3]]
}
