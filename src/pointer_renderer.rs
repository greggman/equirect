use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::input::ControllerState;

// ── GPU types ──────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    projection: [[f32; 4]; 4],
    view:       [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    color:    [f32; 4],
}

// Left = cyan, right = magenta.
const COLORS: [[f32; 4]; 2] = [
    [0.0, 1.0, 1.0, 0.9],
    [1.0, 0.0, 1.0, 0.9],
];

const BEAM_LENGTH: f32 = 3.0;

// ── PointerRenderer ─────────────────────────────────────────────────────────

pub struct PointerRenderer {
    pipeline:         wgpu::RenderPipeline,
    /// Dynamic vertex buffer — up to 4 vertices (2 per hand).
    vertex_buffer:    wgpu::Buffer,
    camera_buffer:    wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
}

impl PointerRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device
            .create_shader_module(wgpu::include_wgsl!("shaders/pointer.wgsl"));

        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ptr_camera_bgl"),
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

        let init_cam = CameraUniform {
            projection: Mat4::IDENTITY.to_cols_array_2d(),
            view:       Mat4::IDENTITY.to_cols_array_2d(),
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("ptr_camera_buf"),
            contents: bytemuck::bytes_of(&init_cam),
            usage:    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("ptr_camera_bg"),
            layout:  &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding:  0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:               None,
            bind_group_layouts:  &[Some(&camera_bgl)],
            immediate_size:      0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:  Some("pointer_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module:               &shader,
                entry_point:          Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode:    wgpu::VertexStepMode::Vertex,
                    attributes:   &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module:       &shader,
                entry_point:  Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend:      Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample:   wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // 4 vertices max (start + end for each of the two hands).
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label:              Some("ptr_vbuf"),
            size:               (std::mem::size_of::<Vertex>() * 4) as u64,
            usage:              wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, vertex_buffer, camera_buffer, camera_bind_group }
    }

    /// Render the pointer beams for one XR eye.  Uses `LoadOp::Load` to
    /// composite on top of whatever was rendered before.
    pub fn render_eye(
        &self,
        target:      &wgpu::TextureView,
        projection:  Mat4,
        view:        Mat4,
        controllers: &[Option<ControllerState>; 2],
        device:      &wgpu::Device,
        queue:       &wgpu::Queue,
    ) {
        // Build line-list vertices: two vertices per active controller.
        let mut verts: [Vertex; 4] = [Vertex { position: [0.0; 3], color: [0.0; 4] }; 4];
        let mut count: u32 = 0;

        for (i, ctrl) in controllers.iter().enumerate() {
            if let Some(ctrl) = ctrl {
                let end = ctrl.ray_origin + ctrl.ray_dir * BEAM_LENGTH;
                verts[count as usize]     = Vertex { position: ctrl.ray_origin.into(), color: COLORS[i] };
                verts[count as usize + 1] = Vertex { position: end.into(),             color: COLORS[i] };
                count += 2;
            }
        }

        if count == 0 {
            return;
        }

        let cam = CameraUniform {
            projection: projection.to_cols_array_2d(),
            view:       view.to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));
        queue.write_buffer(
            &self.vertex_buffer,
            0,
            bytemuck::cast_slice(&verts[..count as usize]),
        );

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pointer_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           target,
                    resolve_target: None,
                    depth_slice:    None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..count, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}
