use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::ui::settings::{Projection, StereoLayout, VideoMode, VideoSettings};
use crate::video::texture::VideoTexture;

// ── GPU types ──────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    projection: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct VideoParams {
    uv_offset: [f32; 2],
    uv_scale:  [f32; 2],
    mode:      u32,
    inv_zoom:  f32,
    /// 0 = BGRA texture, 1 = NV12 (Y + UV planes).
    pixel_fmt: u32,
    _pad:      u32,
}

impl VideoParams {
    fn from_settings(settings: &VideoSettings, eye: usize, pixel_fmt: u32) -> Self {
        let zoom = settings.zoom.max(0.1);
        let inv_zoom = 1.0 / zoom;
        let mode = match (settings.proj, settings.mode) {
            (Projection::Fisheye,  VideoMode::View180) => 1,
            (Projection::Fisheye,  VideoMode::View360) => 2,
            (Projection::Equirect, VideoMode::View180) => 3,
            (Projection::Equirect, VideoMode::View360) => 4,
            _ => 0,
        };
        // Stereo UV crop: select which half of the video this eye should see.
        let (uv_offset, uv_scale) = match settings.stereo {
            StereoLayout::OneView => ([0.0_f32, 0.0], [1.0_f32, 1.0]),
            StereoLayout::LR => {
                if eye == 0 { ([0.0, 0.0], [0.5, 1.0]) }
                else        { ([0.5, 0.0], [0.5, 1.0]) }
            },
            StereoLayout::RL => {
                if eye == 0 { ([0.5, 0.0], [0.5, 1.0]) }
                else        { ([0.0, 0.0], [0.5, 1.0]) }
            },
            StereoLayout::TB => {
                if eye == 0 { ([0.0, 0.0], [1.0, 0.5]) }
                else        { ([0.0, 0.5], [1.0, 0.5]) }
            },
            StereoLayout::BT => {
                if eye == 0 { ([0.0, 0.5], [1.0, 0.5]) }
                else        { ([0.0, 0.0], [1.0, 0.5]) }
            },
        };
        Self {
            uv_offset,
            uv_scale,
            mode,
            inv_zoom,
            pixel_fmt,
            _pad: 0,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
}

fn build_quad(aspect: f32) -> [Vertex; 6] {
    // A flat quad 3.2 m wide, 2.5 m in front of the origin.
    // Y-up, -Z forward (OpenXR stage space).
    // Centered at eye level: cy = hh so that the vertical midpoint sits at
    // approximately standing eye height above the stage floor.
    let hw = 1.6_f32;
    let hh = hw / aspect;
    let cy = hh;      // shift up so center is at eye level
    let z  = -2.5_f32;
    [
        Vertex { position: [-hw, cy + hh, z], uv: [0.0, 0.0] },
        Vertex { position: [-hw, cy - hh, z], uv: [0.0, 1.0] },
        Vertex { position: [ hw, cy + hh, z], uv: [1.0, 0.0] },
        Vertex { position: [ hw, cy + hh, z], uv: [1.0, 0.0] },
        Vertex { position: [-hw, cy - hh, z], uv: [0.0, 1.0] },
        Vertex { position: [ hw, cy - hh, z], uv: [1.0, 1.0] },
    ]
}

// ── VideoRenderer ──────────────────────────────────────────────────────────

pub struct VideoRenderer {
    pipeline:           wgpu::RenderPipeline, // flat quad / fisheye (vertex buffer)
    equirect_pipeline:  wgpu::RenderPipeline, // fullscreen triangle, no vertex buffer
    camera_buffer:      wgpu::Buffer,
    camera_bind_group:  wgpu::BindGroup,
    texture_bgl:        wgpu::BindGroupLayout,
    sampler:            wgpu::Sampler,
    vertex_buffer:      wgpu::Buffer,
    texture_bind_group: Option<wgpu::BindGroup>,
    params_buffer:      wgpu::Buffer,
    params_bind_group:  wgpu::BindGroup,
    /// 0 = BGRA, 1 = NV12.  Set by `set_texture`.
    pixel_fmt:          u32,
}

impl VideoRenderer {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        video_width: u32,
        video_height: u32,
    ) -> Self {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("shaders/video-quad.wgsl"));

        // ── camera bind group layout (group 0) ────────────────────────────
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vr_camera_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let init_cam = CameraUniform {
            projection: Mat4::IDENTITY.to_cols_array_2d(),
            view: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vr_camera_buf"),
            contents: bytemuck::bytes_of(&init_cam),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vr_camera_bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // ── texture bind group layout (group 1) ───────────────────────────
        // binding 0: Y or BGRA texture
        // binding 1: sampler
        // binding 2: UV chroma texture (NV12 only; dummy 1×1 for BGRA)
        let texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vr_texture_bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vr_video_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ── video params bind group layout (group 2) ──────────────────────
        let params_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vr_params_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let default_params = VideoParams {
            uv_offset: [0.0, 0.0], uv_scale: [1.0, 1.0], mode: 0, inv_zoom: 1.0, pixel_fmt: 0, _pad: 0,
        };
        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vr_params_buf"),
            contents: bytemuck::bytes_of(&default_params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let params_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vr_params_bg"),
            layout: &params_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buffer.as_entire_binding(),
            }],
        });

        // ── pipelines ─────────────────────────────────────────────────────
        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&camera_bgl), Some(&texture_bgl), Some(&params_bgl)],
                immediate_size: 0,
            });

        // Flat/fisheye pipeline: uses a vertex buffer with position + uv.
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video_quad_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
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
        });

        // Equirect pipeline: fullscreen triangle, no vertex buffer.
        // The vertex shader (vs_equirect) generates positions from vertex_index
        // and passes clip-space xy as uv for ray-direction reconstruction.
        let equirect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video_equirect_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_equirect"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
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
        });

        // ── quad vertex buffer ────────────────────────────────────────────
        let aspect = video_width as f32 / video_height.max(1) as f32;
        let verts = build_quad(aspect);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("video_quad_vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Self {
            pipeline,
            equirect_pipeline,
            camera_buffer,
            camera_bind_group,
            texture_bgl,
            sampler,
            vertex_buffer,
            texture_bind_group: None,
            params_buffer,
            params_bind_group,
            pixel_fmt: 0,
        }
    }

    /// Call once (and whenever the video texture is recreated) to update the
    /// sampler binding.
    pub fn set_texture(&mut self, device: &wgpu::Device, texture: &VideoTexture) {
        self.pixel_fmt = if texture.is_nv12 { 1 } else { 0 };
        self.texture_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vr_texture_bg"),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&texture.uv_view),
                },
            ],
        }));
    }

    /// Render one eye to `target`.  Does nothing if no texture has been set yet.
    pub fn render_eye(
        &self,
        target: &wgpu::TextureView,
        projection: Mat4,
        view: Mat4,
        settings: &VideoSettings,
        eye: usize,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let Some(tex_bg) = &self.texture_bind_group else { return };

        // All direction-based modes (fisheye + equirect) use the fullscreen-triangle pipeline.
        let use_direction_shader = matches!(
            settings.mode,
            VideoMode::View180 | VideoMode::View360
        );

        let cam = CameraUniform {
            projection: projection.to_cols_array_2d(),
            view: view.to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));

        let params = VideoParams::from_settings(settings, eye, self.pixel_fmt);
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(&params));

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video_quad_pass"),
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

            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, tex_bg, &[]);
            pass.set_bind_group(2, &self.params_bind_group, &[]);

            if use_direction_shader {
                // Fullscreen triangle — no vertex buffer needed.
                // The fragment shader reconstructs ray directions from clip-space uv
                // using the camera projection and view-rotation matrices.
                pass.set_pipeline(&self.equirect_pipeline);
                pass.draw(0..3, 0..1);
            } else {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
        device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .ok();
    }
}
