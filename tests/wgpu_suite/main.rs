//! GPU tests that drive `WgpuRenderDevice` directly, one test per file, each on
//! its own wgpu device.

mod wgpu_effects;
mod wgpu_first_triangle;
mod wgpu_geometry_stream;
mod wgpu_multi_shader;
mod wgpu_offscreen_rt;
mod wgpu_pattern;
mod wgpu_pattern_wrap;
mod wgpu_radial;
mod wgpu_sdf_lcd;
mod wgpu_shadow_blur;
mod wgpu_stencil_clip;
mod wgpu_uniform_ring;

// The native backends, unless `WGPU_BACKEND` names others. GL is left out because EGL setup
// hangs when several tests start it at once without a display session, as on a CI runner
// service.
fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    if wgpu::Backends::from_env().is_none() {
        descriptor.backends = wgpu::Backends::PRIMARY;
    }

    wgpu::Instance::new(descriptor)
}
