//! Custom pixel shaders: WGSL that stands in for the shaders Noesis's
//! `BrushShader` and `ShaderEffect` objects carry.

use std::ffi::c_void;
use std::fmt;
use std::num::NonZeroU64;

use noesis_runtime::render_device::TextureHandle;
#[cfg(feature = "shim")]
use noesis_runtime::render_device::types::Batch;
use noesis_runtime::render_device::types::SamplerState;

/// The most extra textures a custom shader can sample.
pub const MAX_SHADER_TEXTURES: usize = 4;

/// The largest `Constants` struct a custom shader can declare, in bytes: the
/// size of Noesis's `cbuffer1_ps`, 128 floats.
pub const MAX_SHADER_CONSTANTS: u32 = 512;

/// The device's identifier for a custom pixel shader, returned by
/// [`WgpuRenderDevice::create_pixel_shader`].
///
/// Noesis treats a custom shader as an opaque pointer: the application hands
/// it to `BrushShader::SetPixelShader` or `ShaderEffect::SetPixelShader`, and
/// Noesis passes it back as `Batch::pixel_shader`. A host that creates those
/// objects itself can hand Noesis its own pointers and map them to handles when
/// it draws, as it does for textures. Otherwise, hand Noesis
/// [`as_ptr`](Self::as_ptr), which [`BatchShader::from_batch`] reads back.
///
/// [`WgpuRenderDevice::create_pixel_shader`]: crate::WgpuRenderDevice::create_pixel_shader
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PixelShaderHandle(pub NonZeroU64);

impl PixelShaderHandle {
    /// The handle as a pointer for Noesis's `SetPixelShader`. It holds the
    /// handle's value and points at nothing.
    #[must_use]
    pub fn as_ptr(self) -> *mut c_void {
        std::ptr::without_provenance_mut(self.0.get() as usize)
    }

    /// Reads back a handle that [`as_ptr`](Self::as_ptr) turned into a
    /// pointer. `None` for null.
    #[must_use]
    pub fn from_ptr(ptr: *mut c_void) -> Option<Self> {
        NonZeroU64::new(ptr.addr() as u64).map(Self)
    }
}

/// Which Noesis object a custom shader serves, which decides the function its
/// source defines and the batches it draws.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PixelShaderKind {
    /// A `BrushShader`: computes an `ImageBrush`'s paint. The source defines
    ///
    /// ```wgsl
    /// fn main_brush(uv: vec2<f32>) -> vec4<f32>
    /// ```
    ///
    /// which returns the premultiplied color at pattern coordinate `uv`. It
    /// replaces the texture fetch of the batch's pattern shader, so one
    /// brush draws paths, antialiased paths, text and opacity masks alike:
    /// the device applies the opacity, coverage or glyph alpha as Noesis's
    /// shader for that batch would. `sample_image(uv)` samples the brush's
    /// image under the [`PatternLod`](crate::PatternLod) policy. The
    /// `SDF_LCD_*` pattern shaders aren't supported.
    Brush,
    /// A `ShaderEffect`: post-processes an element's rendered image. The
    /// source defines
    ///
    /// ```wgsl
    /// fn main_effect() -> vec4<f32>
    /// ```
    ///
    /// which returns the premultiplied output color for the current pixel.
    /// It draws `CUSTOM_EFFECT` batches, whose pattern texture is the input.
    /// The helpers follow Noesis's `EffectHelpers.h`: `get_input()`,
    /// `sample_input(uv)`, `sample_input_at_offset(pixels)`,
    /// `sample_input_at_position(pixels)`, `input_coordinate()`,
    /// `normalized_input_coordinate()` and `image_position()`.
    Effect,
}

/// Parameters for [`WgpuRenderDevice::create_pixel_shader`].
///
/// The source is WGSL appended to the device's own `noesis.wgsl` variant for
/// the batch being drawn, so it shares that module's namespace: name things
/// so they don't collide with it. Besides the functions listed under
/// [`PixelShaderKind`], it can use:
///
/// - `sample_texture(index: u32, uv: vec2<f32>) -> vec4<f32>`, which samples
///   extra texture `index`, or the raw `extra_texture0`..`extra_texture3` and
///   `extra_sampler0`..`extra_sampler3`.
/// - `constants`, a uniform of the `struct Constants` the source declares,
///   when [`constants`](Self::constants) isn't zero.
///
/// [`WgpuRenderDevice::create_pixel_shader`]: crate::WgpuRenderDevice::create_pixel_shader
#[derive(Copy, Clone, Debug)]
pub struct PixelShaderDesc<'a> {
    /// Debug label for the shader modules and pipelines.
    pub label: &'a str,
    /// The kind of shader, which decides the function `source` defines.
    pub kind: PixelShaderKind,
    /// WGSL defining `main_brush` or `main_effect`, and `Constants` when the
    /// shader has any.
    pub source: &'a str,
    /// How many extra textures the shader samples, at most
    /// [`MAX_SHADER_TEXTURES`]. A draw must provide each of them in
    /// [`BatchShader::textures`].
    pub textures: u32,
    /// Size of the `Constants` struct in bytes, at most
    /// [`MAX_SHADER_CONSTANTS`]; zero when the shader declares none. Each
    /// draw uploads this many bytes, zero-padding shorter data.
    pub constants: u32,
}

/// Why [`WgpuRenderDevice::create_pixel_shader`] rejected a shader: the
/// description is out of range, or wgpu's validation message for the WGSL.
///
/// [`WgpuRenderDevice::create_pixel_shader`]: crate::WgpuRenderDevice::create_pixel_shader
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelShaderError(pub String);

impl fmt::Display for PixelShaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PixelShaderError {}

/// The custom shader a batch draws with, and its inputs, as device handles.
///
/// Pass it to [`WgpuRenderDevice::draw_custom_batch`] for a batch whose
/// `pixel_shader` is set. A host that hands Noesis its own pointers resolves
/// the pointer to the [`PixelShaderHandle`] it registered, as
/// [`BatchTextures`](crate::BatchTextures) does for textures.
///
/// [`WgpuRenderDevice::draw_custom_batch`]: crate::WgpuRenderDevice::draw_custom_batch
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BatchShader<'a> {
    /// The shader, from [`WgpuRenderDevice::create_pixel_shader`].
    ///
    /// [`WgpuRenderDevice::create_pixel_shader`]: crate::WgpuRenderDevice::create_pixel_shader
    pub shader: PixelShaderHandle,
    /// The extra textures, each with the sampler state to read it with. Noesis
    /// puts a `BrushShader`'s texture 0 in the batch's shadow slot, which
    /// [`from_batch`](Self::from_batch) reads; a host that keeps a brush's
    /// textures itself fills these from its own records.
    pub textures: [Option<(TextureHandle, SamplerState)>; MAX_SHADER_TEXTURES],
    /// The `Constants` bytes. `None` uses the batch's `pixel_uniforms[1]`,
    /// where Noesis puts the buffer set with `SetConstantBuffer`.
    pub constants: Option<&'a [u8]>,
}

impl BatchShader<'_> {
    /// `shader` with no extra textures, reading its constants from the batch.
    #[must_use]
    pub fn new(shader: PixelShaderHandle) -> Self {
        Self {
            shader,
            textures: [None; MAX_SHADER_TEXTURES],
            constants: None,
        }
    }

    /// Reads `batch.pixel_shader` as a [`PixelShaderHandle::as_ptr`] value,
    /// and the batch's shadow texture as extra texture 0, through
    /// `noesis_runtime`'s shim. `None` when the batch has no custom shader.
    #[cfg(feature = "shim")]
    #[must_use]
    pub fn from_batch(batch: &Batch) -> Option<Self> {
        let mut shader = Self::new(PixelShaderHandle::from_ptr(batch.pixel_shader)?);
        shader.textures[0] = batch.shadow_handle().map(|h| (h, batch.shadow_sampler));
        Some(shader)
    }
}
