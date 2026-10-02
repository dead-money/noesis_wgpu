//! Lazily built render pipelines, one per [`PipelineKey`].
//!
//! [`PipelineCache`] compiles a pipeline the first time a draw needs its key
//! and reuses it afterwards. One Noesis shader can map to several pipelines
//! because blend mode, stencil mode, color writes, the target's stencil and
//! sample count, and the custom shader are all part of the key.

use std::collections::{HashMap, HashSet};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use noesis_runtime::render_device::types::{
    Batch, BlendMode, FORMAT_FOR_VERTEX, RenderState, Shader, StencilMode, VERTEX_FOR_SHADER,
};

use crate::custom_shader::{
    MAX_SHADER_CONSTANTS, MAX_SHADER_TEXTURES, PixelShaderDesc, PixelShaderError,
    PixelShaderHandle, PixelShaderKind,
};
use crate::shader_defines::{defines_for_custom, defines_for_shader};
use crate::shader_preproc::preprocess;
use crate::vertex_layout::{attributes_for_format, stride_for_format};

const NOESIS_WGSL: &str = include_str!("shaders/noesis.wgsl");

/// One pipeline state combination. Each distinct key gets one cached
/// `wgpu::RenderPipeline`. Build it from a batch with [`PipelineKey::from_batch`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PipelineKey {
    /// Raw `Shader::Enum` value selecting which WGSL variant to compile.
    pub shader: u8,
    /// Raw `RenderState` bits: blend mode, stencil mode, color-write enable,
    /// and the wireframe flag (which the pipeline ignores).
    pub render_state: u8,
    /// Raw `VertexFormat::Enum` value selecting the vertex stride and
    /// attribute layout. Always the format the SDK tables assign to `shader`.
    pub vertex_format: u8,
    /// Whether the destination pass has a stencil attachment. wgpu requires
    /// the pipeline's `depth_stencil` to match the pass, and the same batch
    /// state can draw into both stenciled and stencil-less targets.
    pub has_stencil: bool,
    /// The destination's MSAA sample count, 1 or 4.
    pub sample_count: u32,
    /// The custom shader standing in for part of `shader`, if any.
    pub pixel_shader: Option<PixelShaderHandle>,
}

impl PipelineKey {
    /// Key for drawing `batch`, with `pixel_shader` if it has one, into a
    /// pass with `sample_count` samples that does (`has_stencil`) or doesn't
    /// have a stencil attachment. `None` when `batch.shader` is out of range
    /// for the SDK lookup tables.
    #[must_use]
    pub fn from_batch(
        batch: &Batch,
        has_stencil: bool,
        sample_count: u32,
        pixel_shader: Option<PixelShaderHandle>,
    ) -> Option<Self> {
        let vshader = *VERTEX_FOR_SHADER.get(usize::from(batch.shader.0))?;
        let vfmt = FORMAT_FOR_VERTEX[vshader as usize];
        Some(Self {
            shader: batch.shader.0,
            render_state: batch.render_state.0,
            vertex_format: vfmt,
            has_stencil,
            sample_count,
            pixel_shader,
        })
    }
}

/// Stencil format for render targets and the onscreen stencil. There is no
/// depth aspect, so the `*_ZTest` stencil modes behave like their non-depth
/// twins.
pub const STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;

/// Build the `wgpu::DepthStencilState` for a `RenderState`'s stencil mode.
///
/// The stencil *reference* is dynamic and applied per draw via
/// `set_stencil_reference`, not baked here. Depth is always `Always` / no-write:
/// `Stencil8` carries no depth aspect, and the `Disabled_ZTest` /
/// `Equal_Keep_ZTest` modes (which need a depth buffer for 3D-transformed UI)
/// degrade to their non-depth equivalents.
fn depth_stencil_for(render_state: RenderState) -> wgpu::DepthStencilState {
    use wgpu::{CompareFunction, StencilOperation};

    let (compare, pass_op, fail_op) = match render_state.stencil_mode_raw() {
        // Disabled / Disabled_ZTest: stencil test off (always pass, no write).
        0 | 5 => (
            CompareFunction::Always,
            StencilOperation::Keep,
            StencilOperation::Keep,
        ),
        // Equal_Keep / Equal_Keep_ZTest: pass where stencil == ref; keep.
        1 | 6 => (
            CompareFunction::Equal,
            StencilOperation::Keep,
            StencilOperation::Keep,
        ),
        // Equal_Incr: pass where == ref; increment (wrap) on pass.
        2 => (
            CompareFunction::Equal,
            StencilOperation::IncrementWrap,
            StencilOperation::Keep,
        ),
        // Equal_Decr: pass where == ref; decrement (wrap) on pass.
        3 => (
            CompareFunction::Equal,
            StencilOperation::DecrementWrap,
            StencilOperation::Keep,
        ),
        // Clear: always pass; zero the stencil. `pass_op != Keep` makes wgpu enable
        // VK_DYNAMIC_STATE_STENCIL_REFERENCE, but `compare: Always` keeps
        // `needs_ref_value()` false, so wgpu drops every `set_stencil_reference` and
        // the ref goes unset (VUID-vkCmdDrawIndexed-None-07839). `Always` never fails,
        // so a dead `fail_op: Replace` flips `needs_ref_value()` true to keep it emitted.
        4 => (
            CompareFunction::Always,
            StencilOperation::Zero,
            StencilOperation::Replace,
        ),
        // Unknown SDK raw: degrade rather than panic inside the FFI callback.
        other => {
            warn_once!("unknown StencilMode raw value {other}; disabling stencil test");
            (
                CompareFunction::Always,
                StencilOperation::Keep,
                StencilOperation::Keep,
            )
        }
    };
    let face = wgpu::StencilFaceState {
        compare,
        fail_op,
        depth_fail_op: StencilOperation::Keep,
        pass_op,
    };
    wgpu::DepthStencilState {
        format: STENCIL_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(CompareFunction::Always),
        stencil: wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 0xff,
            write_mask: 0xff,
        },
        bias: wgpu::DepthBiasState::default(),
    }
}

/// A registered custom shader.
pub struct CustomShader {
    pub label: String,
    pub kind: PixelShaderKind,
    pub source: String,
    pub textures: u32,
    pub constants: u32,
}

/// Pipelines built on demand from `noesis.wgsl`, keyed by [`PipelineKey`],
/// and the custom shaders some of them run.
///
/// Every pipeline has four bind groups: `group(0)` vertex uniforms, `group(1)`
/// pixel uniforms (`cbuffer0_ps`, `cbuffer1_ps`, and the pattern mip policy),
/// `group(2)` the paint texture and sampler, and `group(3)` the image, shadow
/// and glyph textures and samplers. Custom pipelines add their extra textures
/// to `group(2)`, so they have a layout of their own. A shader that ignores a
/// group still has to have something bound there.
pub struct PipelineCache {
    device: wgpu::Device,
    pipeline_layout: wgpu::PipelineLayout,
    custom_pipeline_layout: wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    cache: HashMap<PipelineKey, wgpu::RenderPipeline>,
    custom_shaders: HashMap<PixelShaderHandle, CustomShader>,
    // Custom pipelines that failed validation, so they aren't rebuilt per draw.
    failed: HashSet<PipelineKey>,
}

impl PipelineCache {
    /// Creates an empty cache. The layouts must have the four groups
    /// described on [`PipelineCache`], and `target_format` is the color format
    /// of every target the pipelines will draw into.
    #[must_use]
    pub fn new(
        device: wgpu::Device,
        pipeline_layout: wgpu::PipelineLayout,
        custom_pipeline_layout: wgpu::PipelineLayout,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        Self {
            device,
            pipeline_layout,
            custom_pipeline_layout,
            target_format,
            cache: HashMap::new(),
            custom_shaders: HashMap::new(),
            failed: HashSet::new(),
        }
    }

    /// Builds the pipeline for `key` if it isn't cached yet, and returns
    /// `false` when `key.shader` has no WGSL variant (see
    /// [`defines_for_shader`] and [`defines_for_custom`]), needs a device
    /// feature that's missing, or names a custom shader that is unknown or
    /// fails validation for this key. Fetch the pipeline with [`Self::get`];
    /// the split lets the caller hold other borrows between the two calls. A
    /// built-in variant that fails WGSL validation is reported through the
    /// wgpu device's error handler.
    pub fn ensure(&mut self, key: PipelineKey) -> bool {
        if self.cache.contains_key(&key) {
            return true;
        }
        if let Some(handle) = key.pixel_shader {
            return self.ensure_custom(key, handle);
        }
        let Some(defines) = defines_for_shader(Shader(key.shader)) else {
            return false;
        };
        if key.shader == Shader::SDF_LCD_SOLID.0
            && !self
                .device
                .features()
                .contains(wgpu::Features::DUAL_SOURCE_BLENDING)
        {
            return false;
        }
        let source = preprocess(NOESIS_WGSL, &defines);
        let pipeline = build_pipeline(
            &self.device,
            &self.pipeline_layout,
            self.target_format,
            key,
            source,
        );
        self.cache.insert(key, pipeline);
        true
    }

    fn ensure_custom(&mut self, key: PipelineKey, handle: PixelShaderHandle) -> bool {
        if self.failed.contains(&key) {
            return false;
        }
        let Some(shader) = self.custom_shaders.get(&handle) else {
            return false;
        };
        match build_custom_pipeline(
            &self.device,
            &self.custom_pipeline_layout,
            self.target_format,
            key,
            shader,
        ) {
            Ok(pipeline) => {
                self.cache.insert(key, pipeline);
                true
            }
            Err(error) => {
                log::warn!(
                    "custom shader '{}' failed as Noesis shader {}: {error}",
                    shader.label,
                    key.shader,
                );
                self.failed.insert(key);
                false
            }
        }
    }

    /// Registers `desc` under `handle` after checking that it compiles, by
    /// building its pipeline for a representative batch: a path pattern for a
    /// brush, `CUSTOM_EFFECT` for an effect.
    pub fn register_custom(
        &mut self,
        handle: PixelShaderHandle,
        desc: &PixelShaderDesc<'_>,
    ) -> Result<(), PixelShaderError> {
        if desc.textures as usize > MAX_SHADER_TEXTURES {
            return Err(PixelShaderError(format!(
                "'{}' samples {} extra textures; the most is {MAX_SHADER_TEXTURES}",
                desc.label, desc.textures,
            )));
        }
        if desc.constants > MAX_SHADER_CONSTANTS {
            return Err(PixelShaderError(format!(
                "'{}' has {} bytes of constants; the most is {MAX_SHADER_CONSTANTS}",
                desc.label, desc.constants,
            )));
        }
        let shader = CustomShader {
            label: desc.label.to_owned(),
            kind: desc.kind,
            source: desc.source.to_owned(),
            textures: desc.textures,
            constants: desc.constants,
        };
        let probe = Shader(match desc.kind {
            PixelShaderKind::Brush => Shader::PATH_PATTERN.0,
            PixelShaderKind::Effect => Shader::CUSTOM_EFFECT.0,
        });
        let vshader = VERTEX_FOR_SHADER[usize::from(probe.0)];
        let key = PipelineKey {
            shader: probe.0,
            render_state: RenderState::new(true, BlendMode::SrcOver, StencilMode::Disabled, false)
                .0,
            vertex_format: FORMAT_FOR_VERTEX[vshader as usize],
            has_stencil: false,
            sample_count: 1,
            pixel_shader: Some(handle),
        };
        let pipeline = build_custom_pipeline(
            &self.device,
            &self.custom_pipeline_layout,
            self.target_format,
            key,
            &shader,
        )
        .map_err(|error| PixelShaderError(format!("'{}': {error}", desc.label)))?;
        self.cache.insert(key, pipeline);
        self.custom_shaders.insert(handle, shader);
        Ok(())
    }

    /// Forgets the custom shader `handle` and its pipelines.
    pub fn remove_custom(&mut self, handle: PixelShaderHandle) {
        self.custom_shaders.remove(&handle);
        self.cache.retain(|key, _| key.pixel_shader != Some(handle));
        self.failed.retain(|key| key.pixel_shader != Some(handle));
    }

    /// The custom shader registered under `handle`.
    #[must_use]
    pub fn custom(&self, handle: PixelShaderHandle) -> Option<&CustomShader> {
        self.custom_shaders.get(&handle)
    }

    /// Number of pipelines built so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Returns the pipeline built by [`Self::ensure`] for `key`.
    ///
    /// # Panics
    ///
    /// Panics if [`Self::ensure`] wasn't called for `key` first.
    #[must_use]
    pub fn get(&self, key: PipelineKey) -> &wgpu::RenderPipeline {
        self.cache
            .get(&key)
            .expect("pipeline not built; call ensure() before get()")
    }
}

/// wgpu blend state for a raw `BlendMode::Enum`. `None` is `BlendMode::Src`
/// (overwrite). All modes assume premultiplied alpha.
fn blend_state_for(blend_mode_raw: u8) -> Option<wgpu::BlendState> {
    let comp = |src, dst| wgpu::BlendComponent {
        src_factor: src,
        dst_factor: dst,
        operation: wgpu::BlendOperation::Add,
    };
    let src_over_alpha = comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha);

    match blend_mode_raw {
        0 => None, // BlendMode::Src: straight overwrite
        1 => Some(wgpu::BlendState {
            // BlendMode::SrcOver: cs + cd*(1-as), as + ad*(1-as); premultiplied alpha
            color: src_over_alpha,
            alpha: src_over_alpha,
        }),
        2 => Some(wgpu::BlendState {
            // BlendMode::SrcOverMultiply: cs*cd + cd*(1-as)
            color: comp(wgpu::BlendFactor::Dst, wgpu::BlendFactor::OneMinusSrcAlpha),
            alpha: src_over_alpha,
        }),
        3 => Some(wgpu::BlendState {
            // BlendMode::SrcOverScreen: cs + cd*(1-cs)
            color: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc),
            alpha: src_over_alpha,
        }),
        4 => Some(wgpu::BlendState {
            // BlendMode::SrcOverAdditive: cs + cd (additive)
            color: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::One),
            alpha: src_over_alpha,
        }),
        5 => Some(wgpu::BlendState {
            // BlendMode::SrcOverDual: cs + cd*(1 - src1) per channel. src1 is
            // the LCD shader's `@blend_src(1)` coverage; needs DUAL_SOURCE_BLENDING.
            color: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc1),
            alpha: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc1Alpha),
        }),
        // Unknown SDK raw: degrade rather than panic inside the FFI callback.
        other => {
            warn_once!("unknown BlendMode raw value {other}; using SrcOver");
            Some(wgpu::BlendState {
                color: src_over_alpha,
                alpha: src_over_alpha,
            })
        }
    }
}

/// Builds `shader`'s pipeline for `key`, with wgpu's validation errors
/// returned instead of sent to the device's error handler: the WGSL comes
/// from the host.
fn build_custom_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    key: PipelineKey,
    shader: &CustomShader,
) -> Result<wgpu::RenderPipeline, String> {
    let Some(defines) = defines_for_custom(Shader(key.shader), shader.kind, shader.constants > 0)
    else {
        return Err(format!("a {:?} can't draw as this shader", shader.kind));
    };
    let mut source = preprocess(NOESIS_WGSL, &defines);
    source.push_str(&shader.source);

    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = build_pipeline(device, layout, target_format, key, source);
    // Native wgpu resolves the scope immediately, so one poll gets the error.
    let mut error = pin!(scope.pop());
    match error.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(Some(error)) => Err(error.to_string()),
        _ => Ok(pipeline),
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    key: PipelineKey,
    source: String,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("noesis_wgpu Shader({})", key.shader)),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });

    let attrs = attributes_for_format(key.vertex_format);
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: stride_for_format(key.vertex_format),
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attrs,
    };

    // Stencil-only MASK draws output vec4(1.0) with color_enable off; honoring
    // the flag keeps those white pixels out of the color target.
    let render_state = RenderState(key.render_state);
    let blend = blend_state_for(render_state.blend_mode_raw());
    let write_mask = if render_state.color_enable() {
        wgpu::ColorWrites::ALL
    } else {
        wgpu::ColorWrites::empty()
    };
    let depth_stencil = key.has_stencil.then(|| depth_stencil_for(render_state));

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!(
            "noesis_wgpu pipeline shader={} state=0x{:02x} fmt={}",
            key.shader, key.render_state, key.vertex_format
        )),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout)],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil,
        multisample: wgpu::MultisampleState {
            count: key.sample_count,
            ..wgpu::MultisampleState::default()
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend,
                write_mask,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
