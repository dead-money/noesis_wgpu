//! A wgpu render device for the [Noesis GUI](https://www.noesisengine.com/)
//! Native SDK.
//!
//! [`WgpuRenderDevice`] implements Noesis's `RenderDevice` protocol on a
//! `wgpu::Device` and `wgpu::Queue` you provide: it creates Noesis's textures
//! and render targets, compiles a pipeline per shader and render state, and
//! records each batch Noesis draws.
//!
//! The device renders into `Rgba8Unorm` targets only. It is driven from the
//! thread that owns the Noesis view and renderer, and it submits its own
//! command encoders: one when the offscreen phase ends and one when the
//! onscreen phase ends.
//!
//! The protocol types ([`Batch`](noesis_runtime::render_device::types::Batch),
//! [`TextureHandle`](noesis_runtime::render_device::TextureHandle), and so on)
//! come from [`noesis_runtime`].

macro_rules! warn_once {
    ($($arg:tt)*) => {{
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| log::warn!($($arg)*));
    }};
}

mod device;
mod pipeline;
mod shader_defines;
mod shader_preproc;
mod vertex_layout;

pub use device::WgpuRenderDevice;
