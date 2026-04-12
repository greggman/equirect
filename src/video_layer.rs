//! XR composition-layer video swapchain.
//!
//! Instead of rendering the video into the eye swapchains with a shader,
//! we blit each decoded frame into a dedicated XR swapchain and submit it
//! as a `CompositionLayerQuad / Cylinder / Equirect2` to the runtime.
//! The runtime then handles projection, stereo, and reprojection natively.
//!
//! Fisheye modes are **not** handled here; they fall back to the shader path
//! in `video_renderer.rs`.

use std::mem::ManuallyDrop;
use openxr as xr;
use crate::vprintln;

use crate::ui::settings::{StereoLayout, VideoMode, VideoSettings};
use crate::video::texture::VideoTexture;

// ── blit pipeline ─────────────────────────────────────────────────────────────

struct VideoBlit {
    pipeline:    wgpu::RenderPipeline,
    bgl:         wgpu::BindGroupLayout,
    sampler:     wgpu::Sampler,
    bind_group:  Option<wgpu::BindGroup>,
}

impl VideoBlit {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(
            wgpu::include_wgsl!("shaders/video-blit.wgsl")
        );

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video_blit_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video_blit_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video_blit_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self { pipeline, bgl, sampler, bind_group: None }
    }

    fn set_texture(&mut self, device: &wgpu::Device, texture: &VideoTexture) {
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video_blit_bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        }));
    }

    fn blit(
        &self,
        target: &wgpu::TextureView,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let Some(bg) = &self.bind_group else { return };

        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video_blit_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([enc.finish()]);
        device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).ok();
    }
}

// ── VideoSwapchain ─────────────────────────────────────────────────────────────

/// An OpenXR swapchain that holds one decoded video frame.
/// Used as the source for XR composition layers (Quad / Cylinder / Equirect2).
/// The swapchain is wrapped in `ManuallyDrop` so we can `mem::forget` it on
/// tear-down, matching the pattern used for eye swapchains (avoids the
/// Oculus xrDestroySwapchain-after-xrEndSession crash).
pub struct VideoSwapchain {
    pub swapchain: ManuallyDrop<xr::Swapchain<xr::Vulkan>>,
    textures: Vec<wgpu::Texture>,
    pub width:  u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    blit: VideoBlit,
}

impl VideoSwapchain {
    /// Create a new video swapchain sized to `(width, height)` and set up the
    /// blit pipeline.  Returns `None` if the XR runtime rejects the swapchain.
    pub fn new(
        session:  &xr::Session<xr::Vulkan>,
        device:   &wgpu::Device,
        width:    u32,
        height:   u32,
        format:   wgpu::TextureFormat,
        vk_format: u32,
    ) -> Option<Self> {
        let swapchain = session
            .create_swapchain(&xr::SwapchainCreateInfo {
                create_flags: xr::SwapchainCreateFlags::EMPTY,
                usage_flags: xr::SwapchainUsageFlags::SAMPLED
                    | xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
                format: vk_format,
                sample_count: 1,
                width,
                height,
                face_count: 1,
                array_size: 1,
                mip_count: 1,
            })
            .map_err(|e| eprintln!("XR: create video swapchain failed: {e}"))
            .ok()?;

        let raw_images = swapchain
            .enumerate_images()
            .map_err(|e| eprintln!("XR: enumerate video swapchain images failed: {e}"))
            .ok()?;

        let textures = raw_images
            .iter()
            .map(|&img| unsafe { wrap_xr_image_render(device, img, width, height, format) })
            .collect();

        vprintln!("XR: video swapchain {}×{} × {} images ({:?})", width, height, raw_images.len(), format);

        let blit = VideoBlit::new(device, format);
        Some(Self {
            swapchain: ManuallyDrop::new(swapchain),
            textures,
            width,
            height,
            format,
            blit,
        })
    }

    /// Call after uploading a new decoded frame to `VideoTexture` to rebind the
    /// blit source.
    pub fn set_texture(&mut self, device: &wgpu::Device, texture: &VideoTexture) {
        self.blit.set_texture(device, texture);
    }

    /// Acquire an image from the swapchain, blit the latest video frame into
    /// it, and release it.  Must be called once per frame (not per eye).
    pub fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let Ok(idx) = self.swapchain.acquire_image() else { return };
        if self.swapchain.wait_image(xr::Duration::INFINITE).is_err() { return; }
        let view = self.textures[idx as usize].create_view(&Default::default());
        self.blit.blit(&view, device, queue);
        let _ = self.swapchain.release_image();
    }

    // ── layer parameter helpers ───────────────────────────────────────────────

    /// Number of layers to submit (1 for mono, 2 for stereo).
    pub fn layer_count(settings: &VideoSettings) -> usize {
        if settings.stereo == StereoLayout::OneView { 1 } else { 2 }
    }

    /// `SwapchainSubImage` for a given eye (0=left, 1=right).
    /// Crops to the appropriate half of the image for stereo layouts.
    pub fn sub_image<'a>(&'a self, settings: &VideoSettings, eye: usize)
        -> xr::SwapchainSubImage<'a, xr::Vulkan>
    {
        let (off_x, off_y, ext_w, ext_h) = image_rect(
            self.width, self.height, settings.stereo, eye,
        );
        xr::SwapchainSubImage::new()
            .swapchain(&self.swapchain)
            .image_array_index(0)
            .image_rect(xr::Rect2Di {
                offset: xr::Offset2Di { x: off_x, y: off_y },
                extent: xr::Extent2Di { width: ext_w, height: ext_h },
            })
    }

    /// `EyeVisibility` for a given eye index in the layer array.
    pub fn eye_visibility(settings: &VideoSettings, layer_idx: usize) -> xr::EyeVisibility {
        if settings.stereo == StereoLayout::OneView {
            xr::EyeVisibility::BOTH
        } else if layer_idx == 0 {
            xr::EyeVisibility::LEFT
        } else {
            xr::EyeVisibility::RIGHT
        }
    }

    /// Display aspect ratio for one eye (accounts for stereo crop).
    pub fn display_aspect(&self, settings: &VideoSettings) -> f32 {
        let (_, _, ext_w, ext_h) = image_rect(self.width, self.height, settings.stereo, 0);
        ext_w as f32 / ext_h.max(1) as f32
    }
}

impl Drop for VideoSwapchain {
    fn drop(&mut self) {
        // Drop the wgpu texture wrappers first, then forget the XR swapchain.
        // We must NOT call xrDestroySwapchain after xrEndSession (Oculus crashes).
        // The caller is responsible for dropping VideoSwapchain before the VrContext
        // so that the session is still alive when these textures are cleaned up.
        self.textures.clear();
        // SAFETY: we take the value and immediately forget it so xrDestroySwapchain
        // is never called.  xrDestroySession will clean up the child swapchain.
        let sc = unsafe { ManuallyDrop::take(&mut self.swapchain) };
        std::mem::forget(sc);
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Returns (offset_x, offset_y, extent_w, extent_h) in pixels for the portion
/// of the video image that a given eye should see.
pub fn image_rect(
    w: u32, h: u32,
    stereo: StereoLayout,
    eye: usize,          // 0 = left, 1 = right
) -> (i32, i32, i32, i32) {
    let (ox, oy, ew, eh) = match stereo {
        StereoLayout::OneView => (0, 0, w, h),
        // eye 0 = left, gets the left (or top) half
        StereoLayout::LR => if eye == 0 { (0, 0, w/2, h) } else { (w as i32/2, 0, w/2, h) },
        StereoLayout::RL => if eye == 0 { (w as i32/2, 0, w/2, h) } else { (0, 0, w/2, h) },
        StereoLayout::TB => if eye == 0 { (0, 0, w, h/2) } else { (0, h as i32/2, w, h/2) },
        StereoLayout::BT => if eye == 0 { (0, h as i32/2, w, h/2) } else { (0, 0, w, h/2) },
    };
    (ox as i32, oy as i32, ew as i32, eh as i32)
}

/// World-space pose facing the user (quad/cylinder centre at z = -dist_m, y = eye_height_m).
pub fn forward_pose(dist_m: f32, eye_height_m: f32) -> xr::Posef {
    xr::Posef {
        position:    xr::Vector3f    { x: 0.0, y: eye_height_m, z: -dist_m },
        orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
    }
}

/// Apply zoom to equirect angles: zoom=1 → full range, zoom=2 → half range.
pub fn zoom_angles(base_h: f32, base_v_up: f32, base_v_dn: f32, zoom: f32) -> (f32, f32, f32) {
    let z = zoom.max(0.1);
    (base_h / z, base_v_up / z, base_v_dn / z)
}

/// Determines whether to use the XR layer path for these settings.
/// Returns false for fisheye (always shader) and for equirect 180/360 when the
/// runtime doesn't support `XR_KHR_composition_layer_equirect2` (shader sphere fallback).
pub fn use_xr_layer(settings: &VideoSettings, has_equirect2: bool) -> bool {
    use crate::ui::settings::Projection;
    if matches!(settings.proj, Projection::Fisheye) { return false; }
    match settings.mode {
        VideoMode::View180 | VideoMode::View360 => has_equirect2,
        _ => true, // Flat2D, Curved2D, Sbs3D always via XR layer
    }
}

/// Effective VideoMode considering extension availability.
/// If an extension is unavailable, falls back to a supported mode.
pub fn effective_mode(
    settings: &VideoSettings,
    has_cylinder: bool,
    has_equirect2: bool,
) -> EffectiveLayerMode {
    match settings.mode {
        VideoMode::Flat2D | VideoMode::Sbs3D => EffectiveLayerMode::Quad,
        VideoMode::Curved2D => {
            if has_cylinder { EffectiveLayerMode::Cylinder }
            else            { EffectiveLayerMode::Quad }
        }
        VideoMode::View180 => {
            if has_equirect2 { EffectiveLayerMode::Equirect180 }
            else             { EffectiveLayerMode::Quad }
        }
        VideoMode::View360 => {
            if has_equirect2 { EffectiveLayerMode::Equirect360 }
            else             { EffectiveLayerMode::Quad }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum EffectiveLayerMode {
    Quad,
    Cylinder,
    Equirect180,
    Equirect360,
}

// ── wgpu texture wrapping ─────────────────────────────────────────────────────

/// Wrap a raw Vulkan image as a wgpu RENDER_ATTACHMENT texture.
/// Identical to `wrap_xr_image` in `vr.rs` but kept here to avoid exposing
/// that private function.
unsafe fn wrap_xr_image_render(
    device: &wgpu::Device,
    raw_img: u64,
    width:   u32,
    height:  u32,
    format:  wgpu::TextureFormat,
) -> wgpu::Texture {
    use ash::vk::Handle as _;
    let vk_image = ash::vk::Image::from_raw(raw_img);

    let hal_tex = {
        let hal_dev = unsafe {
            device
                .as_hal::<wgpu::hal::vulkan::Api>()
                .expect("wgpu not on Vulkan")
        };
        unsafe {
            hal_dev.texture_from_raw(
                vk_image,
                &wgpu::hal::TextureDescriptor {
                    label: Some("video_sc"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                    memory_flags: wgpu::hal::MemoryFlags::empty(),
                    view_formats: vec![],
                },
                None,
                wgpu::hal::vulkan::TextureMemory::External,
            )
        }
    };

    unsafe {
        device.create_texture_from_hal::<wgpu::hal::vulkan::Api>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: Some("video_sc"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            },
        )
    }
}
