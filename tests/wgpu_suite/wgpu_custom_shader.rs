//! Custom pixel shaders: a brush drawn as a path pattern and as SDF text, and
//! an effect drawn as `CUSTOM_EFFECT`, each reading every input it has.
//!
//! The brush writes one input per channel: red from its image, green from
//! extra texture 0, blue from its constants. The effect swaps its input's red
//! and green. Shaders that fail to compile are rejected at registration, and
//! batches the device can't draw with a custom shader are skipped and counted.

use std::ffi::c_void;
use std::num::NonZeroU64;

use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, Tile, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderTargetDesc, TextureDesc, TextureHandle};
use noesis_wgpu::{
    BatchShader, BatchTextures, DeviceStats, PixelShaderDesc, PixelShaderHandle, PixelShaderKind,
    WgpuRenderDevice,
};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];
// One SDF texel per pixel, as in wgpu_sdf_paints.rs.
const GLYPH_SIZE: [f32; 2] = [4.0, 4.0];
const OPACITY: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
// The brush's `Constants` when the batch carries them.
const BATCH_TINT: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

const BRUSH: &str = "
struct Constants {
    tint: vec4<f32>,
}

fn main_brush(uv: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(sample_image(uv).r, sample_texture(0u, uv).g, constants.tint.b, 1.0);
}
";

const EFFECT: &str = "
fn main_effect() -> vec4<f32> {
    let input = get_input();
    return vec4<f32>(input.g, input.r, input.b, input.a);
}
";

#[test]
fn custom_brush_and_effect_shaders_draw() {
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
        .expect("no wgpu adapter");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis_wgpu custom shader test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    let brush = rd
        .create_pixel_shader(PixelShaderDesc {
            label: "brush",
            kind: PixelShaderKind::Brush,
            source: BRUSH,
            textures: 1,
            constants: 16,
        })
        .expect("brush compiles");
    let effect = rd
        .create_pixel_shader(PixelShaderDesc {
            label: "effect",
            kind: PixelShaderKind::Effect,
            source: EFFECT,
            textures: 0,
            constants: 0,
        })
        .expect("effect compiles");

    let broken = rd.create_pixel_shader(PixelShaderDesc {
        label: "broken",
        kind: PixelShaderKind::Brush,
        source: "fn main_brush(uv: vec2<f32>) -> vec4<f32> { return uv; }",
        textures: 0,
        constants: 0,
    });
    assert!(broken.is_err(), "WGSL that fails validation is rejected");
    let too_many = rd.create_pixel_shader(PixelShaderDesc {
        label: "too many textures",
        kind: PixelShaderKind::Brush,
        source: BRUSH,
        textures: 5,
        constants: 16,
    });
    assert!(
        too_many.is_err(),
        "more extra textures than the device binds"
    );

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
    let red = texture(&mut rd, "red", TextureFormat::Rgba8, &[255, 0, 0, 255]);
    let green = texture(&mut rd, "green", TextureFormat::Rgba8, &[0, 255, 0, 255]);
    let glyph = texture(&mut rd, "glyph inside", TextureFormat::R8, &[255]);

    let nearest = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );
    let extra = (green, nearest);
    let pattern_textures = BatchTextures {
        pattern: Some(red),
        glyphs: Some(glyph),
        ..BatchTextures::default()
    };

    // Constants from the host, then from the batch.
    let host_tint = [0u8; 16];
    let pixel = draw(
        &device,
        &queue,
        &mut rd,
        &path_pattern_batch(),
        pattern_textures,
        Some(brush_inputs(brush, extra, Some(&host_tint))),
    )
    .await;
    assert_eq!(pixel, [255, 255, 0, 255], "brush with host constants");

    let pixel = draw(
        &device,
        &queue,
        &mut rd,
        &path_pattern_batch(),
        pattern_textures,
        Some(brush_inputs(brush, extra, None)),
    )
    .await;
    assert_eq!(pixel, [255, 255, 255, 255], "brush with batch constants");

    let pixel = draw(
        &device,
        &queue,
        &mut rd,
        &sdf_pattern_batch(),
        pattern_textures,
        Some(brush_inputs(brush, extra, None)),
    )
    .await;
    assert_eq!(pixel, [255, 255, 255, 255], "brush painting SDF text");

    let effect_textures = BatchTextures {
        pattern: Some(red),
        ..BatchTextures::default()
    };
    let pixel = draw(
        &device,
        &queue,
        &mut rd,
        &custom_effect_batch(),
        effect_textures,
        Some(BatchShader::new(effect)),
    )
    .await;
    assert_eq!(pixel, [0, 255, 0, 255], "effect swaps red and green");

    let drawn = rd.stats();
    assert_eq!(drawn.draws, 4);

    // Skipped: the brush without its extra texture (protocol); an unknown
    // shader, an effect drawn as a path, a brush drawn as a solid path, and a
    // custom batch passed to draw_batch_with (unsupported).
    let unknown = PixelShaderHandle(NonZeroU64::new(9999).expect("nonzero"));
    let mut custom_path = path_pattern_batch();
    custom_path.pixel_shader = brush.as_ptr();
    let skipped = [
        (path_pattern_batch(), Some(BatchShader::new(brush))),
        (path_pattern_batch(), Some(BatchShader::new(unknown))),
        (path_pattern_batch(), Some(BatchShader::new(effect))),
        (path_solid_batch(), Some(brush_inputs(brush, extra, None))),
        (custom_path, None),
    ];
    for (batch, shader) in skipped {
        draw(&device, &queue, &mut rd, &batch, pattern_textures, shader).await;
    }
    assert_eq!(
        rd.stats() - drawn,
        DeviceStats {
            draws: 0,
            dropped_draws: 1,
            unsupported_shader_draws: 4,
            pipelines: rd.stats().pipelines,
        },
    );

    rd.drop_pixel_shader(brush);
    draw(
        &device,
        &queue,
        &mut rd,
        &path_pattern_batch(),
        pattern_textures,
        Some(brush_inputs(brush, extra, None)),
    )
    .await;
    assert_eq!(rd.stats().unsupported_shader_draws, 5, "a dropped shader");
}

fn brush_inputs(
    brush: PixelShaderHandle,
    extra: (TextureHandle, SamplerState),
    constants: Option<&[u8]>,
) -> BatchShader<'_> {
    let mut shader = BatchShader::new(brush);
    shader.textures[0] = Some(extra);
    shader.constants = constants;
    shader
}

/// Draws `batch` over a full-screen quad into a fresh target and returns
/// pixel (1, 1).
async fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &mut WgpuRenderDevice,
    batch: &Batch,
    textures: BatchTextures,
    shader: Option<BatchShader<'_>>,
) -> [u8; 4] {
    let rt = rd.create_render_target(RenderTargetDesc {
        label: "custom shader rt",
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
    let vb = quad(batch.shader);
    let ib: Vec<u8> = (0u16..6).flat_map(u16::to_le_bytes).collect();

    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);
    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt.handle, tile);
    match shader {
        Some(shader) => rd.draw_custom_batch(batch, textures, shader),
        None => rd.draw_batch_with(batch, textures),
    }
    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[tile]);
    rd.end_offscreen_render();

    let pixel = read_pixel(device, queue, rd, rt.resolve_texture.handle).await;
    rd.drop_render_target(rt.handle);
    rd.drop_texture(rt.resolve_texture.handle);
    pixel
}

/// A full-screen quad in `shader`'s vertex format, with every texture
/// coordinate spanning 0..1 and the effect rect covering the whole input.
fn quad(shader: Shader) -> Vec<u8> {
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
        vb.extend_from_slice(&pos[0].to_le_bytes());
        vb.extend_from_slice(&pos[1].to_le_bytes());
        match shader {
            // PosColor
            Shader::PATH_SOLID => vb.extend_from_slice(&[255, 255, 255, 255]),
            // PosTex0
            Shader::PATH_PATTERN => {
                for v in uv {
                    vb.extend_from_slice(&v.to_le_bytes());
                }
            }
            // PosTex0Tex1
            Shader::SDF_PATTERN => {
                for v in [uv[0], uv[1], uv[0], uv[1]] {
                    vb.extend_from_slice(&v.to_le_bytes());
                }
            }
            // PosColorTex0RectImagePos
            _ => {
                vb.extend_from_slice(&[255, 255, 255, 255]);
                for v in uv {
                    vb.extend_from_slice(&v.to_le_bytes());
                }
                for v in [0u16, 0, u16::MAX, u16::MAX] {
                    vb.extend_from_slice(&v.to_le_bytes());
                }
                for v in [uv[0] * 4.0, uv[1] * 4.0, 0.25, 0.25] {
                    vb.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    vb
}

fn path_pattern_batch() -> Batch {
    batch(Shader::PATH_PATTERN)
}

fn path_solid_batch() -> Batch {
    batch(Shader::PATH_SOLID)
}

fn sdf_pattern_batch() -> Batch {
    let mut batch = batch(Shader::SDF_PATTERN);
    batch.vertex_uniforms[1] = UniformData {
        values: GLYPH_SIZE.as_ptr().cast::<c_void>(),
        num_dwords: 2,
        hash: 2,
    };
    batch
}

fn custom_effect_batch() -> Batch {
    batch(Shader::CUSTOM_EFFECT)
}

fn batch(shader: Shader) -> Batch {
    let nearest = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );
    Batch {
        shader,
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
            UniformData::default(),
        ],
        pixel_uniforms: [
            UniformData {
                values: OPACITY.as_ptr().cast::<c_void>(),
                num_dwords: 4,
                hash: 3,
            },
            UniformData {
                values: BATCH_TINT.as_ptr().cast::<c_void>(),
                num_dwords: 4,
                hash: 4,
            },
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
