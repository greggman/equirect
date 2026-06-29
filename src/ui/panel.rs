use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::input::ControllerState;
use super::assets::{PANEL_OFFSCREEN_FORMAT, UiAssets};
use super::control_bar::{ControlBarActions, ControlBarState, draw};
use super::draw_list::DrawList;
use super::ui::{Ui, UiState, CursorInput, begin_frame, PANEL_BG};

/// Pixels scrolled per frame at full thumbstick deflection.
const THUMBSTICK_SCROLL_SPEED: f32 = 40.0;

// ── GPU types for the 3-D quad pipeline ───────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    projection: [[f32; 4]; 4],
    view:       [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex { position: [f32; 3], uv: [f32; 2] }

fn panel_quad(center: Vec3, width_m: f32, height_m: f32) -> [Vertex; 6] {
    let hw = width_m / 2.0;
    let hh = height_m / 2.0;
    let (cx, cy, cz) = (center.x, center.y, center.z);
    [
        Vertex { position: [cx - hw, cy + hh, cz], uv: [0.0, 0.0] },
        Vertex { position: [cx - hw, cy - hh, cz], uv: [0.0, 1.0] },
        Vertex { position: [cx + hw, cy + hh, cz], uv: [1.0, 0.0] },
        Vertex { position: [cx + hw, cy + hh, cz], uv: [1.0, 0.0] },
        Vertex { position: [cx - hw, cy - hh, cz], uv: [0.0, 1.0] },
        Vertex { position: [cx + hw, cy - hh, cz], uv: [1.0, 1.0] },
    ]
}

// ── PanelRenderer ─────────────────────────────────────────────────────────────

pub struct PanelRenderer {
    // 3-D quad pipeline (renders the offscreen UI texture into the scene).
    pipeline:           wgpu::RenderPipeline,
    camera_buffer:      wgpu::Buffer,
    camera_bind_group:  wgpu::BindGroup,
    texture_bind_group: wgpu::BindGroup,
    #[allow(dead_code)]
    texture_bgl:        wgpu::BindGroupLayout,
    #[allow(dead_code)]
    sampler:            wgpu::Sampler,
    vertex_buffer:      wgpu::Buffer,
    #[allow(dead_code)]
    offscreen_texture:  wgpu::Texture,
    offscreen_view:     wgpu::TextureView,

    pixel_width:  u32,
    pixel_height: u32,

    // Hit-testing geometry (axis-aligned in the panel's local frame).
    panel_center: Vec3,
    panel_hw:     f32,
    panel_hh:     f32,

    // UI state (per-panel).
    ui_state:  UiState,
    draw_list: DrawList,
}

impl PanelRenderer {
    /// `xr_format` is the format the 3-D quad pass writes to (XR swapchain or
    /// desktop surface).  `center`, `width_m`, `height_m` define the world-
    /// space placement of the quad.  `pixel_width` × `pixel_height` is the
    /// resolution of the offscreen UI texture.
    pub fn new(
        device:       &wgpu::Device,
        xr_format:    wgpu::TextureFormat,
        pixel_width:  u32,
        pixel_height: u32,
        center:       Vec3,
        width_m:      f32,
        height_m:     f32,
    ) -> Self {
        // ── offscreen texture (UI lands here) ────────────────────────────────
        let offscreen_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panel_offscreen"),
            size: wgpu::Extent3d { width: pixel_width, height: pixel_height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count:    1,
            dimension:       wgpu::TextureDimension::D2,
            format:          PANEL_OFFSCREEN_FORMAT,
            usage:           wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats:    &[],
        });
        let offscreen_view = offscreen_texture.create_view(&Default::default());

        // ── 3-D quad pipeline ────────────────────────────────────────────────
        let shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/panel-quad.wgsl"));

        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("panel_camera_bgl"),
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

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panel_camera_uniform"),
            size:  std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panel_camera_bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("panel_texture_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
            label: Some("panel_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panel_texture_bg"),
            layout: &texture_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&offscreen_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panel_pipeline_layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&texture_bgl)],
            immediate_size:     0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("panel_pipeline"),
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
                    format: xr_format,
                    blend: Some(wgpu::BlendState {
                        // Premultiplied-alpha over operator.
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation:  wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::REPLACE,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive:      wgpu::PrimitiveState::default(),
            depth_stencil:  None,
            multisample:    wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache:          None,
        });

        let verts = panel_quad(center, width_m, height_m);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("panel_vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Self {
            pipeline,
            camera_buffer,
            camera_bind_group,
            texture_bind_group,
            texture_bgl,
            sampler,
            vertex_buffer,
            offscreen_texture,
            offscreen_view,
            pixel_width,
            pixel_height,
            panel_center: center,
            panel_hw:     width_m  / 2.0,
            panel_hh:     height_m / 2.0,
            ui_state:     UiState::new(),
            draw_list:    DrawList::new(),
        }
    }

    // ── ray hit-testing ───────────────────────────────────────────────────────

    fn intersect(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<(f32, Vec3)> {
        if ray_dir.z.abs() < 1e-6 { return None; }
        let t = (self.panel_center.z - ray_origin.z) / ray_dir.z;
        if t <= 0.001 { return None; }
        let hit = ray_origin + ray_dir * t;
        let dx  = hit.x - self.panel_center.x;
        let dy  = hit.y - self.panel_center.y;
        if dx.abs() > self.panel_hw || dy.abs() > self.panel_hh { return None; }
        Some((t, hit))
    }

    /// Hit-test that clamps the result to the panel bounds when the ray would
    /// otherwise miss — used while a cursor is held so dragging off the panel
    /// still updates sliders / scroll thumbs.
    fn hit_test_clamped(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<(f32, f32)> {
        if ray_dir.z.abs() < 1e-6 { return None; }
        let t = (self.panel_center.z - ray_origin.z) / ray_dir.z;
        if t <= 0.001 { return None; }
        let hit = ray_origin + ray_dir * t;
        let dx  = hit.x - self.panel_center.x;
        let dy  = hit.y - self.panel_center.y;
        let u   = ((dx + self.panel_hw) / (self.panel_hw * 2.0)).clamp(0.0, 1.0);
        let v   = 1.0 - ((dy + self.panel_hh) / (self.panel_hh * 2.0)).clamp(0.0, 1.0);
        Some((u * self.pixel_width as f32, v * self.pixel_height as f32))
    }

    /// Ray hit point in world space, or `None` when the ray misses the quad.
    /// Used by the pointer renderer to truncate the beam.
    pub fn hit_test_3d(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<Vec3> {
        self.intersect(ray_origin, ray_dir).map(|(_, p)| p)
    }

    /// Ray hit point in panel-pixel space, or `None` when the ray misses.
    pub fn hit_test(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<(f32, f32)> {
        let (_, hit) = self.intersect(ray_origin, ray_dir)?;
        let dx = hit.x - self.panel_center.x;
        let dy = hit.y - self.panel_center.y;
        let u  = (dx + self.panel_hw) / (self.panel_hw * 2.0);
        let v  = 1.0 - (dy + self.panel_hh) / (self.panel_hh * 2.0);
        Some((u * self.pixel_width as f32, v * self.pixel_height as f32))
    }

    // ── per-frame UI build ────────────────────────────────────────────────────

    /// Run a UI draw closure with two-cursor input and render the resulting
    /// draw list to the offscreen texture.  Returns whatever the closure
    /// returns — typically an `…Actions` struct.
    pub fn update_ui<R>(
        &mut self,
        device:      &wgpu::Device,
        queue:       &wgpu::Queue,
        assets:      &mut UiAssets,
        controllers: &[Option<ControllerState>; 2],
        draw_fn:     impl FnOnce(&mut Ui) -> R,
    ) -> R {
        // Build per-cursor input from each controller.
        let mut cursors: [CursorInput; 2] = [CursorInput::default(), CursorInput::default()];
        for i in 0..2 {
            if let Some(ctrl) = &controllers[i] {
                // Only compute a cursor position when the aim pose is being
                // tracked; otherwise we'd hit-test against a zero ray and
                // place a phantom cursor at the projected origin.
                let pos = if !ctrl.pose_valid {
                    None
                } else if ctrl.clicking {
                    // Clamp while held so drags continue when the ray strays off.
                    self.hit_test_clamped(ctrl.ray_origin, ctrl.ray_dir)
                        .or_else(|| self.hit_test(ctrl.ray_origin, ctrl.ray_dir))
                } else {
                    self.hit_test(ctrl.ray_origin, ctrl.ray_dir)
                };
                cursors[i] = CursorInput {
                    pos,
                    clicking: ctrl.clicking,
                    scroll_y: -ctrl.thumbstick_y * THUMBSTICK_SCROLL_SPEED,
                };
            }
        }

        // Begin a UI frame.
        self.draw_list.clear();
        let mut ui = begin_frame(
            &mut self.ui_state,
            &mut self.draw_list,
            &mut assets.atlas,
            (self.pixel_width as f32, self.pixel_height as f32),
            cursors,
        );
        ui.paint_background(PANEL_BG);
        let result = draw_fn(&mut ui);
        drop(ui);

        self.draw_list.finish();

        // Upload any new glyph rasterisations.
        assets.atlas.flush(queue);

        // Render the draw list into the offscreen UI texture.
        assets.renderer.render(
            device, queue,
            &self.offscreen_view,
            self.pixel_width, self.pixel_height,
            &self.draw_list,
            assets.atlas.view(),
            &assets.icon_view,
        );

        result
    }

    /// Convenience: build the control-bar UI.
    pub fn update(
        &mut self,
        device:      &wgpu::Device,
        queue:       &wgpu::Queue,
        assets:      &mut UiAssets,
        state:       &ControlBarState,
        controllers: &[Option<ControllerState>; 2],
    ) -> ControlBarActions {
        self.update_ui(device, queue, assets, controllers, |ui| draw(ui, state))
    }

    // ── 3-D quad render ───────────────────────────────────────────────────────

    pub fn render_eye(
        &self,
        target:      &wgpu::TextureView,
        projection:  Mat4,
        view:        Mat4,
        device:      &wgpu::Device,
        queue:       &wgpu::Queue,
    ) {
        let cam = CameraUniform {
            projection: projection.to_cols_array_2d(),
            view:       view.to_cols_array_2d(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&cam));

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("panel_quad_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group,  &[]);
            pass.set_bind_group(1, &self.texture_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);
        // No CPU wait: GPU sync is handled by the swapchain protocol.
        let _ = device;
    }
}
