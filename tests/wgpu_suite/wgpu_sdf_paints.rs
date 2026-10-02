//! The SDF gradient and pattern paints (`SDF_LINEAR`, `SDF_RADIAL`,
//! `SDF_PATTERN` and its wrap variants) draw text with the paint's color where
//! the glyph covers the pixel, and leave the background where it doesn't.
//!
//! Each paint reads a 1x1 texture of its own color from its own slot (ramps
//! for the gradients, pattern for the rest) while the glyph atlas is a 1x1 R8
//! texture, fully inside (255) or fully outside (0) the glyph. A paint read
//! from the wrong slot, or a glyph alpha that isn't applied, shows up as the
//! wrong color.

use std::ffi::c_void;

use noesis_runtime::render_device::types::{
    Batch, BlendMode, FORMAT_FOR_VERTEX, MinMagFilter, MipFilter, RenderState, SIZE_FOR_FORMAT,
    SamplerState, Shader, StencilMode, TextureFormat, Tile, UniformData, VERTEX_FOR_SHADER,
    WrapMode,
};
use noesis_runtime::render_device::{RenderTargetDesc, TextureDesc, TextureHandle};
use noesis_wgpu::{BatchTextures, WgpuRenderDevice};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];
// uv1 spans 0..1 over the 4 px target, so `st1 = uv1 * glyph_size` moves one
// texel per pixel: the AA window stays narrow and the limits saturate.
const GLYPH_SIZE: [f32; 2] = [4.0, 4.0];
// cbuffer0_ps: opacity 1 for the linear and pattern paints (values[0].x), and
// for radial (cb[3]) with every gradient coefficient 0, so u = 0.
const PS_UNIFORMS: [f32; 8] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];

const RAMP: [u8; 4] = [0, 0, 255, 255];
const PATTERN: [u8; 4] = [255, 0, 255, 255];
const BACKGROUND: [u8; 4] = [0, 0, 0, 0];

#[derive(Copy, Clone)]
enum Layout {
    /// `PosTex0Tex1`.
    Plain,
    /// `PosTex0Tex1Rect`.
    Rect,
    /// `PosTex0Tex1RectTile`.
    RectTile,
}

#[test]
fn sdf_gradient_and_pattern_paints_fill_glyphs() {
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
            label: Some("noesis_wgpu sdf paints test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    let texture = |rd: &mut WgpuRenderDevice, label, format, texel: &[u8]| {
        let levels = [texel];
        rd.create_texture(TextureDesc {
            label,
            width: 1,
            height: 1,
            num_levels: 1,
            format,
            data: Some(&levels),
        })
        .handle
    };
    let inside = texture(&mut rd, "glyph inside", TextureFormat::R8, &[255]);
    let outside = texture(&mut rd, "glyph outside", TextureFormat::R8, &[0]);
    let ramps = texture(&mut rd, "ramp", TextureFormat::Rgba8, &RAMP);
    let pattern = texture(&mut rd, "pattern", TextureFormat::Rgba8, &PATTERN);

    let cases = [
        (Shader::SDF_LINEAR, Layout::Plain, RAMP),
        (Shader::SDF_RADIAL, Layout::Plain, RAMP),
        (Shader::SDF_PATTERN, Layout::Plain, PATTERN),
        (Shader::SDF_PATTERN_CLAMP, Layout::Rect, PATTERN),
        (Shader::SDF_PATTERN_REPEAT, Layout::RectTile, PATTERN),
        (Shader::SDF_PATTERN_MIRROR_U, Layout::RectTile, PATTERN),
        (Shader::SDF_PATTERN_MIRROR_V, Layout::RectTile, PATTERN),
        (Shader::SDF_PATTERN_MIRROR, Layout::RectTile, PATTERN),
    ];
    for (shader, layout, paint) in cases {
        let textures = |glyphs| BatchTextures {
            pattern: Some(pattern),
            ramps: Some(ramps),
            glyphs: Some(glyphs),
            ..BatchTextures::default()
        };
        let covered = draw(&device, &queue, &mut rd, shader, layout, textures(inside)).await;
        assert_eq!(covered, paint, "shader {} inside the glyph", shader.0);
        let uncovered = draw(&device, &queue, &mut rd, shader, layout, textures(outside)).await;
        assert_eq!(
            uncovered, BACKGROUND,
            "shader {} outside the glyph",
            shader.0
        );
    }
    assert_eq!(rd.stats().draws, 16, "every SDF paint draws");
}

async fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &mut WgpuRenderDevice,
    shader: Shader,
    layout: Layout,
    textures: BatchTextures,
) -> [u8; 4] {
    let rt = rd.create_render_target(RenderTargetDesc {
        label: "sdf paints rt",
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
    let (vb, stride) = quad(layout);
    assert_eq!(
        stride,
        sdk_stride(shader),
        "shader {} vertex layout",
        shader.0
    );
    let ib: Vec<u8> = (0u16..6).flat_map(u16::to_le_bytes).collect();

    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);
    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt.handle, tile);
    rd.draw_batch_with(&batch(shader), textures);
    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[tile]);
    rd.end_offscreen_render();

    let pixel = read_pixel(device, queue, rd, rt.resolve_texture.handle).await;
    rd.drop_render_target(rt.handle);
    rd.drop_texture(rt.resolve_texture.handle);
    pixel
}

/// A full-screen quad in `layout`, with uv0 and uv1 spanning 0..1, the rect
/// covering the whole pattern and the tile one pattern wide. Returns the
/// vertex bytes and the stride.
fn quad(layout: Layout) -> (Vec<u8>, u32) {
    let corners = [
        ([-1.0f32, -1.0], [0.0f32, 1.0]),
        ([1.0, -1.0], [1.0, 1.0]),
        ([-1.0, 1.0], [0.0, 0.0]),
        ([-1.0, 1.0], [0.0, 0.0]),
        ([1.0, -1.0], [1.0, 1.0]),
        ([1.0, 1.0], [1.0, 0.0]),
    ];
    let mut vb = Vec::new();
    for (pos, uv) in corners {
        for v in [pos[0], pos[1], uv[0], uv[1], uv[0], uv[1]] {
            vb.extend_from_slice(&v.to_le_bytes());
        }
        if matches!(layout, Layout::Rect | Layout::RectTile) {
            for v in [0u16, 0, u16::MAX, u16::MAX] {
                vb.extend_from_slice(&v.to_le_bytes());
            }
        }
        if matches!(layout, Layout::RectTile) {
            for v in [0.0f32, 0.0, 1.0, 1.0] {
                vb.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let stride = match layout {
        Layout::Plain => 24,
        Layout::Rect => 32,
        Layout::RectTile => 48,
    };
    (vb, stride)
}

fn batch(shader: Shader) -> Batch {
    let nearest = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );
    Batch {
        shader,
        render_state: RenderState::new(true, BlendMode::SrcOver, StencilMode::Disabled, false),
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
        ramps_sampler: nearest,
        image_sampler: nearest,
        glyphs_sampler: nearest,
        shadow_sampler: nearest,
        vertex_uniforms: [
            UniformData {
                values: IDENTITY.as_ptr().cast::<c_void>(),
                num_dwords: 16,
                hash: 1,
            },
            UniformData {
                values: GLYPH_SIZE.as_ptr().cast::<c_void>(),
                num_dwords: 2,
                hash: 2,
            },
        ],
        pixel_uniforms: [
            UniformData {
                values: PS_UNIFORMS.as_ptr().cast::<c_void>(),
                num_dwords: 8,
                hash: 3,
            },
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}

/// The SDK's vertex stride for `shader`, to check the test's layouts against.
fn sdk_stride(shader: Shader) -> u32 {
    let vertex = VERTEX_FOR_SHADER[usize::from(shader.0)];
    let format = FORMAT_FOR_VERTEX[usize::from(vertex)];
    u32::from(SIZE_FOR_FORMAT[usize::from(format)])
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
