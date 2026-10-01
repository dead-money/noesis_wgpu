# noesis_wgpu

[![CI](https://github.com/dead-money/noesis_wgpu/actions/workflows/ci.yml/badge.svg)](https://github.com/dead-money/noesis_wgpu/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/noesis_wgpu.svg)](https://crates.io/crates/noesis_wgpu)
[![docs.rs](https://docs.rs/noesis_wgpu/badge.svg)](https://docs.rs/noesis_wgpu)

A [wgpu](https://wgpu.rs/) render device for the [Noesis GUI](https://www.noesisengine.com/) Native SDK. Noesis lays out and animates XAML UI but draws nothing itself: it hands a render device textures, render targets, and batches of triangles. `WgpuRenderDevice` draws them on a `wgpu::Device` and `wgpu::Queue` you provide.

It works with the Rust bindings in [`noesis_runtime`](https://github.com/dead-money/noesis_runtime), and also without them, for a host that loads Noesis another way and forwards the device calls. [`noesis_bevy`](https://github.com/dead-money/noesis_bevy) uses it to render Noesis in Bevy.

Built for Dead Money's own games and mostly written by AI agents under human direction.

## You need a Noesis license

With the default `shim` feature, this crate links the [Noesis Native SDK](https://www.noesisengine.com/) through `noesis_runtime`. The SDK is closed-source commercial software from Noesis Technologies S.L. that we don't redistribute. Buy it separately and point `NOESIS_SDK_DIR` at your install.

This release targets **Noesis Native SDK 3.2.13**, the version `noesis_runtime` is built against.

Without the `shim` feature, nothing here or in `noesis_runtime` touches the SDK: the build needs no `NOESIS_SDK_DIR` and links no Noesis library.

## Quick start

```toml
[dependencies]
noesis_runtime = "0.13"
noesis_wgpu = "0.2"
wgpu = "30"
```

Create the device on your wgpu device and queue, point it at the texture Noesis should draw into, and register it with `noesis_runtime`:

```rust
use noesis_runtime::render_device::register;
use noesis_runtime::view::{FrameworkElement, View};
use noesis_wgpu::WgpuRenderDevice;

noesis_runtime::init();

let root = FrameworkElement::parse(
    r#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
           <TextBlock Text="Hello"/>
       </Grid>"#,
)
.expect("XAML parses");
let mut view = View::create(root);
view.set_size(1280, 720);

// `target` is an Rgba8Unorm texture view the UI renders into.
let mut device = WgpuRenderDevice::new(wgpu_device.clone(), wgpu_queue.clone());
device.set_onscreen_target(target, 1280, 720);
let device = register(device);
view.renderer().init(&device);

// Each frame:
view.update(time_seconds);
let mut renderer = view.renderer();
renderer.update_render_tree();
renderer.render_offscreen();
renderer.render(false, true);
```

The device submits its own command encoders when the offscreen and onscreen phases end, so the UI texture is ready for anything you submit to the same queue afterwards. To render into a new target, for example after a resize, call `set_onscreen_target` again through `Registered::device_mut`.

### Driving the device yourself

A host that receives Noesis's render-device calls some other way, such as a C# `RenderDevice` in the managed SDK that forwards them over a C ABI, calls the same methods on `WgpuRenderDevice` directly. Turn off default features so no Noesis library is linked:

```toml
[dependencies]
noesis_runtime = { version = "0.13", default-features = false }
noesis_wgpu = { version = "0.2", default-features = false }
```

That host creates Noesis's texture objects, so it knows which `TextureHandle` each texture pointer in a `Batch` stands for. It passes them with the batch:

```rust
use noesis_wgpu::{BatchTextures, WgpuRenderDevice};

let textures = BatchTextures {
    pattern: handle_for(batch.pattern),
    ramps: handle_for(batch.ramps),
    image: handle_for(batch.image),
    glyphs: handle_for(batch.glyphs),
    shadow: handle_for(batch.shadow),
};
device.draw_batch_with(batch, textures);
```

## How it works

- **One shader source.** `noesis.wgsl` covers Noesis's shader set with `#ifdef` branches, the convention of Noesis's own GL shaders. The device strips it down to one variant per Noesis shader and compiles a pipeline the first time a draw needs that shader, render state, vertex format, and stencil combination.
- **One encoder per phase.** `begin_offscreen_render` and `begin_onscreen_render` each open a command encoder, and the matching `end_*` submits it. Every draw records its own render pass.
- **Per-draw uniforms.** All of a phase's buffer writes land before its encoder is submitted, so each draw writes its uniforms to its own slot of a ring buffer, and geometry is appended rather than overwritten.
- **Stencil clipping.** Render targets that ask for one, and the onscreen target, get a `Stencil8` buffer, cleared before the first draw into each target.
- **No extra wgpu features.** Text uses Noesis's grayscale SDF path. Subpixel (LCD) text, which needs dual-source blending, is off.

What it doesn't do yet: MSAA render targets (Noesis's offscreen sample count must stay 1), custom pixel shaders (`ShaderEffect` and `BrushShader` batches), and the `SDF_*` gradient and pattern paints. A phase holds at most 1024 draws.

## Version compatibility

| noesis_wgpu | wgpu | Bevy (through noesis_bevy) |
|-------------|------|----------------------------|
| 0.2         | 30   |                            |
| 0.1         | 29   | 0.19                       |

wgpu types are part of the API, so each wgpu major gets a new `noesis_wgpu` minor. Older lines get fixes on `release/0.N` branches while a `noesis_bevy` release still uses them.

## Building

```sh
unzip NoesisGUI-NativeSDK-linux-3.2.13-Indie.zip -d ~/sdks/noesis-3.2.13
export NOESIS_SDK_DIR=~/sdks/noesis-3.2.13
cargo test
```

The tests render on a real GPU adapter and read the pixels back; a software Vulkan driver such as lavapipe works too. They drive the device directly and need no Noesis license, so `cargo test --no-default-features` runs them without the SDK.

## License

[MIT](./LICENSE). No Noesis SDK code is included. Binaries that link the SDK are covered by the Noesis EULA.

## Acknowledgements

Built on [wgpu](https://wgpu.rs/) and the [Noesis](https://www.noesisengine.com/) Native SDK. The render-device protocol and shader set follow the SDK's own reference devices; the [Noesis documentation](https://docs.noesisengine.com/) is the source of truth for how Noesis drives a device. Report SDK bugs there; report bugs in this device here.
