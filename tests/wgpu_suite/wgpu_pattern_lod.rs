//! `set_pattern_lod` shifts and caps the mip level pattern sampling picks.
//!
//! A 16x16 pattern with a different color per mip level fills a 4x4 target,
//! so the hardware picks level 2 on its own. A bias of -1 or +1 moves that one
//! level, and a max level of 1 caps it, with no wgpu feature beyond the
//! downlevel defaults.

use std::ffi::c_void;

use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, Tile, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderTargetDesc, TextureDesc, TextureHandle};
use noesis_wgpu::{BatchTextures, PatternLod, WgpuRenderDevice};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT
const PATTERN_SIZE: u32 = 16;

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];
const OPACITY: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

/// The color of each mip level, 16x16 down to 1x1.
const LEVEL_COLORS: [[u8; 4]; 5] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 0, 255],
    [255, 255, 255, 255],
];

#[test]
fn pattern_lod_biases_and_caps_the_mip_level() {
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
            label: Some("noesis_wgpu pattern lod test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    let levels: Vec<Vec<u8>> = (0..LEVEL_COLORS.len())
        .map(|level| {
            let size = (PATTERN_SIZE >> level) as usize;
            LEVEL_COLORS[level].repeat(size * size)
        })
        .collect();
    let level_slices: Vec<&[u8]> = levels.iter().map(Vec::as_slice).collect();
    let pattern = rd
        .create_texture(TextureDesc {
            label: "mip colors",
            width: PATTERN_SIZE,
            height: PATTERN_SIZE,
            num_levels: LEVEL_COLORS.len() as u32,
            format: TextureFormat::Rgba8,
            data: Some(&level_slices),
        })
        .handle;

    let cases = [
        (PatternLod::default(), 2),
        (
            PatternLod {
                bias: -1.0,
                ..PatternLod::default()
            },
            1,
        ),
        (
            PatternLod {
                bias: 1.0,
                ..PatternLod::default()
            },
            3,
        ),
        (
            PatternLod {
                bias: 0.0,
                max_level: 1.0,
            },
            1,
        ),
        (
            PatternLod {
                bias: -4.0,
                max_level: 1.0,
            },
            0,
        ),
    ];
    for (lod, level) in cases {
        rd.set_pattern_lod(lod);
        let pixel = draw(&device, &queue, &mut rd, pattern).await;
        assert_eq!(pixel, LEVEL_COLORS[level], "{lod:?} samples level {level}");
    }
}

async fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &mut WgpuRenderDevice,
    pattern: TextureHandle,
) -> [u8; 4] {
    let rt = rd.create_render_target(RenderTargetDesc {
        label: "pattern lod rt",
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
    // PosTex0: the whole pattern over the whole target, 4 texels per pixel.
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
    // Nearest mip filtering, so each case lands on exactly one level.
    let sampler = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Nearest,
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
        pattern_sampler: sampler,
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
