use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::input::ControllerState;
use super::control_bar::{ControlBarActions, ControlBarState, draw};

/// Pixels scrolled per frame when the thumbstick is fully deflected.
const THUMBSTICK_SCROLL_SPEED: f32 = 40.0;

// ── GPU types (identical to video_renderer) ────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    projection: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
}

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

// ── PanelRenderer ──────────────────────────────────────────────────────────

pub struct PanelRenderer {
    // egui
    egui_ctx: egui::Context,
    egui_renderer: egui_wgpu::Renderer,
    offscreen_view: wgpu::TextureView,
    pixel_width: u32,
    pixel_height: u32,
    // quad pipeline
    pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    texture_bind_group: wgpu::BindGroup,
    #[allow(dead_code)]
    texture_bgl: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    sampler: wgpu::Sampler,
    vertex_buffer: wgpu::Buffer,
    #[allow(dead_code)]
    offscreen_texture: wgpu::Texture, // kept alive alongside bind group
    // hit-testing geometry (panel is always axis-aligned in stage space)
    panel_center: Vec3,
    panel_hw: f32,   // half-width  in metres
    panel_hh: f32,   // half-height in metres
    // pointer interaction state
    cursor_pos:    Option<egui::Pos2>,
    prev_clicking: bool,
    /// Where the button was first pressed; used to detect drags vs clicks.
    press_pos:     Option<egui::Pos2>,
}

impl PanelRenderer {
    /// `xr_format` is the format of the XR eye swapchain textures we'll render onto.
    /// `center` and `width_m`/`height_m` set the panel's world-space position and size.
    pub fn new(
        device: &wgpu::Device,
        xr_format: wgpu::TextureFormat,
        pixel_width: u32,
        pixel_height: u32,
        center: Vec3,
        width_m: f32,
        height_m: f32,
    ) -> Self {
        // ── offscreen texture (Rgba8Unorm — egui renders here) ─────────────
        let offscreen_fmt = wgpu::TextureFormat::Rgba8Unorm;
        let offscreen_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panel_offscreen"),
            size: wgpu::Extent3d { width: pixel_width, height: pixel_height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: offscreen_fmt,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let offscreen_view = offscreen_texture.create_view(&Default::default());

        // ── egui ──────────────────────────────────────────────────────────
        let egui_ctx = egui::Context::default();
        // VR controllers have natural hand tremor; the default 6px threshold is too
        // tight and causes clicks to be silently dropped.  200px is generous enough
        // to survive any realistic controller movement while pressing a button.
        egui_ctx.options_mut(|o| o.input_options.max_click_dist = 200.0);
        let egui_renderer = egui_wgpu::Renderer::new(device, offscreen_fmt, egui_wgpu::RendererOptions::default());

        // ── camera bind group layout ──────────────────────────────────────
        let shader = device.create_shader_module(
            wgpu::include_wgsl!("../shaders/panel-quad.wgsl"),
        );
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
        let init_cam = CameraUniform {
            projection: Mat4::IDENTITY.to_cols_array_2d(),
            view: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("panel_camera_buf"),
            contents: bytemuck::bytes_of(&init_cam),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panel_camera_bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // ── texture bind group layout ─────────────────────────────────────
        let texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("panel_texture_bgl"),
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
            label: Some("panel_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panel_texture_bg"),
            layout: &texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&offscreen_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        // ── pipeline (same shader as video quad) ──────────────────────────
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&camera_bgl), Some(&texture_bgl)],
            immediate_size: 0,
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
                    // Premultiplied alpha blend so the panel composites over video.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::REPLACE,
                    }),
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

        // ── vertex buffer ─────────────────────────────────────────────────
        let verts = panel_quad(center, width_m, height_m);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("panel_vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Self {
            egui_ctx,
            egui_renderer,
            offscreen_view,
            pixel_width,
            pixel_height,
            pipeline,
            camera_buffer,
            camera_bind_group,
            texture_bind_group,
            texture_bgl,
            sampler,
            vertex_buffer,
            offscreen_texture,
            panel_center: center,
            panel_hw: width_m / 2.0,
            panel_hh: height_m / 2.0,
            cursor_pos: None,
            prev_clicking: false,
            press_pos: None,
        }
    }

    /// Internal: test a world-space ray against the panel plane.
    /// Returns `(t, hit_point)` if the ray hits the quad, `None` otherwise.
    fn intersect(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<(f32, Vec3)> {
        // Panel is in the plane z = panel_center.z, facing +Z toward the viewer.
        if ray_dir.z.abs() < 1e-6 {
            return None; // ray parallel to panel
        }
        let t = (self.panel_center.z - ray_origin.z) / ray_dir.z;
        if t <= 0.001 {
            return None; // panel is behind or at the controller
        }
        let hit = ray_origin + ray_dir * t;
        let dx = hit.x - self.panel_center.x;
        let dy = hit.y - self.panel_center.y;
        if dx.abs() > self.panel_hw || dy.abs() > self.panel_hh {
            return None; // outside the quad
        }
        Some((t, hit))
    }

    /// Like `hit_test` but clamps the result to the panel bounds instead of
    /// returning `None` when the ray misses.  Used for pointer capture so the
    /// cursor continues to track along the panel edge during a drag.
    /// Returns `None` only if the ray is parallel to (or behind) the panel.
    fn hit_test_clamped(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<egui::Pos2> {
        if ray_dir.z.abs() < 1e-6 { return None; }
        let t = (self.panel_center.z - ray_origin.z) / ray_dir.z;
        if t <= 0.001 { return None; }
        let hit = ray_origin + ray_dir * t;
        let dx = hit.x - self.panel_center.x;
        let dy = hit.y - self.panel_center.y;
        let u = ((dx + self.panel_hw) / (self.panel_hw * 2.0)).clamp(0.0, 1.0);
        let v = 1.0 - ((dy + self.panel_hh) / (self.panel_hh * 2.0)).clamp(0.0, 1.0);
        Some(egui::Pos2 {
            x: u * self.pixel_width  as f32,
            y: v * self.pixel_height as f32,
        })
    }

    /// Returns the 3-D world-space hit point on the panel, or `None`.
    pub fn hit_test_3d(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<Vec3> {
        self.intersect(ray_origin, ray_dir).map(|(_, p)| p)
    }

    /// Returns the egui pixel coordinate of the intersection, or `None`.
    pub fn hit_test(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<egui::Pos2> {
        let (_, hit) = self.intersect(ray_origin, ray_dir)?;
        let dx = hit.x - self.panel_center.x;
        let dy = hit.y - self.panel_center.y;
        let u = (dx + self.panel_hw) / (self.panel_hw * 2.0);
        let v = 1.0 - (dy + self.panel_hh) / (self.panel_hh * 2.0);
        Some(egui::Pos2 {
            x: u * self.pixel_width as f32,
            y: v * self.pixel_height as f32,
        })
    }

    /// Generic update: runs `draw_fn` inside the egui panel, renders the result
    /// to the offscreen texture, and returns whatever `draw_fn` returns.
    ///
    /// `draw_fn(ui, interaction)` — `interaction` is `Some((press_pos, release_pos))`
    /// on the single frame the controller select button is released.  Both positions
    /// are egui pixel coordinates from our own hit-test, not from egui's internal
    /// state.  A widget should activate only if both positions fall within its rect,
    /// implementing true pointer capture: the widget under the press owns the
    /// interaction until release.
    pub fn update_ui<T: Default>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        controllers: &[Option<ControllerState>; 2],
        draw_fn: impl FnOnce(&mut egui::Ui, Option<(egui::Pos2, egui::Pos2)>) -> T,
    ) -> T {
        // ── Build pointer events from controller hit-tests ─────────────────
        let mut events: Vec<egui::Event> = Vec::new();
        let mut new_pos: Option<egui::Pos2> = None;
        let mut new_clicking = false;
        let mut thumbstick_y = 0.0_f32;

        for ctrl in controllers.iter().flatten() {
            if let Some(hit) = self.hit_test(ctrl.ray_origin, ctrl.ray_dir) {
                new_pos      = Some(hit);
                new_clicking = ctrl.clicking;
            }
            // Largest-magnitude thumbstick across both controllers drives scroll.
            if ctrl.thumbstick_y.abs() > thumbstick_y.abs() {
                thumbstick_y = ctrl.thumbstick_y;
            }
        }

        // Pointer capture: while the button is held and the ray has left the
        // panel, project onto the infinite panel plane and clamp to bounds.
        // This keeps sliders and scrollbars tracking along the edge rather than
        // freezing at the last known position.
        if self.prev_clicking && new_pos.is_none() {
            new_pos = controllers.iter().flatten()
                .find_map(|c| self.hit_test_clamped(c.ray_origin, c.ray_dir))
                .or(self.cursor_pos);            // fallback: ray parallel to panel
            new_clicking = controllers.iter().flatten().any(|c| c.clicking);
        }

        // Record where the button first went down; this is the widget that owns
        // the pointer capture and is the only widget allowed to fire on release.
        if new_clicking && !self.prev_clicking {
            self.press_pos = new_pos;
        }

        // `interaction`: on the single frame the button is released, `Some((press_pos,
        // release_pos))` — both in egui pixel space from our own hit-test.  Draw
        // functions use this to implement true pointer capture: a widget fires if
        // and only if both positions fall within its rect.
        let interaction: Option<(egui::Pos2, egui::Pos2)> =
            if !new_clicking && self.prev_clicking {
                // `new_pos` is authoritative release position from this frame's hit-test.
                // Fall back to press_pos if the ray left the panel on the same frame
                // the button was released (extremely unlikely but safe).
                self.press_pos.zip(new_pos.or(self.press_pos))
            } else {
                None
            };

        if !new_clicking {
            self.press_pos = None;
        }

        match (self.cursor_pos, new_pos) {
            (_, Some(pos)) => {
                events.push(egui::Event::PointerMoved(pos));
                // Press / release events are still injected so the Slider can drag.
                if new_clicking && !self.prev_clicking {
                    events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                if !new_clicking && self.prev_clicking {
                    events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                // Thumbstick scroll — only when the pointer is over the panel so
                // scroll areas know they're being targeted.
                if thumbstick_y.abs() > 0.05 {
                    events.push(egui::Event::MouseWheel {
                        unit:      egui::MouseWheelUnit::Point,
                        delta:     egui::Vec2::new(0.0, thumbstick_y * THUMBSTICK_SCROLL_SPEED),
                        phase:     egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            }
            (Some(_), None) => {
                events.push(egui::Event::PointerGone);
            }
            (None, None) => {}
        }

        self.cursor_pos    = new_pos;
        self.prev_clicking = new_clicking;

        // ── Run egui ──────────────────────────────────────────────────────
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(self.pixel_width as f32, self.pixel_height as f32),
            )),
            events,
            ..Default::default()
        };

        let mut result = T::default();
        // `run_ui` requires FnMut; wrap the FnOnce in an Option so we can `take` it once.
        let mut draw_fn_opt = Some(draw_fn);

        let full_output = self.egui_ctx.run_ui(raw_input, |ctx| {
            ctx.set_visuals(egui::Visuals::dark());

            #[allow(deprecated)]
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(15, 15, 15, 220))
                        .inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 6 }),
                )
                .show(ctx, |ui| {
                    if let Some(f) = draw_fn_opt.take() {
                        result = f(ui, interaction);
                    }
                });
        });

        let primitives = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        let screen_desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.pixel_width, self.pixel_height],
            pixels_per_point: 1.0,
        };

        let mut encoder = device.create_command_encoder(&Default::default());

        // Upload any new fonts / images egui wants on the GPU.
        for (id, delta) in &full_output.textures_delta.set {
            self.egui_renderer.update_texture(device, queue, *id, delta);
        }
        self.egui_renderer.update_buffers(device, queue, &mut encoder, &primitives, &screen_desc);

        // Render egui into the offscreen texture.
        // `.forget_lifetime()` erases the borrow of `encoder` so egui_wgpu's
        // `render` method (which requires `RenderPass<'static>`) can accept it.
        // Safety: we drop the pass before calling `encoder.finish()` below.
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui_offscreen"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.offscreen_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, &primitives, &screen_desc);
        }

        // Free textures egui no longer needs.
        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        queue.submit([encoder.finish()]);

        result
    }

    /// Convenience wrapper: run the control-bar draw function.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        state: &ControlBarState,
        controllers: &[Option<ControllerState>; 2],
    ) -> ControlBarActions {
        self.update_ui(device, queue, controllers, |ui, interaction| {
            draw(ui, state, interaction)
        })
    }

    /// Render the panel quad into one XR eye.  Uses `LoadOp::Load` so the video
    /// rendered before this call is preserved and the panel composites on top.
    pub fn render_eye(
        &self,
        target: &wgpu::TextureView,
        projection: Mat4,
        view: Mat4,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let cam = CameraUniform {
            projection: projection.to_cols_array_2d(),
            view: view.to_cols_array_2d(),
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
                        // Load existing content (video) so we composite on top.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.texture_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);
        device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).ok();
    }
}
