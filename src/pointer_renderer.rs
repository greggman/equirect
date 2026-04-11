use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use wgpu::util::DeviceExt;

use crate::input::ControllerState;

// ── Geometry constants ─────────────────────────────────────────────────────

const BEAM_RADIUS:   f32   = 0.003; // 3 mm
const SPHERE_RADIUS: f32   = 0.006; // 6 mm — 2× the beam
const BEAM_LENGTH:   f32   = 3.0;   // metres when no surface is hit
const TUBE_SEGS:     usize = 8;     // circumference divisions of the tube
const SPHERE_LON:    usize = 8;     // longitude divisions of the sphere
const SPHERE_LAT:    usize = 6;     // latitude  divisions of the sphere

// Two triangle fans per tube segment, two per sphere quad.
const TUBE_VERTS:   usize = TUBE_SEGS   * 6;
const SPHERE_VERTS: usize = SPHERE_LON  * SPHERE_LAT * 6;
const MAX_VERTS:    usize = 2 * (TUBE_VERTS + SPHERE_VERTS); // both hands

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

/// Left = cyan, right = magenta.
const COLORS: [[f32; 4]; 2] = [
    [0.0, 1.0, 1.0, 0.9],
    [1.0, 0.0, 1.0, 0.9],
];

// ── Geometry helpers ───────────────────────────────────────────────────────

/// Append a tube (open cylinder, no end-caps) to `out`.
///
/// The unit cylinder occupies x²+y²≤1, z∈[0,1].  The `model` matrix maps it
/// to world space: typically
/// `translate(origin) × rotate(Z→dir) × scale(radius, radius, length)`.
fn append_tube(model: &Mat4, color: [f32; 4], out: &mut Vec<Vertex>) {
    let pt = |x: f32, y: f32, z: f32| -> Vertex {
        Vertex { position: model.transform_point3(Vec3::new(x, y, z)).into(), color }
    };
    for seg in 0..TUBE_SEGS {
        let a0 = std::f32::consts::TAU * seg       as f32 / TUBE_SEGS as f32;
        let a1 = std::f32::consts::TAU * (seg + 1) as f32 / TUBE_SEGS as f32;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        // Quad: (c0,s0,0)→(c0,s0,1)→(c1,s1,0)  +  (c1,s1,0)→(c0,s0,1)→(c1,s1,1)
        out.extend([
            pt(c0, s0, 0.0), pt(c0, s0, 1.0), pt(c1, s1, 0.0),
            pt(c1, s1, 0.0), pt(c0, s0, 1.0), pt(c1, s1, 1.0),
        ]);
    }
}

/// Append a UV-sphere to `out`.
///
/// The unit sphere is centred at the origin with radius 1.  `model` is
/// typically `translate(centre) × scale(radius)`.
fn append_sphere(model: &Mat4, color: [f32; 4], out: &mut Vec<Vertex>) {
    let pt = |x: f32, y: f32, z: f32| -> Vertex {
        Vertex { position: model.transform_point3(Vec3::new(x, y, z)).into(), color }
    };
    for lat in 0..SPHERE_LAT {
        let theta0 = std::f32::consts::PI * (lat       as f32 / SPHERE_LAT as f32 - 0.5);
        let theta1 = std::f32::consts::PI * ((lat + 1) as f32 / SPHERE_LAT as f32 - 0.5);
        let (st0, ct0) = theta0.sin_cos();
        let (st1, ct1) = theta1.sin_cos();
        for lon in 0..SPHERE_LON {
            let phi0 = std::f32::consts::TAU * lon       as f32 / SPHERE_LON as f32;
            let phi1 = std::f32::consts::TAU * (lon + 1) as f32 / SPHERE_LON as f32;
            let (sp0, cp0) = phi0.sin_cos();
            let (sp1, cp1) = phi1.sin_cos();
            // Four corners of the lat-lon quad.
            let v00 = pt(ct0 * cp0, ct0 * sp0, st0);
            let v01 = pt(ct0 * cp1, ct0 * sp1, st0);
            let v10 = pt(ct1 * cp0, ct1 * sp0, st1);
            let v11 = pt(ct1 * cp1, ct1 * sp1, st1);
            out.extend([v00, v10, v01, v01, v10, v11]);
        }
    }
}

// ── PointerRenderer ─────────────────────────────────────────────────────────

pub struct PointerRenderer {
    pipeline:          wgpu::RenderPipeline,
    vertex_buffer:     wgpu::Buffer,
    camera_buffer:     wgpu::Buffer,
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
            label:              None,
            bind_group_layouts: &[Some(&camera_bgl)],
            immediate_size:     0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:  Some("pointer_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module:      &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode:    wgpu::VertexStepMode::Vertex,
                    attributes:   &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module:      &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend:      Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology:  wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // thin geometry; render both faces
                ..Default::default()
            },
            depth_stencil:  None,
            multisample:    wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache:          None,
        });

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label:              Some("ptr_vbuf"),
            size:               (std::mem::size_of::<Vertex>() * MAX_VERTS) as u64,
            usage:              wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, vertex_buffer, camera_buffer, camera_bind_group }
    }

    /// Render pointer beams (and hit-spheres) for one XR eye.
    ///
    /// `hits[i]` is the world-space hit point for controller `i`, or `None` if
    /// that controller's ray doesn't currently touch a surface.
    pub fn render_eye(
        &self,
        target:      &wgpu::TextureView,
        projection:  Mat4,
        view:        Mat4,
        controllers: &[Option<ControllerState>; 2],
        hits:        &[Option<Vec3>; 2],
        device:      &wgpu::Device,
        queue:       &wgpu::Queue,
    ) {
        let mut verts: Vec<Vertex> = Vec::with_capacity(MAX_VERTS);

        for (i, ctrl) in controllers.iter().enumerate() {
            let Some(ctrl) = ctrl else { continue };
            let color = COLORS[i];

            // Length of the beam: to the hit surface, or the default free-air length.
            let length = hits[i]
                .map(|h| (h - ctrl.ray_origin).length())
                .unwrap_or(BEAM_LENGTH)
                .max(0.001); // guard against degenerate zero-length beam

            // ── Tube ──────────────────────────────────────────────────────
            // Model: unit cylinder (z=0..1, r=1) → scaled → rotated → translated.
            let tube_model =
                Mat4::from_translation(ctrl.ray_origin)
                * Mat4::from_quat(Quat::from_rotation_arc(Vec3::Z, ctrl.ray_dir))
                * Mat4::from_scale(Vec3::new(BEAM_RADIUS, BEAM_RADIUS, length));

            append_tube(&tube_model, color, &mut verts);

            // ── Hit sphere ────────────────────────────────────────────────
            if let Some(hit) = hits[i] {
                let sphere_model =
                    Mat4::from_translation(hit)
                    * Mat4::from_scale(Vec3::splat(SPHERE_RADIUS));
                append_sphere(&sphere_model, color, &mut verts);
            }
        }

        if verts.is_empty() {
            return;
        }

        let cam = CameraUniform {
            projection: projection.to_cols_array_2d(),
            view:       view.to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&verts));

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
            pass.draw(0..verts.len() as u32, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}
