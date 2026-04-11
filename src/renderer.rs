use std::ffi::CStr;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;


#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    projection: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
}

impl CameraUniform {
    fn new(width: u32, height: u32) -> Self {
        let aspect = width as f32 / height as f32;
        let projection = glam::Mat4::perspective_rh(45_f32.to_radians(), aspect, 0.1, 100.0);
        Self {
            projection: projection.to_cols_array_2d(),
            view: glam::Mat4::IDENTITY.to_cols_array_2d(),
        }
    }

    fn from_matrices(projection: glam::Mat4, view: glam::Mat4) -> Self {
        Self {
            projection: projection.to_cols_array_2d(),
            view: view.to_cols_array_2d(),
        }
    }
}

pub struct Renderer {
    window: Arc<Window>,
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    xr_pipeline: Option<wgpu::RenderPipeline>,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    camera_bgl: wgpu::BindGroupLayout,
}

impl Renderer {
    pub fn new(window: Arc<Window>, xr_device_exts: &[&'static CStr]) -> Self {
        pollster::block_on(Self::new_async(window, xr_device_exts))
    }

    async fn new_async(window: Arc<Window>, xr_device_exts: &[&'static CStr]) -> Self {
        let size = window.inner_size();

        // Force Vulkan so wgpu and OpenXR share the same backend.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::empty(),
            ..Default::default()
        });
        let surface = instance.create_surface(Arc::clone(&window)).unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .expect("No Vulkan adapter found");

        // Create the Vulkan device via wgpu's HAL, injecting the extensions that the
        // OpenXR runtime reported as required (queried before device creation).
        let (device, queue) = {
            let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::vulkan::Api>() }
                .expect("wgpu not on Vulkan backend");

            let extra: Vec<&'static CStr> = xr_device_exts
                .iter()
                .filter(|&&e| hal_adapter.physical_device_capabilities().supports_extension(e))
                .copied()
                .collect();

            println!("Renderer: injecting XR extensions: {extra:?}");

            let hal_device = unsafe {
                hal_adapter.open_with_callback(
                    wgpu::Features::empty(),
                    &wgpu::MemoryHints::default(),
                    Some(Box::new(move |args| {
                        for &ext in &extra {
                            if !args.extensions.contains(&ext) {
                                args.extensions.push(ext);
                            }
                        }
                    })),
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
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let camera_data = CameraUniform::new(config.width, config.height);
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera"),
            contents: bytemuck::bytes_of(&camera_data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera_bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline = Self::build_pipeline_for(&device, &camera_bgl, format);

        Self {
            window,
            instance,
            adapter,
            surface,
            device,
            queue,
            config,
            pipeline,
            xr_pipeline: None,
            camera_buffer,
            camera_bind_group,
            camera_bgl,
        }
    }

    fn build_pipeline_for(
        device: &wgpu::Device,
        camera_bgl: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("shaders/hello-triangle.wgsl"));

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[camera_bgl],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertexMain"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragmentMain"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
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
        })
    }

    /// Call once after the XR swapchain format is known.
    /// Creates a second pipeline if the XR format differs from the desktop format.
    pub fn prepare_for_xr(&mut self, xr_format: wgpu::TextureFormat) {
        if xr_format != self.config.format {
            self.xr_pipeline =
                Some(Self::build_pipeline_for(&self.device, &self.camera_bgl, xr_format));
        }
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);

            let cam = CameraUniform::new(new_size.width, new_size.height);
            self.queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));
        }
    }

    pub fn render(&self) -> Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&Default::default());
        self.draw_to_view(&view, &self.pipeline);
        output.present();
        Ok(())
    }

    /// Render one XR eye to the provided texture view with the given per-eye matrices.
    pub fn render_xr_eye(
        &self,
        target: &wgpu::TextureView,
        projection: glam::Mat4,
        view_matrix: glam::Mat4,
    ) {
        let cam = CameraUniform::from_matrices(projection, view_matrix);
        self.queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));

        let pipeline = self.xr_pipeline.as_ref().unwrap_or(&self.pipeline);
        self.draw_to_view(target, pipeline);
        // Wait for the GPU to finish before the caller releases the swapchain image.
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).ok();
    }

    fn draw_to_view(&self, target: &wgpu::TextureView, pipeline: &wgpu::RenderPipeline) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.05,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.draw(0..3, 0..5);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn window(&self) -> &Window {
        &self.window
    }
}
