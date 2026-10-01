//! A wgpu render device for the [Noesis GUI](https://www.noesisengine.com/)
//! Native SDK.
//!
//! Noesis does no drawing of its own: it hands a render device textures to
//! create, render targets to fill, and batches of triangles to draw.
//! [`WgpuRenderDevice`] does that work on a `wgpu::Device` and `wgpu::Queue`
//! you provide. It compiles a pipeline for each shader and render state Noesis
//! asks for, and submits its own command encoders: one when the offscreen
//! phase ends and one when the onscreen phase ends.
//!
//! The device renders into `Rgba8Unorm` targets. Use it from the thread that
//! drives the Noesis view and renderer.
//!
//! # Driving the device
//!
//! With the default `shim` feature, the device implements
//! [`noesis_runtime`]'s `RenderDevice` trait. Register it with
//! `noesis_runtime::render_device::register`, bind it to a view's renderer,
//! and Noesis calls it directly:
//!
//! ```ignore
//! let mut device = noesis_wgpu::WgpuRenderDevice::new(wgpu_device, wgpu_queue);
//! device.set_onscreen_target(target_view, width, height);
//! let registered = noesis_runtime::render_device::register(device);
//! view.renderer().init(&registered);
//! ```
//!
//! A host that receives Noesis's device calls some other way, such as a C#
//! `RenderDevice` that forwards them over a C ABI, calls the same methods
//! itself. That host creates Noesis's texture objects, so it knows which
//! [`TextureHandle`](noesis_runtime::render_device::TextureHandle) each texture
//! pointer in a [`Batch`](noesis_runtime::render_device::types::Batch) stands
//! for. It passes them to [`WgpuRenderDevice::draw_batch_with`] as
//! [`BatchTextures`].
//!
//! # Features
//!
//! - `shim` (default): implements `noesis_runtime`'s `RenderDevice` trait and
//!   `BatchTextures::from_batch`. Turns on `noesis_runtime`'s own `shim`
//!   feature, so the build needs the Noesis Native SDK. Without it, this crate
//!   and `noesis_runtime` build with no SDK and link no Noesis library.

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

pub use device::{BatchTextures, WgpuRenderDevice};
