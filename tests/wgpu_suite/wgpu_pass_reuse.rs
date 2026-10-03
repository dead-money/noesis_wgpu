//! Draws into one render target share a render pass, so a tile's scissor must
//! not outlive the tile: a red draw clipped to a tile, then a green draw with no
//! tile into the same target, must leave the whole target green.

use std::ffi::c_void;

use noesis_runtime::render_device::RenderTargetDesc;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, RenderState, SamplerState, Shader, StencilMode, UniformData,
};
use noesis_wgpu::{BatchTextures, WgpuRenderDevice};

const RT_SIZE: u32 = 128;
const BYTES_PER_ROW: u32 = RT_SIZE * 4;

#[test]
fn untiled_draw_after_a_tile_covers_the_whole_target() {
    pollster::block_on(run_test());
}

#[allow(clippy::too_many_lines)]
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
        .expect("no wgpu adapter available");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis_runtime offscreen test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    let rt = rd.create_render_target(RenderTargetDesc {
        label: "test rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: true,
    });
    assert_eq!(rd.render_target_size(rt.handle), Some((RT_SIZE, RT_SIZE)));

    // Two identical fullscreen `Pos` quads; each tile's scissor limits its draw.
    let mut vb = Vec::with_capacity(96);
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
    assert_eq!(vb.len(), 96);

    let mut ib = Vec::with_capacity(24);
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    assert_eq!(ib.len(), 24);

    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    let red: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    let green: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);

    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    rd.begin_tile(
        rt.handle,
        noesis_runtime::render_device::types::Tile {
            x: 0,
            y: 0,
            width: 32,
            height: 32,
        },
    );
    let batch_left = make_rgba_batch(0, 0, &identity_mat, &red);
    rd.draw_batch_with(&batch_left, BatchTextures::default());
    rd.end_tile(rt.handle);

    let batch_full = make_rgba_batch(48, 6, &identity_mat, &green);
    rd.draw_batch_with(&batch_full, BatchTextures::default());

    rd.resolve_render_target(
        rt.handle,
        &[noesis_runtime::render_device::types::Tile {
            x: 0,
            y: 0,
            width: RT_SIZE,
            height: RT_SIZE,
        }],
    );
    rd.end_offscreen_render();

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(RT_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    {
        let resolve = rd
            .texture(rt.resolve_texture.handle)
            .expect("resolve still registered");
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback copy"),
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: resolve,
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
    }

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("readback send");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver
        .recv()
        .expect("readback recv")
        .expect("readback map");

    let data = slice.get_mapped_range().expect("mapped range");
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let offset = (y * BYTES_PER_ROW + x * 4) as usize;
        [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]
    };

    for (x, y) in [(16, 112), (100, 16), (100, 100), (127, 127)] {
        assert_eq!(pixel(x, y), [0, 255, 0, 255], "({x}, {y}) should be green");
    }

    drop(data);
    readback.unmap();

    rd.drop_render_target(rt.handle);
}

fn make_rgba_batch(
    vertex_offset: u32,
    start_index: u32,
    vs_uniforms: &[f32; 16],
    ps_uniforms: &[f32; 4],
) -> Batch {
    Batch {
        shader: Shader::RGBA,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset,
        num_vertices: 6,
        start_index,
        num_indices: 6,
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
                values: vs_uniforms.as_ptr().cast::<c_void>(),
                num_dwords: 16,
                hash: 1,
            },
            UniformData::default(),
        ],
        pixel_uniforms: [
            UniformData {
                values: ps_uniforms.as_ptr().cast::<c_void>(),
                num_dwords: 4,
                hash: 2,
            },
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}
