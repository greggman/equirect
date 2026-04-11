use std::ffi::CStr;
use openxr as xr;

use crate::renderer::Renderer;

// ── format helpers ────────────────────────────────────────────────────────────

fn vk_to_wgpu(vk: u32) -> Option<wgpu::TextureFormat> {
    match vk {
        37 => Some(wgpu::TextureFormat::Rgba8Unorm),
        43 => Some(wgpu::TextureFormat::Rgba8UnormSrgb),
        44 => Some(wgpu::TextureFormat::Bgra8Unorm),
        50 => Some(wgpu::TextureFormat::Bgra8UnormSrgb),
        _ => None,
    }
}

fn wgpu_to_vk(fmt: wgpu::TextureFormat) -> Option<u32> {
    match fmt {
        wgpu::TextureFormat::Rgba8Unorm => Some(37),
        wgpu::TextureFormat::Rgba8UnormSrgb => Some(43),
        wgpu::TextureFormat::Bgra8Unorm => Some(44),
        wgpu::TextureFormat::Bgra8UnormSrgb => Some(50),
        _ => None,
    }
}

// ── math helpers ──────────────────────────────────────────────────────────────

fn fov_to_projection(fov: xr::Fovf, near: f32, far: f32) -> glam::Mat4 {
    let tl = fov.angle_left.tan();
    let tr = fov.angle_right.tan();
    let td = fov.angle_down.tan();
    let tu = fov.angle_up.tan();
    let w = tr - tl;
    let h = tu - td;
    glam::Mat4::from_cols(
        glam::Vec4::new(2.0 / w, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, 2.0 / h, 0.0, 0.0),
        glam::Vec4::new((tr + tl) / w, (tu + td) / h, -far / (far - near), -1.0),
        glam::Vec4::new(0.0, 0.0, -(far * near) / (far - near), 0.0),
    )
}

fn pose_to_view(pose: xr::Posef) -> glam::Mat4 {
    let rot = glam::Quat::from_xyzw(
        pose.orientation.x,
        pose.orientation.y,
        pose.orientation.z,
        pose.orientation.w,
    );
    let pos = glam::Vec3::new(pose.position.x, pose.position.y, pose.position.z);
    glam::Mat4::from_rotation_translation(rot, pos).inverse()
}

// ── swapchain image wrapping ──────────────────────────────────────────────────

unsafe fn wrap_xr_image(
    device: &wgpu::Device,
    raw_img: u64,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    use ash::vk::Handle as _;

    let vk_image = ash::vk::Image::from_raw(raw_img);

    let hal_texture = {
        let hal_dev = unsafe {
            device
                .as_hal::<wgpu::hal::vulkan::Api>()
                .expect("wgpu not on Vulkan backend")
        };
        unsafe {
            hal_dev.texture_from_raw(
                vk_image,
                &wgpu::hal::TextureDescriptor {
                    label: Some("xr_swapchain"),
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
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("xr_swapchain"),
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

// ── per-eye swapchain ─────────────────────────────────────────────────────────

pub struct EyeSwapchain {
    // textures must drop before swapchain: wgpu cleans up image views while
    // the VkImages still exist; the XR runtime frees VkImages on swapchain drop.
    pub textures: Vec<wgpu::Texture>,
    pub swapchain: xr::Swapchain<xr::Vulkan>,
    pub resolution: xr::Extent2Di,
    pub format: wgpu::TextureFormat,
}

// ── pre-init: XR instance + system, before wgpu device creation ──────────────

/// Holds the OpenXR instance and system ID before the wgpu device is created.
/// Used to query required Vulkan device extensions so the renderer can enable them.
pub struct VrPreInit {
    instance: xr::Instance,
    system: xr::SystemId,
    has_legacy_vulkan: bool,
}

impl VrPreInit {
    pub fn new() -> Option<Self> {
        println!("XR: loading OpenXR...");

        let xr_entry = unsafe { xr::Entry::load() }
            .map_err(|e| eprintln!("XR: loader not found: {e}"))
            .ok()?;

        let exts = xr_entry
            .enumerate_extensions()
            .map_err(|e| eprintln!("XR: enumerate_extensions failed: {e}"))
            .ok()?;

        if !exts.khr_vulkan_enable2 {
            eprintln!("XR: runtime does not support KHR_vulkan_enable2");
            return None;
        }

        // Enable both so we can query device extensions via the legacy query function.
        let mut enabled = xr::ExtensionSet::default();
        enabled.khr_vulkan_enable2 = true;
        enabled.khr_vulkan_enable = exts.khr_vulkan_enable;

        let instance = xr_entry
            .create_instance(
                &xr::ApplicationInfo {
                    application_name: "vrust-v",
                    application_version: 1,
                    engine_name: "vrust-v",
                    engine_version: 1,
                    api_version: xr::Version::new(1, 0, 0),
                },
                &enabled,
                &[],
            )
            .map_err(|e| eprintln!("XR: create_instance failed: {e}"))
            .ok()?;

        let system = instance
            .system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
            .map_err(|e| eprintln!("XR: no HMD system found: {e}"))
            .ok()?;

        println!("XR: HMD found");

        Some(Self { instance, system, has_legacy_vulkan: exts.khr_vulkan_enable })
    }

    /// Returns the Vulkan device extensions required by the OpenXR runtime.
    /// The renderer must enable these when creating the wgpu Vulkan device.
    pub fn required_device_extensions(&self) -> Vec<&'static CStr> {
        if !self.has_legacy_vulkan {
            return Vec::new();
        }

        let ext_string = match self.instance.vulkan_legacy_device_extensions(self.system) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("XR: vulkan_legacy_device_extensions failed: {e}");
                return Vec::new();
            }
        };

        println!("XR: runtime requires device extensions: {ext_string}");

        ext_string
            .split_ascii_whitespace()
            .map(|name| {
                let cstring = std::ffi::CString::new(name).unwrap();
                // Leak is acceptable for this one-time startup allocation.
                let leaked: &'static CStr = Box::leak(cstring.into_boxed_c_str());
                leaked
            })
            .collect()
    }
}

// ── VR context ────────────────────────────────────────────────────────────────

pub struct VrContext {
    // All XR/wgpu fields are ManuallyDrop so Drop can run them in order with tracing.
    frame_stream: std::mem::ManuallyDrop<xr::FrameStream<xr::Vulkan>>,
    frame_waiter: std::mem::ManuallyDrop<xr::FrameWaiter>,
    stage: std::mem::ManuallyDrop<xr::Space>,
    pub eyes: [std::mem::ManuallyDrop<EyeSwapchain>; 2],
    session: std::mem::ManuallyDrop<xr::Session<xr::Vulkan>>,
    instance: std::mem::ManuallyDrop<xr::Instance>,
    pub swapchain_format: wgpu::TextureFormat,
    view_type: xr::ViewConfigurationType,
    blend_mode: xr::EnvironmentBlendMode,
    running: bool,
    pub should_quit: bool,
    frame_count: u64,
}

impl Drop for VrContext {
    fn drop(&mut self) {
        // SAFETY: every ManuallyDrop field is handled exactly once here.
        unsafe {
            // Drop the wgpu texture wrappers first while the Vulkan device is alive.
            for eye in &mut self.eyes {
                eye.textures.clear();
            }

            // The Oculus runtime frees swapchain/space GPU resources when
            // xrEndSession is called, so calling xrDestroySwapchain / xrDestroySpace
            // afterwards causes a double-free crash. Instead we forget those handles
            // and let xrDestroySession implicitly release all child objects, which
            // the OpenXR spec explicitly permits.
            let EyeSwapchain { swapchain: sc0, textures: t0, .. } =
                std::mem::ManuallyDrop::take(&mut self.eyes[0]);
            let EyeSwapchain { swapchain: sc1, textures: t1, .. } =
                std::mem::ManuallyDrop::take(&mut self.eyes[1]);
            drop(t0); // already empty
            drop(t1);
            std::mem::forget(sc0); // do NOT call xrDestroySwapchain
            std::mem::forget(sc1);

            let stage = std::mem::ManuallyDrop::take(&mut self.stage);
            std::mem::forget(stage); // do NOT call xrDestroySpace

            // xrDestroySession releases everything; xrDestroyInstance last.
            std::mem::ManuallyDrop::drop(&mut self.session);
            std::mem::ManuallyDrop::drop(&mut self.instance);

            // FrameStream / FrameWaiter hold no owned XR handles — just drop.
            std::mem::ManuallyDrop::drop(&mut self.frame_stream);
            std::mem::ManuallyDrop::drop(&mut self.frame_waiter);
        }
    }
}

impl VrContext {
    /// Complete XR initialisation using a pre-initialised instance/system and
    /// a fully constructed renderer whose Vulkan device has the required extensions.
    pub fn new(renderer: &Renderer, pre: VrPreInit) -> Option<Self> {
        let VrPreInit { instance: xr_instance, system, .. } = pre;

        // ── extract wgpu's Vulkan handles ─────────────────────────────────────
        let (vk_instance, vk_physical, vk_device, queue_family) = unsafe {
            use ash::vk::Handle as _;

            let hal_inst = renderer.instance
                .as_hal::<wgpu::hal::vulkan::Api>()
                .expect("wgpu not on Vulkan backend");
            let vi = hal_inst.shared_instance().raw_instance().handle().as_raw()
                as usize as *const std::ffi::c_void;

            let hal_adapt = renderer.adapter
                .as_hal::<wgpu::hal::vulkan::Api>()
                .expect("adapter not Vulkan");
            let vp = hal_adapt.raw_physical_device().as_raw() as usize
                as *const std::ffi::c_void;
            drop(hal_adapt);

            let hal_dev = renderer.device
                .as_hal::<wgpu::hal::vulkan::Api>()
                .expect("device not Vulkan");
            let vd = hal_dev.raw_device().handle().as_raw() as usize
                as *const std::ffi::c_void;
            let qf = hal_dev.queue_family_index();
            drop(hal_dev);

            (vi, vp, vd, qf)
        };

        // Required by the OpenXR spec before xrCreateSession.
        xr_instance
            .graphics_requirements::<xr::Vulkan>(system)
            .map_err(|e| eprintln!("XR: graphics_requirements failed: {e}"))
            .ok()?;

        // ── create XR session ────────────────────────────────────────────────
        let (session, frame_waiter, frame_stream) = unsafe {
            xr_instance
                .create_session::<xr::Vulkan>(
                    system,
                    &xr::vulkan::SessionCreateInfo {
                        instance: vk_instance,
                        physical_device: vk_physical,
                        device: vk_device,
                        queue_family_index: queue_family,
                        queue_index: 0,
                    },
                )
                .map_err(|e| eprintln!("XR: create_session failed: {e}"))
                .ok()?
        };

        println!("XR: session created");

        let stage = session
            .create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)
            .map_err(|e| eprintln!("XR: create_reference_space failed: {e}"))
            .ok()?;

        let view_type = xr::ViewConfigurationType::PRIMARY_STEREO;
        let view_cfgs = xr_instance
            .enumerate_view_configuration_views(system, view_type)
            .map_err(|e| eprintln!("XR: enumerate_view_configuration_views failed: {e}"))
            .ok()?;

        let blend_mode = xr_instance
            .enumerate_environment_blend_modes(system, view_type)
            .map_err(|e| eprintln!("XR: enumerate_environment_blend_modes failed: {e}"))
            .ok()?
            .into_iter()
            .find(|&m| m == xr::EnvironmentBlendMode::OPAQUE)
            .unwrap_or(xr::EnvironmentBlendMode::OPAQUE);

        // ── pick swapchain format ────────────────────────────────────────────
        let supported = session
            .enumerate_swapchain_formats()
            .map_err(|e| eprintln!("XR: enumerate_swapchain_formats failed: {e}"))
            .ok()?;

        let desktop_fmt = renderer.surface_format();

        let (xr_vk_fmt, wgpu_fmt) = match wgpu_to_vk(desktop_fmt)
            .filter(|v| supported.contains(v))
            .map(|v| (v, desktop_fmt))
            .or_else(|| supported.iter().find_map(|&f| vk_to_wgpu(f).map(|wf| (f, wf))))
        {
            Some(v) => v,
            None => {
                eprintln!("XR: no recognised format in runtime list {supported:?}");
                return None;
            }
        };

        println!("XR: swapchain format {wgpu_fmt:?}");

        // ── create per-eye swapchains ────────────────────────────────────────
        let mut eyes: [Option<EyeSwapchain>; 2] = [None, None];

        for (i, cfg) in view_cfgs.iter().enumerate().take(2) {
            let w = cfg.recommended_image_rect_width;
            let h = cfg.recommended_image_rect_height;

            let swapchain = session
                .create_swapchain(&xr::SwapchainCreateInfo {
                    create_flags: xr::SwapchainCreateFlags::EMPTY,
                    usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                        | xr::SwapchainUsageFlags::SAMPLED,
                    format: xr_vk_fmt,
                    sample_count: 1,
                    width: w,
                    height: h,
                    face_count: 1,
                    array_size: 1,
                    mip_count: 1,
                })
                .map_err(|e| eprintln!("XR: create_swapchain eye {i} failed: {e}"))
                .ok()?;

            let raw_images = swapchain
                .enumerate_images()
                .map_err(|e| eprintln!("XR: enumerate_images eye {i} failed: {e}"))
                .ok()?;

            let textures = raw_images
                .iter()
                .map(|&img| unsafe { wrap_xr_image(&renderer.device, img, w, h, wgpu_fmt) })
                .collect();

            let eye = EyeSwapchain {
                textures,
                swapchain,
                resolution: xr::Extent2Di { width: w as i32, height: h as i32 },
                format: wgpu_fmt,
            };
            println!("XR: eye {i}: {}x{} × {} images ({:?})", w, h, raw_images.len(), eye.format);
            eyes[i] = Some(eye);
        }

        let [Some(eye0), Some(eye1)] = eyes else {
            eprintln!("XR: failed to create both eye swapchains");
            return None;
        };

        println!("XR: ready — waiting for headset");

        Some(VrContext {
            instance: std::mem::ManuallyDrop::new(xr_instance),
            session: std::mem::ManuallyDrop::new(session),
            frame_waiter: std::mem::ManuallyDrop::new(frame_waiter),
            frame_stream: std::mem::ManuallyDrop::new(frame_stream),
            stage: std::mem::ManuallyDrop::new(stage),
            eyes: [
                std::mem::ManuallyDrop::new(eye0),
                std::mem::ManuallyDrop::new(eye1),
            ],
            swapchain_format: wgpu_fmt,
            view_type,
            blend_mode,
            running: false,
            should_quit: false,
            frame_count: 0,
        })
    }

    /// Ask the runtime to end the session cleanly (FOCUSED → STOPPING → EXITING).
    pub fn request_exit(&self) {
        if let Err(e) = self.session.request_exit() {
            eprintln!("XR: request_exit failed (may be OK if session not running): {e}");
        }
    }

    pub fn poll_events(&mut self) -> bool {
        let mut buf = xr::EventDataBuffer::new();
        while let Some(event) = self.instance.poll_event(&mut buf).ok().flatten() {
            use xr::Event::*;
            match event {
                SessionStateChanged(e) => {
                    println!("XR: session → {:?}", e.state());
                    match e.state() {
                        xr::SessionState::READY => {
                            self.session
                                .begin(self.view_type)
                                .expect("XR session.begin() failed");
                            self.running = true;
                            println!("XR: rendering started");
                        }
                        xr::SessionState::STOPPING => {
                            self.session.end().expect("XR session.end() failed");
                            self.running = false;
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                            self.should_quit = true;
                        }
                        _ => {}
                    }
                }
                InstanceLossPending(_) => {
                    self.should_quit = true;
                }
                _ => {}
            }
        }
        self.running
    }

    pub fn render_frame(&mut self, renderer: &Renderer) {
        if !self.poll_events() {
            return;
        }

        let frame_state = match self.frame_waiter.wait() {
            Ok(fs) => fs,
            Err(e) => {
                eprintln!("XR: frame_waiter.wait failed: {e}");
                return;
            }
        };

        if let Err(e) = self.frame_stream.begin() {
            eprintln!("XR: frame_stream.begin failed: {e}");
            return;
        }

        if !frame_state.should_render {
            self.frame_stream
                .end(frame_state.predicted_display_time, self.blend_mode, &[])
                .unwrap_or_else(|e| eprintln!("XR: frame_stream.end (no render) failed: {e}"));
            self.frame_count += 1;
            return;
        }

        if self.frame_count == 0 {
            println!("XR: first rendered frame");
        }

        let (_flags, views) = match self
            .session
            .locate_views(self.view_type, frame_state.predicted_display_time, &self.stage)
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("XR: locate_views failed: {e}");
                self.frame_stream
                    .end(frame_state.predicted_display_time, self.blend_mode, &[])
                    .ok();
                return;
            }
        };

        for eye in 0..2 {
            let img_idx = self.eyes[eye].swapchain.acquire_image().unwrap() as usize;
            self.eyes[eye].swapchain.wait_image(xr::Duration::INFINITE).unwrap();
            let tex_view = self.eyes[eye].textures[img_idx].create_view(&Default::default());
            renderer.render_xr_eye(
                &tex_view,
                fov_to_projection(views[eye].fov, 0.1, 100.0),
                pose_to_view(views[eye].pose),
            );
            self.eyes[eye].swapchain.release_image().unwrap();
        }

        let eyes = &self.eyes;
        let stage = &self.stage;
        let frame_stream = &mut self.frame_stream;

        let proj_views = [
            xr::CompositionLayerProjectionView::new()
                .pose(views[0].pose)
                .fov(views[0].fov)
                .sub_image(
                    xr::SwapchainSubImage::new()
                        .swapchain(&eyes[0].swapchain)
                        .image_array_index(0)
                        .image_rect(xr::Rect2Di {
                            offset: xr::Offset2Di { x: 0, y: 0 },
                            extent: eyes[0].resolution,
                        }),
                ),
            xr::CompositionLayerProjectionView::new()
                .pose(views[1].pose)
                .fov(views[1].fov)
                .sub_image(
                    xr::SwapchainSubImage::new()
                        .swapchain(&eyes[1].swapchain)
                        .image_array_index(0)
                        .image_rect(xr::Rect2Di {
                            offset: xr::Offset2Di { x: 0, y: 0 },
                            extent: eyes[1].resolution,
                        }),
                ),
        ];

        let layer = xr::CompositionLayerProjection::new()
            .space(stage)
            .views(&proj_views);

        frame_stream
            .end(frame_state.predicted_display_time, self.blend_mode, &[&layer])
            .unwrap_or_else(|e| eprintln!("XR: frame_stream.end failed: {e}"));

        self.frame_count += 1;
    }
}
