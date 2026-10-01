//! [`WgpuRenderDevice`] and [`BatchTextures`].
//!
//! Each `begin_*_render` opens a command encoder that the matching
//! `end_*_render` submits. Inside the offscreen phase, draws go to the render
//! target chosen by `set_render_target`; inside the onscreen phase they go to
//! the view passed to [`WgpuRenderDevice::set_onscreen_target`]. Per-batch
//! uniforms and geometry are appended to per-phase buffers, so every draw
//! recorded in a phase reads its own data when the encoder is submitted.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use log::warn;

#[cfg(feature = "shim")]
use noesis_runtime::render_device::RenderDevice;
use noesis_runtime::render_device::types::{
    Batch, DeviceCaps, SIZE_FOR_FORMAT, SamplerState, Shader, TextureFormat, Tile,
};
use noesis_runtime::render_device::{
    RenderTargetBinding, RenderTargetDesc, RenderTargetHandle, TextureBinding, TextureDesc,
    TextureHandle, TextureRect,
};

use crate::pipeline::{PipelineCache, PipelineKey, STENCIL_FORMAT};

const DYNAMIC_VB_SIZE: u64 = 512 * 1024;
const DYNAMIC_IB_SIZE: u64 = 128 * 1024;

// cbuffer0_vs (mat4, 64B) + cbuffer1_vs (glyph-atlas size padded to vec4,
// 16B); matches `VsUniforms` in noesis.wgsl.
const VS_UNIFORM_SIZE: u64 = 80;
/// Byte offset of `cbuffer1_vs` within the VS uniform slot.
const VS_GLYPH_SIZE_OFFSET: usize = 64;

// cbuffer0_ps: 8 floats.
const PS_UNIFORM0_SIZE: u64 = 32;

// First 8 floats of cbuffer1_ps (float[128] in the SDK); SHADOW reads 7, BLUR 1.
const PS_UNIFORM1_SIZE: u64 = 32;

// Initial `draw_batch_with` calls per render phase; the rings double past it.
const UNIFORM_RING_SLOTS: u32 = 1024;

/// Color format every RT allocates with, and the format the pipeline cache
/// compiles all pipelines against. Onscreen views handed to
/// [`WgpuRenderDevice::set_onscreen_target`] must also be this format.
const RT_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const RT_STENCIL_FORMAT: wgpu::TextureFormat = STENCIL_FORMAT;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FramePhase {
    /// No encoder live.
    Idle,
    /// Inside `begin_offscreen_render` / `end_offscreen_render`. Draws target
    /// the currently-selected render target.
    Offscreen,
    /// Inside `begin_onscreen_render` / `end_onscreen_render`. Draws target
    /// `target_view`.
    Onscreen,
}

/// A Noesis render device that draws with wgpu.
///
/// Noesis calls it with textures, render targets and per-batch geometry, and
/// it turns each call into wgpu work: uploads, pipelines from its pipeline
/// cache, and draws recorded into the active encoder.
///
/// Create one with [`WgpuRenderDevice::new`], then drive it one of two ways:
///
/// - Register it with `noesis_runtime`'s shim through
///   `noesis_runtime::render_device::register` (`shim` feature). Noesis then
///   calls the [`RenderDevice`](noesis_runtime::render_device::RenderDevice)
///   methods itself; reach the device between frames with
///   `Registered::device_mut`.
/// - Call the protocol methods yourself, from a host that receives Noesis's
///   device calls some other way. Pass each batch to
///   [`draw_batch_with`](Self::draw_batch_with) with its textures resolved to
///   handles.
///
/// Either way, point it at an `Rgba8Unorm` target with
/// [`set_onscreen_target`](Self::set_onscreen_target) before the onscreen
/// phase, and use it from the thread that drives the Noesis view and renderer.
///
/// If a call panics partway through a phase (`noesis_runtime`'s trampoline
/// catches the panic), the next `begin_*_render` logs a warning and starts a
/// clean phase.
pub struct WgpuRenderDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,

    vertex_stream: GeometryStream,
    index_stream: GeometryStream,

    uniforms: UniformRings,

    // group(2) paint texture. Shaders without one bind `dummy_pattern_bg`.
    pattern_bind_group_layout: wgpu::BindGroupLayout,
    dummy_pattern_bg: wgpu::BindGroup,
    samplers: HashMap<SamplerState, wgpu::Sampler>,
    pattern_bind_groups: HashMap<(TextureHandle, SamplerState), wgpu::BindGroup>,

    // group(3) image (bindings 0/1) and shadow (2/3). Unused slots get the
    // dummy texture.
    image_bind_group_layout: wgpu::BindGroupLayout,
    dummy_image_bg: wgpu::BindGroup,
    image_bind_groups: HashMap<ImageBindGroupKey, wgpu::BindGroup>,

    // 1x1 white; fills unused texture slots.
    #[allow(dead_code)] // owns the allocation behind `dummy_view`
    dummy_texture: wgpu::Texture,
    dummy_view: wgpu::TextureView,
    dummy_sampler: wgpu::Sampler,

    pipelines: PipelineCache,
    // Shaders already reported as unsupported.
    warned_shaders: HashSet<u8>,

    // Entries removed by `drop_texture` / `drop_render_target`.
    textures: HashMap<TextureHandle, GpuTexture>,
    render_targets: HashMap<RenderTargetHandle, GpuRenderTarget>,

    target_view: Option<wgpu::TextureView>,
    // Onscreen draws clip with the stencil too (e.g. ScrollViewer viewports).
    onscreen_stencil: Option<GpuStencil>,
    // Noesis assumes a zeroed stencil, so the first draw into a target after
    // `begin_onscreen_render` / `set_render_target` clears it.
    onscreen_stencil_cleared: bool,
    current_rt_stencil_cleared: bool,

    phase: FramePhase,
    encoder: Option<wgpu::CommandEncoder>,
    current_rt: Option<RenderTargetHandle>,
    current_tile: Option<Tile>,

    next_handle: u64,
}

/// Why [`WgpuRenderDevice::draw_batch_with`] skipped a batch.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SkippedDraw {
    /// The call broke the render-device protocol or named an unknown handle.
    Protocol,
    /// The batch's shader has no `noesis.wgsl` variant.
    UnsupportedShader,
}

/// `(image, image sampler, shadow)`; a `None` shadow binds the dummy texture.
type ImageBindGroupKey = (
    TextureHandle,
    SamplerState,
    Option<(TextureHandle, SamplerState)>,
);

#[allow(dead_code)] // width/height are never read
struct GpuTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    noesis_format: TextureFormat,
    width: u32,
    height: u32,
    num_levels: u32,
}

struct GpuStencil {
    #[allow(dead_code)] // keeps the allocation alive behind `view`
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// The resolve texture is a separate `textures` entry under `resolve_handle`,
/// which Noesis releases through its own `drop_texture`.
struct GpuRenderTarget {
    // Same texture as the resolve (sample_count is always 1).
    color_view: wgpu::TextureView,
    #[allow(dead_code)] // records the resolve entry; never read
    resolve_handle: TextureHandle,
    stencil: Option<(wgpu::Texture, wgpu::TextureView)>,
    width: u32,
    height: u32,
}

impl WgpuRenderDevice {
    /// Creates a device that renders with `device` and submits to `queue`.
    /// Call [`Self::set_onscreen_target`] before the first onscreen render.
    ///
    /// Every pipeline targets `Rgba8Unorm`, so onscreen views must use that
    /// format.
    #[must_use]
    #[allow(clippy::too_many_lines)] // wgpu setup is linear and hard to split usefully
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let vertex_stream = GeometryStream::new(
            &device,
            "noesis_wgpu vertex stream",
            DYNAMIC_VB_SIZE,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        let index_stream = GeometryStream::new(
            &device,
            "noesis_wgpu index stream",
            DYNAMIC_IB_SIZE,
            wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        );

        let uniforms = UniformRings::new(&device);

        // Group(2): pattern texture + pattern sampler. Shared layout for
        // both PAINT_PATTERN draws and the dummy used by non-pattern draws.
        let pattern_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("noesis_wgpu pattern layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        // Group(3): image texture+sampler (bindings 0/1) plus the shadow
        // texture+sampler (bindings 2/3) co-bound for SHADOW / BLUR. Separate
        // group from pattern so existing pipelines keep their group(2)-only
        // setup; OPACITY-class shaders layer the offscreen image on top and
        // leave the shadow slots dummy.
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let image_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("noesis_wgpu image+shadow layout"),
                entries: &[
                    texture_entry(0),
                    sampler_entry(1),
                    texture_entry(2),
                    sampler_entry(3),
                ],
            });

        // Dummy 1x1 white texture + default sampler for non-pattern draws.
        // The pipeline layout always has group(2) so every draw must bind
        // something; the shader just doesn't sample it.
        let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noesis_wgpu dummy pattern"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &dummy_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0xFF_u8; 4],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let dummy_view = dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("noesis_wgpu dummy sampler"),
            ..Default::default()
        });
        let dummy_pattern_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_wgpu dummy pattern bg"),
            layout: &pattern_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&dummy_sampler),
                },
            ],
        });
        let dummy_image_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_wgpu dummy image bg"),
            layout: &image_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&dummy_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&dummy_sampler),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("noesis_wgpu pipeline layout"),
            bind_group_layouts: &[
                Some(&uniforms.vs_layout),
                Some(&uniforms.ps_layout),
                Some(&pattern_bind_group_layout),
                Some(&image_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let pipelines = PipelineCache::new(device.clone(), pipeline_layout, RT_COLOR_FORMAT);

        Self {
            device,
            queue,
            vertex_stream,
            index_stream,
            uniforms,
            pattern_bind_group_layout,
            dummy_pattern_bg,
            samplers: HashMap::new(),
            pattern_bind_groups: HashMap::new(),
            image_bind_group_layout,
            dummy_image_bg,
            image_bind_groups: HashMap::new(),
            dummy_texture,
            dummy_view,
            dummy_sampler,
            pipelines,
            warned_shaders: HashSet::new(),
            textures: HashMap::new(),
            render_targets: HashMap::new(),
            target_view: None,
            onscreen_stencil: None,
            onscreen_stencil_cleared: false,
            current_rt_stencil_cleared: false,
            phase: FramePhase::Idle,
            encoder: None,
            current_rt: None,
            current_tile: None,
            next_handle: 1,
        }
    }

    /// Caches the group(2) bind group for `(handle, state)`; `drop_texture`
    /// evicts it. `false` for an unknown handle.
    fn ensure_pattern_bind_group(&mut self, handle: TextureHandle, state: SamplerState) -> bool {
        if self.pattern_bind_groups.contains_key(&(handle, state)) {
            return true;
        }
        let Some(view) = self.textures.get(&handle).map(|t| &t.view) else {
            return false;
        };
        let sampler = self
            .samplers
            .entry(state)
            .or_insert_with(|| build_sampler(&self.device, state));
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_wgpu pattern bg"),
            layout: &self.pattern_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        self.pattern_bind_groups.insert((handle, state), bg);
        true
    }

    /// Caches the group(3) bind group for `key`; a `None` shadow binds the
    /// dummy at 2/3. `false` for an unknown handle.
    fn ensure_image_bind_group(&mut self, key: ImageBindGroupKey) -> bool {
        let (image_handle, image_state, shadow) = key;
        if self.image_bind_groups.contains_key(&key) {
            return true;
        }
        let image = (image_handle, image_state);
        self.samplers
            .entry(image.1)
            .or_insert_with(|| build_sampler(&self.device, image.1));
        if let Some((_, sstate)) = shadow {
            self.samplers
                .entry(sstate)
                .or_insert_with(|| build_sampler(&self.device, sstate));
        }

        let Some(image_view) = self.textures.get(&image.0).map(|t| &t.view) else {
            return false;
        };
        let image_sampler = &self.samplers[&image.1];
        let (shadow_view, shadow_sampler) = match shadow {
            Some((handle, sstate)) => {
                let Some(view) = self.textures.get(&handle).map(|t| &t.view) else {
                    return false;
                };
                (view, &self.samplers[&sstate])
            }
            None => (&self.dummy_view, &self.dummy_sampler),
        };
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_wgpu image+shadow bg"),
            layout: &self.image_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(image_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(image_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(shadow_sampler),
                },
            ],
        });
        self.image_bind_groups.insert(key, bg);
        true
    }

    /// Sets the view that onscreen draws render into. Call it before an
    /// onscreen phase whenever the target changes, such as after a resize; a
    /// host with one fixed target calls it once.
    ///
    /// `view` must be an `Rgba8Unorm` texture of `width` x `height` pixels
    /// (the format isn't checked). The device keeps a matching stencil buffer
    /// for onscreen clipping and reallocates it only when the size changes.
    ///
    /// Call it between `end_*_render` and the next `begin_*_render`. Called
    /// inside a phase, it logs a warning and the draws that follow use the new
    /// target.
    pub fn set_onscreen_target(&mut self, view: wgpu::TextureView, width: u32, height: u32) {
        if self.phase != FramePhase::Idle {
            warn!(
                "set_onscreen_target called inside the {:?} phase",
                self.phase
            );
        }
        let need_alloc = self
            .onscreen_stencil
            .as_ref()
            .is_none_or(|s| s.width != width || s.height != height);
        if need_alloc {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("noesis_wgpu onscreen stencil"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: RT_STENCIL_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.onscreen_stencil = Some(GpuStencil {
                texture,
                view,
                width,
                height,
            });
            self.onscreen_stencil_cleared = false;
        }
        self.target_view = Some(view);
    }

    /// The `wgpu::Texture` behind `handle`, including a render target's
    /// resolve texture. `None` once Noesis has dropped it or for an unknown
    /// handle.
    #[must_use]
    pub fn texture(&self, handle: TextureHandle) -> Option<&wgpu::Texture> {
        self.textures.get(&handle).map(|t| &t.texture)
    }

    /// Pixel size the render target `handle` was created with. `None` once
    /// it has been dropped.
    #[must_use]
    pub fn render_target_size(&self, handle: RenderTargetHandle) -> Option<(u32, u32)> {
        self.render_targets
            .get(&handle)
            .map(|rt| (rt.width, rt.height))
    }

    fn alloc_handle(&mut self) -> NonZeroU64 {
        let h = self.next_handle;
        self.next_handle += 1;
        NonZeroU64::new(h).expect("alloc_handle starts at 1")
    }

    /// Uploads `batch`'s uniforms and returns the `(vs, ps0, ps1)` dynamic
    /// offsets.
    fn upload_uniforms(&mut self, batch: &Batch) -> (u32, u32, u32) {
        let cbuf0 = batch.vertex_uniforms[0].as_bytes();
        let cbuf1 = batch.vertex_uniforms[1].as_bytes();
        let mut vs_buf = [0u8; VS_UNIFORM_SIZE as usize];
        let cbuf0_len = cbuf0.len().min(VS_GLYPH_SIZE_OFFSET);
        vs_buf[..cbuf0_len].copy_from_slice(&cbuf0[..cbuf0_len]);
        let cbuf1_len = cbuf1
            .len()
            .min(VS_UNIFORM_SIZE as usize - VS_GLYPH_SIZE_OFFSET);
        vs_buf[VS_GLYPH_SIZE_OFFSET..VS_GLYPH_SIZE_OFFSET + cbuf1_len]
            .copy_from_slice(&cbuf1[..cbuf1_len]);
        self.uniforms.write(
            &self.device,
            &self.queue,
            &vs_buf,
            batch.pixel_uniforms[0].as_bytes(),
            batch.pixel_uniforms[1].as_bytes(),
        )
    }
}

const fn round_up_to_4(n: usize) -> usize {
    (n + 3) & !3
}

const fn align_up_u64(n: u64, align: u64) -> u64 {
    (n + align - 1) & !(align - 1)
}

/// One uniform buffer split into slots, one per draw. The bind group covers
/// one slot at offset 0 and the dynamic offset selects the slot.
/// `write_buffer` calls all land before the phase's submit, so a shared slot
/// would leave every draw reading the last batch's values. Reset at each
/// `begin_*_render`.
struct UniformRing {
    buffer: wgpu::Buffer,
    label: &'static str,
    /// Bytes the shader actually reads from each slot.
    struct_size: u64,
    /// Distance between slot starts: `struct_size` rounded up to
    /// `min_uniform_buffer_offset_alignment`.
    slot_stride: u64,
    slot_capacity: u32,
    next_slot: u32,
    /// Zero-padded copy of the payload so a short payload still fills the slot.
    scratch: Vec<u8>,
}

impl UniformRing {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        struct_size: u64,
        slot_capacity: u32,
    ) -> Self {
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let slot_stride = align_up_u64(struct_size, alignment);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: slot_stride * u64::from(slot_capacity),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            label,
            struct_size,
            slot_stride,
            slot_capacity,
            next_slot: 0,
            scratch: vec![0u8; struct_size as usize],
        }
    }

    fn is_full(&self) -> bool {
        self.next_slot == self.slot_capacity
    }

    /// Replaces the buffer with an empty one of twice the slots. Draws already
    /// recorded keep the old buffer alive through the encoder, so nothing is
    /// copied.
    fn grow(&mut self, device: &wgpu::Device) {
        *self = Self::new(device, self.label, self.struct_size, self.slot_capacity * 2);
    }

    fn reset(&mut self) {
        self.next_slot = 0;
    }

    /// Uploads `bytes` (truncated or zero-padded to `struct_size`) to the next
    /// slot and returns its dynamic offset. The caller grows a full ring first.
    fn write(&mut self, queue: &wgpu::Queue, bytes: &[u8]) -> u32 {
        let slot = self.next_slot;
        self.next_slot += 1;
        let offset = u64::from(slot) * self.slot_stride;

        let len = bytes.len().min(self.struct_size as usize);
        self.scratch[..len].copy_from_slice(&bytes[..len]);
        self.scratch[len..].fill(0);
        queue.write_buffer(&self.buffer, offset, &self.scratch);

        u32::try_from(offset).expect("uniform ring offset overflowed u32")
    }
}

/// The per-draw uniform rings and the bind groups that window them: group(0)
/// holds the vertex uniforms, group(1) `cbuffer0_ps` at binding 0 and
/// `cbuffer1_ps` at binding 1. The three rings fill in step, one slot per draw,
/// and grow together.
struct UniformRings {
    vs: UniformRing,
    ps0: UniformRing,
    ps1: UniformRing,
    vs_layout: wgpu::BindGroupLayout,
    ps_layout: wgpu::BindGroupLayout,
    vs_bind_group: wgpu::BindGroup,
    ps_bind_group: wgpu::BindGroup,
}

impl UniformRings {
    fn new(device: &wgpu::Device) -> Self {
        let vs = UniformRing::new(
            device,
            "noesis_wgpu vs_uniforms ring (mat4 projection)",
            VS_UNIFORM_SIZE,
            UNIFORM_RING_SLOTS,
        );
        let ps0 = UniformRing::new(
            device,
            "noesis_wgpu ps_uniforms0 ring (cbuffer0_ps[8])",
            PS_UNIFORM0_SIZE,
            UNIFORM_RING_SLOTS,
        );
        let ps1 = UniformRing::new(
            device,
            "noesis_wgpu ps_uniforms1 ring (cbuffer1_ps[8])",
            PS_UNIFORM1_SIZE,
            UNIFORM_RING_SLOTS,
        );

        let uniform_entry = |binding, visibility, size| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(size),
            },
            count: None,
        };
        let vs_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("noesis_wgpu vs_uniforms layout"),
            entries: &[uniform_entry(
                0,
                wgpu::ShaderStages::VERTEX,
                VS_UNIFORM_SIZE,
            )],
        });
        // cbuffer1_ps: only SHADOW / BLUR read it, but the shared layout always
        // declares it so every pipeline matches.
        let ps_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("noesis_wgpu ps_uniforms layout"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT, PS_UNIFORM0_SIZE),
                uniform_entry(1, wgpu::ShaderStages::FRAGMENT, PS_UNIFORM1_SIZE),
            ],
        });

        let vs_bind_group = vs_bind_group(device, &vs_layout, &vs);
        let ps_bind_group = ps_bind_group(device, &ps_layout, &ps0, &ps1);
        Self {
            vs,
            ps0,
            ps1,
            vs_layout,
            ps_layout,
            vs_bind_group,
            ps_bind_group,
        }
    }

    fn reset(&mut self) {
        self.vs.reset();
        self.ps0.reset();
        self.ps1.reset();
    }

    /// Uploads one draw's uniforms and returns the `(vs, ps0, ps1)` dynamic
    /// offsets, doubling the rings first when they are full.
    fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vs: &[u8],
        ps0: &[u8],
        ps1: &[u8],
    ) -> (u32, u32, u32) {
        if self.vs.is_full() {
            self.vs.grow(device);
            self.ps0.grow(device);
            self.ps1.grow(device);
            self.vs_bind_group = vs_bind_group(device, &self.vs_layout, &self.vs);
            self.ps_bind_group = ps_bind_group(device, &self.ps_layout, &self.ps0, &self.ps1);
        }
        (
            self.vs.write(queue, vs),
            self.ps0.write(queue, ps0),
            self.ps1.write(queue, ps1),
        )
    }
}

/// A bind group exposing one struct-sized window into each ring buffer. The
/// dynamic offset passed to `set_bind_group` slides it to the draw's slot.
fn ring_binding(ring: &UniformRing) -> wgpu::BindingResource<'_> {
    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
        buffer: &ring.buffer,
        offset: 0,
        size: NonZeroU64::new(ring.struct_size),
    })
}

fn vs_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    vs: &UniformRing,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("noesis_wgpu vs_uniforms"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: ring_binding(vs),
        }],
    })
}

fn ps_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    ps0: &UniformRing,
    ps1: &UniformRing,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("noesis_wgpu ps_uniforms"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: ring_binding(ps0),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: ring_binding(ps1),
            },
        ],
    })
}

/// Growable vertex or index buffer for one render phase. Noesis maps and
/// unmaps it several times per phase; each `unmap` appends at `cursor` rather
/// than offset 0, for the same reason as [`UniformRing`]. Batch offsets are
/// relative to the latest segment, so `draw_batch` adds `segment_base`.
struct GeometryStream {
    buffer: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
    /// Handed to Noesis by `map`, uploaded at `cursor` by `unmap`.
    staging: Vec<u8>,
    /// Bytes claimed by the in-flight `map`; `None` outside a map/unmap pair.
    mapped_bytes: Option<u32>,
    cursor: u64,
    /// Start of the segment the latest `unmap` wrote.
    segment_base: u64,
}

impl GeometryStream {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        size: u64,
        usage: wgpu::BufferUsages,
    ) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            label,
            usage,
            staging: vec![0u8; size as usize],
            mapped_bytes: None,
            cursor: 0,
            segment_base: 0,
        }
    }

    fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    fn segment_base(&self) -> u64 {
        self.segment_base
    }

    fn reset(&mut self) {
        self.cursor = 0;
    }

    fn map(&mut self, bytes: u32) -> &mut [u8] {
        if self.mapped_bytes.is_some() {
            warn!(
                "{}: map without unmap; the earlier map is discarded",
                self.label
            );
        }
        let len = bytes as usize;
        if len > self.staging.len() {
            self.staging.resize(len, 0);
        }
        self.mapped_bytes = Some(bytes);
        &mut self.staging[..len]
    }

    fn unmap(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let Some(bytes) = self.mapped_bytes.take() else {
            warn!("{}: unmap without map", self.label);
            return;
        };
        let padded = round_up_to_4(bytes as usize) as u64;
        // No copy on growth: earlier draws hold the old buffer through the
        // encoder and never read past their own segment.
        if self.cursor + padded > self.buffer.size() {
            let new_size = (self.cursor + padded).max(self.buffer.size() * 2);
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: new_size,
                usage: self.usage,
                mapped_at_creation: false,
            });
        }
        self.segment_base = self.cursor;
        queue.write_buffer(&self.buffer, self.cursor, &self.staging[..padded as usize]);
        self.cursor += padded;
    }
}

const fn wgpu_format_for(format: TextureFormat) -> wgpu::TextureFormat {
    match format {
        // No wgpu Rgbx8; `has_alpha: false` on the binding tells Noesis.
        TextureFormat::Rgba8 | TextureFormat::Rgbx8 => wgpu::TextureFormat::Rgba8Unorm,
        TextureFormat::R8 => wgpu::TextureFormat::R8Unorm,
        // Non-exhaustive SDK enum. Raw conversions here default instead of
        // panicking: the FFI trampoline would catch it but leave the frame
        // half-mutated.
        _ => wgpu::TextureFormat::Rgba8Unorm,
    }
}

const fn bytes_per_pixel(format: TextureFormat) -> u32 {
    match format {
        TextureFormat::Rgba8 | TextureFormat::Rgbx8 => 4,
        TextureFormat::R8 => 1,
        // See `wgpu_format_for`.
        _ => 4,
    }
}

/// True when `shader` reads the group(2) paint texture. Must list exactly the
/// shaders [`defines_for_shader`] gives `HAS_PAINT_TEXTURE`, as must
/// [`batch_paint_texture`].
///
/// [`defines_for_shader`]: crate::shader_defines::defines_for_shader
const fn shader_uses_paint_texture(shader: u8) -> bool {
    shader == Shader::PATH_PATTERN.0
        || shader == Shader::PATH_AA_PATTERN.0
        || shader == Shader::PATH_PATTERN_CLAMP.0
        || shader == Shader::PATH_AA_PATTERN_CLAMP.0
        || shader == Shader::PATH_PATTERN_REPEAT.0
        || shader == Shader::PATH_AA_PATTERN_REPEAT.0
        || shader == Shader::PATH_PATTERN_MIRROR_U.0
        || shader == Shader::PATH_AA_PATTERN_MIRROR_U.0
        || shader == Shader::PATH_PATTERN_MIRROR_V.0
        || shader == Shader::PATH_AA_PATTERN_MIRROR_V.0
        || shader == Shader::PATH_PATTERN_MIRROR.0
        || shader == Shader::PATH_AA_PATTERN_MIRROR.0
        || shader == Shader::PATH_LINEAR.0
        || shader == Shader::PATH_AA_LINEAR.0
        || shader == Shader::PATH_RADIAL.0
        || shader == Shader::PATH_AA_RADIAL.0
        || shader == Shader::SDF_SOLID.0
        || shader == Shader::SDF_LCD_SOLID.0
        || shader == Shader::OPACITY_LINEAR.0
        || shader == Shader::OPACITY_RADIAL.0
        || shader == Shader::OPACITY_PATTERN.0
        || shader == Shader::OPACITY_PATTERN_CLAMP.0
        || shader == Shader::OPACITY_PATTERN_REPEAT.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_U.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_V.0
        || shader == Shader::OPACITY_PATTERN_MIRROR.0
        // DOWNSAMPLE/UPSAMPLE read the source image at group(2) `pattern`.
        || shader == Shader::DOWNSAMPLE.0
        || shader == Shader::UPSAMPLE.0
}

/// The batch slot (pattern, ramps, or glyphs) bound at group(2) for
/// `batch`'s shader. `None` for shaders without a paint texture, or when
/// Noesis left the slot empty.
fn batch_paint_texture(
    batch: &Batch,
    textures: BatchTextures,
) -> Option<(TextureHandle, SamplerState)> {
    match batch.shader.0 {
        s if s == Shader::PATH_PATTERN.0
            || s == Shader::PATH_AA_PATTERN.0
            || s == Shader::PATH_PATTERN_CLAMP.0
            || s == Shader::PATH_AA_PATTERN_CLAMP.0
            || s == Shader::PATH_PATTERN_REPEAT.0
            || s == Shader::PATH_AA_PATTERN_REPEAT.0
            || s == Shader::PATH_PATTERN_MIRROR_U.0
            || s == Shader::PATH_AA_PATTERN_MIRROR_U.0
            || s == Shader::PATH_PATTERN_MIRROR_V.0
            || s == Shader::PATH_AA_PATTERN_MIRROR_V.0
            || s == Shader::PATH_PATTERN_MIRROR.0
            || s == Shader::PATH_AA_PATTERN_MIRROR.0
            || s == Shader::OPACITY_PATTERN.0
            || s == Shader::OPACITY_PATTERN_CLAMP.0
            || s == Shader::OPACITY_PATTERN_REPEAT.0
            || s == Shader::OPACITY_PATTERN_MIRROR_U.0
            || s == Shader::OPACITY_PATTERN_MIRROR_V.0
            || s == Shader::OPACITY_PATTERN_MIRROR.0 =>
        {
            textures.pattern.map(|h| (h, batch.pattern_sampler))
        }
        s if s == Shader::PATH_LINEAR.0
            || s == Shader::PATH_AA_LINEAR.0
            || s == Shader::PATH_RADIAL.0
            || s == Shader::PATH_AA_RADIAL.0
            || s == Shader::OPACITY_LINEAR.0
            || s == Shader::OPACITY_RADIAL.0 =>
        {
            textures.ramps.map(|h| (h, batch.ramps_sampler))
        }
        s if s == Shader::SDF_SOLID.0 || s == Shader::SDF_LCD_SOLID.0 => {
            textures.glyphs.map(|h| (h, batch.glyphs_sampler))
        }
        s if s == Shader::DOWNSAMPLE.0 || s == Shader::UPSAMPLE.0 => {
            textures.pattern.map(|h| (h, batch.pattern_sampler))
        }
        _ => None,
    }
}

/// True when `shader` reads the group(3) `image` texture. Must match
/// `HAS_IMAGE_TEXTURE` in `defines_for_shader`.
const fn shader_uses_image_texture(shader: u8) -> bool {
    shader == Shader::OPACITY_SOLID.0
        || shader == Shader::OPACITY_LINEAR.0
        || shader == Shader::OPACITY_RADIAL.0
        || shader == Shader::OPACITY_PATTERN.0
        || shader == Shader::OPACITY_PATTERN_CLAMP.0
        || shader == Shader::OPACITY_PATTERN_REPEAT.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_U.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_V.0
        || shader == Shader::OPACITY_PATTERN_MIRROR.0
        || shader == Shader::UPSAMPLE.0
        || shader == Shader::SHADOW.0
        || shader == Shader::BLUR.0
}

/// True when `shader` also reads the group(3) `shadow` texture.
const fn shader_uses_shadow_texture(shader: u8) -> bool {
    shader == Shader::SHADOW.0 || shader == Shader::BLUR.0
}

fn wgpu_wrap_mode(wrap_raw: u8) -> wgpu::AddressMode {
    // ClampToZero (1) needs a border color, which downlevel wgpu lacks;
    // CLAMP_PATTERN masks out-of-rect samples in the shader anyway. The
    // caller applies one mode to every axis, so MirrorU/MirrorV mirror both.
    match wrap_raw {
        0 | 1 => wgpu::AddressMode::ClampToEdge,
        2 => wgpu::AddressMode::Repeat,
        3..=5 => wgpu::AddressMode::MirrorRepeat,
        // See `wgpu_format_for`.
        other => {
            warn_once!("unknown Noesis WrapMode raw value {other}; using ClampToEdge");
            wgpu::AddressMode::ClampToEdge
        }
    }
}

fn build_sampler(device: &wgpu::Device, state: SamplerState) -> wgpu::Sampler {
    let wrap = wgpu_wrap_mode(state.wrap_mode_raw());
    let filter = match state.minmag_filter_raw() {
        0 => wgpu::FilterMode::Nearest,
        _ => wgpu::FilterMode::Linear,
    };
    let mipmap_filter = match state.mip_filter_raw() {
        0 | 1 => wgpu::MipmapFilterMode::Nearest,
        _ => wgpu::MipmapFilterMode::Linear,
    };
    let lod_max = match state.mip_filter_raw() {
        0 => 0.25, // mips disabled: mip 0 only
        _ => 32.0,
    };

    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("noesis_wgpu sampler"),
        address_mode_u: wrap,
        address_mode_v: wrap,
        address_mode_w: wrap,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter,
        lod_min_clamp: 0.0,
        lod_max_clamp: lod_max,
        compare: None,
        anisotropy_clamp: 1,
        border_color: None,
    })
}

impl WgpuRenderDevice {
    /// The capabilities Noesis queries once during setup: no linear
    /// rendering, no subpixel text, zero-to-one depth range, and clip space
    /// with y up.
    pub fn caps(&self) -> DeviceCaps {
        DeviceCaps {
            center_pixel_offset: 0.0,
            linear_rendering: false,
            // SDF_LCD_SOLID exists, but turning this on makes Noesis emit the
            // whole SDF_LCD_* family: most variants are unported, all need
            // DUAL_SOURCE_BLENDING (not in downlevel defaults), and the SDK has
            // no LCD reference to validate against. tests/wgpu_suite/wgpu_sdf_lcd.rs
            // drives the solid variant directly.
            subpixel_rendering: false,
            depth_range_zero_to_one: true,
            clip_space_y_inverted: false,
        }
    }

    /// Creates a texture and uploads its initial mip levels, if `desc` has
    /// any. `Rgbx8` is stored as `Rgba8Unorm` and reported without alpha.
    /// Extra data levels are ignored, with a warning.
    pub fn create_texture(&mut self, desc: TextureDesc<'_>) -> TextureBinding {
        let handle = TextureHandle(self.alloc_handle());
        let wgpu_format = wgpu_format_for(desc.format);

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(desc.label),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: desc.num_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        if let Some(levels) = desc.data {
            if levels.len() != desc.num_levels as usize {
                warn!(
                    "create_texture '{}': {} data levels for {} mip levels",
                    desc.label,
                    levels.len(),
                    desc.num_levels,
                );
            }
            let bpp = bytes_per_pixel(desc.format);
            for (level, bytes) in levels.iter().take(desc.num_levels as usize).enumerate() {
                let level_u32 = level as u32;
                let w = (desc.width >> level_u32).max(1);
                let h = (desc.height >> level_u32).max(1);
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: level_u32,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * bpp),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let has_alpha = !matches!(desc.format, TextureFormat::Rgbx8);
        self.textures.insert(
            handle,
            GpuTexture {
                texture,
                view,
                noesis_format: desc.format,
                width: desc.width,
                height: desc.height,
                num_levels: desc.num_levels,
            },
        );

        TextureBinding {
            handle,
            width: desc.width,
            height: desc.height,
            has_mipmaps: desc.num_levels > 1,
            inverted: false,
            has_alpha,
        }
    }

    /// Writes `data` into `rect` of mip `level`. `data` is tightly packed,
    /// with no row padding.
    /// An unknown handle or a level the texture doesn't have logs a warning
    /// and writes nothing.
    pub fn update_texture(
        &mut self,
        handle: TextureHandle,
        level: u32,
        rect: TextureRect,
        data: &[u8],
    ) {
        let Some(tex) = self.textures.get(&handle) else {
            warn!("update_texture: unknown texture {handle:?}");
            return;
        };
        if level >= tex.num_levels {
            warn!(
                "update_texture: level {level} of a texture with {} levels",
                tex.num_levels
            );
            return;
        }
        let bpp = bytes_per_pixel(tex.noesis_format);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: level,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(rect.width * bpp),
                rows_per_image: Some(rect.height),
            },
            wgpu::Extent3d {
                width: rect.width,
                height: rect.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Ends a block of [`update_texture`](Self::update_texture) calls. A no-op:
    /// wgpu orders the uploads before later passes on its own.
    pub fn end_updating_textures(&mut self, _textures: &[TextureHandle]) {}

    /// Releases a texture and the bind groups that sample it.
    pub fn drop_texture(&mut self, handle: TextureHandle) {
        self.textures.remove(&handle);
        self.pattern_bind_groups.retain(|(h, _), _| *h != handle);
        self.image_bind_groups.retain(|(img, _, shadow), _| {
            *img != handle && shadow.is_none_or(|(s, _)| s != handle)
        });
    }

    /// Creates an `Rgba8Unorm` render target, with a `Stencil8` buffer when
    /// `desc.needs_stencil` is set. Its color texture doubles as the resolve
    /// texture Noesis samples.
    ///
    /// MSAA isn't supported: any `desc.sample_count` gets a single-sampled
    /// target, with a one-time warning.
    pub fn create_render_target(&mut self, desc: RenderTargetDesc<'_>) -> RenderTargetBinding {
        if desc.sample_count != 1 {
            warn_once!(
                "create_render_target: MSAA is unsupported; creating single-sampled targets \
                 instead of {} samples",
                desc.sample_count,
            );
        }

        let rt_handle = RenderTargetHandle(self.alloc_handle());
        let resolve_handle = TextureHandle(self.alloc_handle());

        let color_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(desc.label),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: RT_COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let resolve_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let stencil = desc.needs_stencil.then(|| {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("{} stencil", desc.label)),
                size: wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: RT_STENCIL_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            (tex, view)
        });

        self.textures.insert(
            resolve_handle,
            GpuTexture {
                texture: color_texture,
                view: resolve_view,
                noesis_format: TextureFormat::Rgba8,
                width: desc.width,
                height: desc.height,
                num_levels: 1,
            },
        );
        self.render_targets.insert(
            rt_handle,
            GpuRenderTarget {
                color_view,
                resolve_handle,
                stencil,
                width: desc.width,
                height: desc.height,
            },
        );

        RenderTargetBinding {
            handle: rt_handle,
            resolve_texture: TextureBinding {
                handle: resolve_handle,
                width: desc.width,
                height: desc.height,
                has_mipmaps: false,
                inverted: false,
                has_alpha: true,
            },
        }
    }

    /// Creates a render target with the size and stencil of `src`. Noesis
    /// allows the two to share transient buffers; this device doesn't.
    /// An unknown `src` logs a warning and gets a 1x1 target.
    pub fn clone_render_target(
        &mut self,
        label: &str,
        src: RenderTargetHandle,
    ) -> RenderTargetBinding {
        // Noesis allows the clone to share src's transient buffers; a fresh RT
        // of the same size and stencil is a valid, unshared implementation.
        let (width, height, needs_stencil) = match self.render_targets.get(&src) {
            Some(src_rt) => (src_rt.width, src_rt.height, src_rt.stencil.is_some()),
            None => {
                warn!(
                    "clone_render_target '{label}': unknown source {src:?}; creating a 1x1 target"
                );
                (1, 1, false)
            }
        };
        self.create_render_target(RenderTargetDesc {
            label,
            width,
            height,
            sample_count: 1,
            needs_stencil,
        })
    }

    /// Releases a render target. Noesis releases its resolve texture
    /// separately through [`drop_texture`](Self::drop_texture).
    pub fn drop_render_target(&mut self, handle: RenderTargetHandle) {
        // The resolve texture is released separately through `drop_texture`.
        self.render_targets.remove(&handle);
    }

    /// Starts the offscreen phase and opens its command encoder.
    pub fn begin_offscreen_render(&mut self) {
        // A panic caught by the FFI trampoline in an earlier callback can leave
        // the phase open. Asserting would re-trip every frame; re-sync instead
        // (replacing `encoder` drops the stale one).
        if self.phase != FramePhase::Idle {
            warn!(
                "begin_offscreen_render found phase {:?} (previous frame aborted \
                 mid-callback); resetting",
                self.phase,
            );
        }
        self.uniforms.reset();
        self.vertex_stream.reset();
        self.index_stream.reset();
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("noesis_wgpu offscreen frame"),
                }),
        );
        self.phase = FramePhase::Offscreen;
        self.current_rt = None;
        self.current_tile = None;
    }

    /// Ends the offscreen phase and submits its command encoder.
    pub fn end_offscreen_render(&mut self) {
        // Warn, don't assert: see `begin_offscreen_render`.
        if self.phase != FramePhase::Offscreen {
            warn!(
                "end_offscreen_render found phase {:?}, expected Offscreen",
                self.phase,
            );
        }
        if let Some(encoder) = self.encoder.take() {
            self.queue.submit(Some(encoder.finish()));
        }
        self.phase = FramePhase::Idle;
        self.current_rt = None;
        self.current_tile = None;
    }

    /// Starts the onscreen phase and opens its command encoder. Draws go to
    /// the view set by [`set_onscreen_target`](Self::set_onscreen_target).
    pub fn begin_onscreen_render(&mut self) {
        // See `begin_offscreen_render`.
        if self.phase != FramePhase::Idle {
            warn!(
                "begin_onscreen_render found phase {:?} (previous frame aborted \
                 mid-callback); resetting",
                self.phase,
            );
        }
        self.uniforms.reset();
        self.vertex_stream.reset();
        self.index_stream.reset();
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("noesis_wgpu onscreen frame"),
                }),
        );
        self.phase = FramePhase::Onscreen;
        self.onscreen_stencil_cleared = false;
    }

    /// Ends the onscreen phase and submits its command encoder.
    pub fn end_onscreen_render(&mut self) {
        // See `begin_offscreen_render`.
        if self.phase != FramePhase::Onscreen {
            warn!(
                "end_onscreen_render found phase {:?}, expected Onscreen",
                self.phase,
            );
        }
        if let Some(encoder) = self.encoder.take() {
            self.queue.submit(Some(encoder.finish()));
        }
        self.phase = FramePhase::Idle;
    }

    /// Makes `handle` the target of the offscreen draws that follow. Its
    /// stencil is cleared before the next draw.
    /// An unknown handle logs a warning, and offscreen draws skip until the
    /// next call.
    pub fn set_render_target(&mut self, handle: RenderTargetHandle) {
        if self.phase != FramePhase::Offscreen {
            warn!("set_render_target called in the {:?} phase", self.phase);
        }
        if self.render_targets.contains_key(&handle) {
            self.current_rt = Some(handle);
        } else {
            warn!(
                "set_render_target: unknown render target {handle:?}; draws skip until the next one"
            );
            self.current_rt = None;
        }
        self.current_tile = None;
        // The protocol discards the RT's contents here, so re-clear its stencil.
        self.current_rt_stencil_cleared = false;
    }

    /// Limits the draws that follow to `tile`, given with a bottom-left
    /// origin, until [`end_tile`](Self::end_tile).
    pub fn begin_tile(&mut self, handle: RenderTargetHandle, tile: Tile) {
        if self.current_rt != Some(handle) {
            warn!("begin_tile for {handle:?}, which isn't the current render target");
        }
        self.current_tile = Some(tile);
    }

    /// Ends the tile started by [`begin_tile`](Self::begin_tile).
    pub fn end_tile(&mut self, handle: RenderTargetHandle) {
        if self.current_rt != Some(handle) {
            warn!("end_tile for {handle:?}, which isn't the current render target");
        }
        self.current_tile = None;
    }

    /// A no-op: render targets are single-sampled, so the color texture is
    /// already the resolve texture.
    pub fn resolve_render_target(&mut self, _handle: RenderTargetHandle, _tiles: &[Tile]) {
        // sample_count is always 1: the color attachment is the resolve texture.
    }

    /// Returns `bytes` bytes of vertex storage for Noesis to fill. The data
    /// reaches the GPU at [`unmap_vertices`](Self::unmap_vertices).
    pub fn map_vertices(&mut self, bytes: u32) -> &mut [u8] {
        self.vertex_stream.map(bytes)
    }
    /// Uploads the vertices written since [`map_vertices`](Self::map_vertices).
    pub fn unmap_vertices(&mut self) {
        self.vertex_stream.unmap(&self.device, &self.queue);
    }
    /// Like [`map_vertices`](Self::map_vertices), for 16-bit indices.
    pub fn map_indices(&mut self, bytes: u32) -> &mut [u8] {
        self.index_stream.map(bytes)
    }
    /// Uploads the indices written since [`map_indices`](Self::map_indices).
    pub fn unmap_indices(&mut self) {
        self.index_stream.unmap(&self.device, &self.queue);
    }

    /// Draws `batch`, sampling the textures in `textures`.
    ///
    /// The batch's own texture pointers are ignored; its samplers, uniforms,
    /// shader and render state are used as given. A draw goes to the current
    /// render target and tile in the offscreen phase, and to the onscreen
    /// target in the onscreen phase. Each draw records its own render pass.
    ///
    /// A batch that can't be drawn is skipped with a warning (logged once per
    /// cause) rather than a panic: one outside a phase or without a target,
    /// one whose shader needs a texture that `textures` leaves empty or names
    /// an unknown handle, one whose geometry runs past the mapped buffers, and
    /// one whose shader this device doesn't implement (custom effects, the
    /// `SDF_*` gradient and pattern paints, and most `SDF_LCD_*` variants).
    pub fn draw_batch_with(&mut self, batch: &Batch, textures: BatchTextures) {
        let _ = self.record_draw(batch, textures);
    }

    #[allow(clippy::too_many_lines)] // one pass of checks, then one render pass
    fn record_draw(&mut self, batch: &Batch, textures: BatchTextures) -> Result<(), SkippedDraw> {
        let (has_stencil, clear_stencil) = match self.phase {
            FramePhase::Onscreen => {
                if self.target_view.is_none() {
                    warn_once!("onscreen draw without set_onscreen_target; skipping");
                    return Err(SkippedDraw::Protocol);
                }
                let has = self.onscreen_stencil.is_some();
                (has, has && !self.onscreen_stencil_cleared)
            }
            FramePhase::Offscreen => {
                let Some(rt) = self.current_rt.and_then(|h| self.render_targets.get(&h)) else {
                    warn_once!("offscreen draw without a live render target; skipping");
                    return Err(SkippedDraw::Protocol);
                };
                let has = rt.stencil.is_some();
                (has, has && !self.current_rt_stencil_cleared)
            }
            FramePhase::Idle => {
                warn_once!("draw outside begin/end_*_render; skipping");
                return Err(SkippedDraw::Protocol);
            }
        };

        let Some(key) = PipelineKey::from_batch(batch, has_stencil) else {
            self.warn_unsupported_shader(batch.shader.0);
            return Err(SkippedDraw::UnsupportedShader);
        };
        if !self.pipelines.ensure(key) {
            self.warn_unsupported_shader(batch.shader.0);
            return Err(SkippedDraw::UnsupportedShader);
        }

        // Build bind groups before borrowing `encoder` mutably.
        let pattern_slot = if shader_uses_paint_texture(batch.shader.0) {
            let Some(slot) = batch_paint_texture(batch, textures) else {
                warn_once!("batch's shader samples a paint texture it wasn't given; skipping");
                return Err(SkippedDraw::Protocol);
            };
            if !self.ensure_pattern_bind_group(slot.0, slot.1) {
                warn_once!("batch names an unknown paint texture; skipping");
                return Err(SkippedDraw::Protocol);
            }
            Some(slot)
        } else {
            None
        };

        let image_slot: Option<ImageBindGroupKey> = if shader_uses_image_texture(batch.shader.0) {
            let Some(image) = textures.image else {
                warn_once!("batch's shader samples an image texture it wasn't given; skipping");
                return Err(SkippedDraw::Protocol);
            };
            let shadow = if shader_uses_shadow_texture(batch.shader.0) {
                let Some(shadow) = textures.shadow else {
                    warn_once!("batch's shader samples a shadow texture it wasn't given; skipping");
                    return Err(SkippedDraw::Protocol);
                };
                Some((shadow, batch.shadow_sampler))
            } else {
                None
            };
            let key = (image, batch.image_sampler, shadow);
            if !self.ensure_image_bind_group(key) {
                warn_once!("batch names an unknown image or shadow texture; skipping");
                return Err(SkippedDraw::Protocol);
            }
            Some(key)
        } else {
            None
        };

        // Batch offsets are relative to the latest unmapped segment.
        let stride = u64::from(SIZE_FOR_FORMAT[key.vertex_format as usize]);
        let vertex_offset = self.vertex_stream.segment_base() + u64::from(batch.vertex_offset);
        let vertex_byte_count = u64::from(batch.num_vertices) * stride;
        let index_byte_offset = self.index_stream.segment_base() + u64::from(batch.start_index) * 2;
        let index_byte_count = u64::from(batch.num_indices) * 2;
        if vertex_offset + vertex_byte_count > self.vertex_stream.buffer().size()
            || index_byte_offset + index_byte_count > self.index_stream.buffer().size()
        {
            warn_once!("batch geometry runs past the mapped buffers; skipping");
            return Err(SkippedDraw::Protocol);
        }

        let (vs_offset, ps_offset, ps1_offset) = self.upload_uniforms(batch);

        let (target_view, scissor, stencil_view) = if self.phase == FramePhase::Offscreen {
            let Some(rt) = self.current_rt.and_then(|h| self.render_targets.get(&h)) else {
                return Err(SkippedDraw::Protocol);
            };
            self.current_rt_stencil_cleared = true;
            // Noesis tiles are bottom-left origin; wgpu scissors top-left.
            let scissor = self.current_tile.map(|t| {
                let y_top = rt.height.saturating_sub(t.y + t.height);
                (t.x, y_top, t.width, t.height)
            });
            let stencil = rt.stencil.as_ref().map(|(_, view)| view);
            (&rt.color_view, scissor, stencil)
        } else {
            self.onscreen_stencil_cleared = true;
            let Some(view) = self.target_view.as_ref() else {
                return Err(SkippedDraw::Protocol);
            };
            let stencil = self.onscreen_stencil.as_ref().map(|s| &s.view);
            (view, None, stencil)
        };

        let pipeline = self.pipelines.get(key);
        let vertex_buffer = self.vertex_stream.buffer();
        let index_buffer = self.index_stream.buffer();
        let vs_bg = &self.uniforms.vs_bind_group;
        let ps_bg = &self.uniforms.ps_bind_group;
        let pattern_bg = match pattern_slot {
            Some(slot) => &self.pattern_bind_groups[&slot],
            None => &self.dummy_pattern_bg,
        };
        let image_bg = match image_slot {
            Some(slot) => &self.image_bind_groups[&slot],
            None => &self.dummy_image_bg,
        };
        let Some(encoder) = self.encoder.as_mut() else {
            warn_once!("draw with no open command encoder; skipping");
            return Err(SkippedDraw::Protocol);
        };

        // Later draws load the stencil so the clip stack accumulates.
        let depth_stencil_attachment =
            stencil_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: None,
                stencil_ops: Some(wgpu::Operations {
                    load: if clear_stencil {
                        wgpu::LoadOp::Clear(0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
            });

        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("noesis_wgpu draw_batch"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rpass.set_pipeline(pipeline);
        if has_stencil {
            rpass.set_stencil_reference(u32::from(batch.stencil_ref));
        }
        rpass.set_bind_group(0, vs_bg, &[vs_offset]);
        rpass.set_bind_group(1, ps_bg, &[ps_offset, ps1_offset]);
        rpass.set_bind_group(2, pattern_bg, &[]);
        rpass.set_bind_group(3, image_bg, &[]);
        if let Some((x, y, w, h)) = scissor {
            rpass.set_scissor_rect(x, y, w, h);
        }
        rpass.set_vertex_buffer(
            0,
            vertex_buffer.slice(vertex_offset..vertex_offset + vertex_byte_count),
        );
        rpass.set_index_buffer(
            index_buffer.slice(index_byte_offset..index_byte_offset + index_byte_count),
            wgpu::IndexFormat::Uint16,
        );
        rpass.draw_indexed(0..batch.num_indices, 0, 0..1);
        Ok(())
    }

    /// Warns once per shader that this device can't draw it.
    fn warn_unsupported_shader(&mut self, shader: u8) {
        if self.warned_shaders.insert(shader) {
            warn!("Noesis shader {shader} isn't implemented; skipping its batches");
        }
    }
}

/// The textures a [`Batch`] samples, as device handles.
///
/// Noesis hands a batch its textures as pointers to its own texture objects.
/// [`WgpuRenderDevice::draw_batch_with`] takes handles instead, so a host that
/// creates Noesis's textures itself (and so knows which handle each pointer
/// stands for) can resolve them and pass them in. A field is `None` when the
/// batch leaves that slot empty. Samplers still come from the batch.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct BatchTextures {
    /// Pattern (brush) texture, sampled with `batch.pattern_sampler`.
    pub pattern: Option<TextureHandle>,
    /// Gradient ramps, sampled with `batch.ramps_sampler`.
    pub ramps: Option<TextureHandle>,
    /// Offscreen image or effect input, sampled with `batch.image_sampler`.
    pub image: Option<TextureHandle>,
    /// SDF glyph atlas, sampled with `batch.glyphs_sampler`.
    pub glyphs: Option<TextureHandle>,
    /// Shadow intermediate, sampled with `batch.shadow_sampler`.
    pub shadow: Option<TextureHandle>,
}

impl BatchTextures {
    /// Reads the handles out of `batch`'s texture pointers through
    /// `noesis_runtime`'s shim. Only valid when every texture in the batch was
    /// created through a device registered with
    /// [`noesis_runtime::render_device::register`].
    #[cfg(feature = "shim")]
    #[must_use]
    pub fn from_batch(batch: &Batch) -> Self {
        Self {
            pattern: batch.pattern_handle(),
            ramps: batch.ramps_handle(),
            image: batch.image_handle(),
            glyphs: batch.glyphs_handle(),
            shadow: batch.shadow_handle(),
        }
    }
}

/// Lets `noesis_runtime`'s shim drive the device: pass it to
/// [`noesis_runtime::render_device::register`]. Each method forwards to the
/// inherent method of the same name, and `draw_batch` reads the batch's
/// textures with [`BatchTextures::from_batch`].
#[cfg(feature = "shim")]
impl RenderDevice for WgpuRenderDevice {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn caps(&self) -> DeviceCaps {
        Self::caps(self)
    }

    fn create_texture(&mut self, desc: TextureDesc<'_>) -> TextureBinding {
        Self::create_texture(self, desc)
    }

    fn update_texture(
        &mut self,
        handle: TextureHandle,
        level: u32,
        rect: TextureRect,
        data: &[u8],
    ) {
        Self::update_texture(self, handle, level, rect, data);
    }

    fn end_updating_textures(&mut self, textures: &[TextureHandle]) {
        Self::end_updating_textures(self, textures);
    }

    fn drop_texture(&mut self, handle: TextureHandle) {
        Self::drop_texture(self, handle);
    }

    fn create_render_target(&mut self, desc: RenderTargetDesc<'_>) -> RenderTargetBinding {
        Self::create_render_target(self, desc)
    }

    fn clone_render_target(&mut self, label: &str, src: RenderTargetHandle) -> RenderTargetBinding {
        Self::clone_render_target(self, label, src)
    }

    fn drop_render_target(&mut self, handle: RenderTargetHandle) {
        Self::drop_render_target(self, handle);
    }

    fn begin_offscreen_render(&mut self) {
        Self::begin_offscreen_render(self);
    }

    fn end_offscreen_render(&mut self) {
        Self::end_offscreen_render(self);
    }

    fn begin_onscreen_render(&mut self) {
        Self::begin_onscreen_render(self);
    }

    fn end_onscreen_render(&mut self) {
        Self::end_onscreen_render(self);
    }

    fn set_render_target(&mut self, handle: RenderTargetHandle) {
        Self::set_render_target(self, handle);
    }

    fn begin_tile(&mut self, handle: RenderTargetHandle, tile: Tile) {
        Self::begin_tile(self, handle, tile);
    }

    fn end_tile(&mut self, handle: RenderTargetHandle) {
        Self::end_tile(self, handle);
    }

    fn resolve_render_target(&mut self, handle: RenderTargetHandle, tiles: &[Tile]) {
        Self::resolve_render_target(self, handle, tiles);
    }

    fn map_vertices(&mut self, bytes: u32) -> &mut [u8] {
        Self::map_vertices(self, bytes)
    }

    fn unmap_vertices(&mut self) {
        Self::unmap_vertices(self);
    }

    fn map_indices(&mut self, bytes: u32) -> &mut [u8] {
        Self::map_indices(self, bytes)
    }

    fn unmap_indices(&mut self) {
        Self::unmap_indices(self);
    }

    fn draw_batch(&mut self, batch: &Batch) {
        self.draw_batch_with(batch, BatchTextures::from_batch(batch));
    }
}
