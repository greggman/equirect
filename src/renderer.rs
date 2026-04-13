use std::ffi::CStr;
use std::sync::Arc;
use crate::vprintln;
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::logo::{LOGO_HEIGHT, LOGO_PNG, LOGO_WIDTH};

// ── Logo params uniform ───────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LogoParams {
    /// Fraction of NDC space that the logo occupies on each axis (letterbox /
    /// pillarbox correction).  See `compute_scale`.
    scale: [f32; 2],
    _pad:  [f32; 2],
}

/// Compute the NDC scale vector so the logo fits inside the window while
/// preserving its aspect ratio.
fn compute_scale(win_w: u32, win_h: u32) -> [f32; 2] {
    let win_aspect  = win_w  as f32 / win_h  as f32;
    let logo_aspect = LOGO_WIDTH as f32 / LOGO_HEIGHT as f32;
    if win_aspect > logo_aspect {
        // Window is wider than logo → pillarbox: logo fills full height.
        [logo_aspect / win_aspect, 1.0]
    } else {
        // Window is taller than logo → letterbox: logo fills full width.
        [1.0, win_aspect / logo_aspect]
    }
}

// ── Renderer ──────────────────────────────────────────────────────────────────

pub struct Renderer {
    window: Arc<Window>,
    pub instance: wgpu::Instance,
    pub adapter:  wgpu::Adapter,
    surface:      wgpu::Surface<'static>,
    pub device:   wgpu::Device,
    pub queue:    wgpu::Queue,
    config:       wgpu::SurfaceConfiguration,
    logo_pipeline:   wgpu::RenderPipeline,
    logo_params_buf: wgpu::Buffer,
    logo_params_bg:  wgpu::BindGroup,
    logo_tex_bg:     wgpu::BindGroup,
}

impl Renderer {
    pub fn new(window: Arc<Window>, xr_device_exts: &[&'static CStr]) -> Self {
        pollster::block_on(Self::new_async(window, xr_device_exts))
    }

    async fn new_async(window: Arc<Window>, xr_device_exts: &[&'static CStr]) -> Self {
        let size = window.inner_size();

        // Force Vulkan so wgpu and OpenXR share the same backend.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::empty(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(Arc::clone(&window)).unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .expect("No Vulkan adapter found");

        // Create the Vulkan device via wgpu's HAL, injecting the extensions that
        // the OpenXR runtime reported as required.
        let (device, queue) = {
            let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::vulkan::Api>() }
                .expect("wgpu not on Vulkan backend");

            let extra: Vec<&'static CStr> = xr_device_exts
                .iter()
                .filter(|&&e| hal_adapter.physical_device_capabilities().supports_extension(e))
                .copied()
                .collect();

            vprintln!("Renderer: injecting XR extensions: {extra:?}");

            let hal_device = unsafe {
                hal_adapter.open_with_callback(
                    wgpu::Features::empty(),
                    &wgpu::Limits::default(),
                    &wgpu::MemoryHints::default(),
                    Some(Box::new(
                        move |args: wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
                            for &ext in &extra {
                                if !args.extensions.contains(&ext) {
                                    args.extensions.push(ext);
                                }
                            }
                        },
                    )),
                )
            }
            .expect("Failed to create Vulkan device with XR extensions");

            drop(hal_adapter);

            unsafe {
                adapter.create_device_from_hal(hal_device, &wgpu::DeviceDescriptor::default())
            }
            .expect("Failed to wrap Vulkan device")
        };

        let surface_caps = surface.get_capabilities(&adapter);
        let format = surface_caps.formats[0];

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width:  size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // ── Logo texture ──────────────────────────────────────────────────────

        let logo_rgba = decode_logo_png();

        let logo_extent = wgpu::Extent3d {
            width:                 LOGO_WIDTH,
            height:                LOGO_HEIGHT,
            depth_or_array_layers: 1,
        };
        let logo_texture = device.create_texture(&wgpu::TextureDescriptor {
            label:           Some("logo_tex"),
            size:            logo_extent,
            mip_level_count: 1,
            sample_count:    1,
            dimension:       wgpu::TextureDimension::D2,
            format:          wgpu::TextureFormat::Rgba8UnormSrgb,
            usage:           wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats:    &[],
        });
        queue.write_texture(
            logo_texture.as_image_copy(),
            &logo_rgba,
            wgpu::TexelCopyBufferLayout {
                offset:         0,
                bytes_per_row:  Some(LOGO_WIDTH * 4),
                rows_per_image: Some(LOGO_HEIGHT),
            },
            logo_extent,
        );
        let logo_tex_view = logo_texture.create_view(&Default::default());
        let logo_sampler  = device.create_sampler(&wgpu::SamplerDescriptor {
            label:      Some("logo_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ── Bind group layouts ────────────────────────────────────────────────

        let params_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label:   Some("logo_params_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding:    0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty:                 wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size:   None,
                },
                count: None,
            }],
        });

        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label:   Some("logo_tex_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding:    0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled:   false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // ── Params uniform buffer & bind group ────────────────────────────────

        let scale = compute_scale(config.width, config.height);
        let params_data = LogoParams { scale, _pad: [0.0; 2] };

        let logo_params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("logo_params"),
            contents: bytemuck::bytes_of(&params_data),
            usage:    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let logo_params_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("logo_params_bg"),
            layout:  &params_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding:  0,
                resource: logo_params_buf.as_entire_binding(),
            }],
        });

        // ── Texture bind group ────────────────────────────────────────────────

        let logo_tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("logo_tex_bg"),
            layout:  &tex_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding:  0,
                    resource: wgpu::BindingResource::TextureView(&logo_tex_view),
                },
                wgpu::BindGroupEntry {
                    binding:  1,
                    resource: wgpu::BindingResource::Sampler(&logo_sampler),
                },
            ],
        });

        // ── Pipeline ──────────────────────────────────────────────────────────

        let logo_pipeline = build_logo_pipeline(&device, &params_bgl, &tex_bgl, format);

        Self {
            window,
            instance,
            adapter,
            surface,
            device,
            queue,
            config,
            logo_pipeline,
            logo_params_buf,
            logo_params_bg,
            logo_tex_bg,
        }
    }

    /// Called after the XR swapchain format is known.  The logo is only shown
    /// in the desktop window, so this is a no-op.
    pub fn prepare_for_xr(&mut self, _xr_format: wgpu::TextureFormat) {}

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.config.width  = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);

            let scale  = compute_scale(new_size.width, new_size.height);
            let params = LogoParams { scale, _pad: [0.0; 2] };
            self.queue.write_buffer(&self.logo_params_buf, 0, bytemuck::bytes_of(&params));
        }
    }

    /// Returns `false` when the surface is lost/outdated and needs to be
    /// reconfigured.
    pub fn render(&self) -> bool {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                let view = output.texture.create_view(&Default::default());
                self.draw_logo(&view);
                output.present();
                true
            }
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => false,
            _ => true, // Timeout / Occluded — skip frame
        }
    }

    fn draw_logo(&self, target: &wgpu::TextureView) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           target,
                    resolve_target: None,
                    depth_slice:    None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.logo_pipeline);
            pass.set_bind_group(0, &self.logo_params_bg, &[]);
            pass.set_bind_group(1, &self.logo_tex_bg,    &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Clear an XR eye swapchain image to fully transparent black.
    pub fn clear_xr_eye(&self, target: &wgpu::TextureView) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           target,
                    resolve_target: None,
                    depth_slice:    None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).ok();
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn window(&self) -> &Window {
        &self.window
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn decode_logo_png() -> Vec<u8> {
    let decoder = png::Decoder::new(std::io::Cursor::new(LOGO_PNG));
    let mut reader = decoder.read_info().expect("logo png: failed to read info");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("logo png: failed to decode");
    let raw  = &buf[..info.buffer_size()];
    match info.color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb  => raw.chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        other => panic!("logo png: unsupported color type {other:?}"),
    }
}

fn build_logo_pipeline(
    device:     &wgpu::Device,
    params_bgl: &wgpu::BindGroupLayout,
    tex_bgl:    &wgpu::BindGroupLayout,
    format:     wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/logo-blit.wgsl"));

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label:                Some("logo_pipeline_layout"),
        bind_group_layouts:   &[Some(params_bgl), Some(tex_bgl)],
        immediate_size:       0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label:  Some("logo_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module:              &shader,
            entry_point:         Some("vs_main"),
            buffers:             &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module:              &shader,
            entry_point:         Some("fs_main"),
            targets:             &[Some(wgpu::ColorTargetState {
                format,
                blend:      Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive:    wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample:  wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache:        None,
    })
}
