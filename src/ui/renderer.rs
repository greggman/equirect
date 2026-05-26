use wgpu::util::DeviceExt;

use super::draw_list::{DrawList, Vertex};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewportUniform {
    size: [f32; 2],
    _pad: [f32; 2],
}

/// Renders a `DrawList` into a 2-D offscreen texture using the UI shader.
///
/// The renderer is independent of which atlases are bound — caller passes
/// glyph and sprite texture views per frame so the same renderer can serve
/// multiple panels.
pub struct UiRenderer {
    pipeline:        wgpu::RenderPipeline,
    bgl:             wgpu::BindGroupLayout,
    sampler:         wgpu::Sampler,
    viewport_buffer: wgpu::Buffer,
    /// Reusable vertex buffer; grown lazily.
    vertex_buffer:   Option<wgpu::Buffer>,
    vertex_capacity: u64,
}

impl UiRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/ui.wgsl"));

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding:    0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size:   None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled:   false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled:   false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui_pipeline_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size:     0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:  Some("ui_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module:      &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode:    wgpu::VertexStepMode::Vertex,
                    attributes:   &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Uint32,
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module:      &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend:  Some(wgpu::BlendState {
                        // Source colours are premultiplied in the shader, so
                        // (One, OneMinusSrcAlpha) is the correct over operator.
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation:  wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation:  wgpu::BlendOperation::Add,
                        },
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let viewport_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("ui_viewport"),
            contents: bytemuck::bytes_of(&ViewportUniform { size: [1.0, 1.0], _pad: [0.0, 0.0] }),
            usage:    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Self {
            pipeline,
            bgl,
            sampler,
            viewport_buffer,
            vertex_buffer:   None,
            vertex_capacity: 0,
        }
    }

    /// Render `draw_list` into `target` at the given pixel size.  `target`
    /// must already exist; this pass clears it to transparent black before
    /// drawing.  `glyph_atlas` is sampled for `QuadKind::Glyph` quads, and
    /// `sprite_atlas` for `QuadKind::Sprite` quads.
    pub fn render(
        &mut self,
        device:        &wgpu::Device,
        queue:         &wgpu::Queue,
        target:        &wgpu::TextureView,
        target_w:      u32,
        target_h:      u32,
        draw_list:     &DrawList,
        glyph_atlas:   &wgpu::TextureView,
        sprite_atlas:  &wgpu::TextureView,
    ) {
        // ── upload viewport + vertices ─────────────────────────────────────
        queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [target_w as f32, target_h as f32],
                _pad: [0.0, 0.0],
            }),
        );

        if !draw_list.vertices.is_empty() {
            let needed = (draw_list.vertices.len() * std::mem::size_of::<Vertex>()) as u64;
            if self.vertex_buffer.is_none() || self.vertex_capacity < needed {
                let cap = needed.next_power_of_two().max(4096);
                self.vertex_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ui_vbuf"),
                    size:  cap,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.vertex_capacity = cap;
            }
            queue.write_buffer(
                self.vertex_buffer.as_ref().unwrap(),
                0,
                bytemuck::cast_slice(&draw_list.vertices),
            );
        }

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:  Some("ui_bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.viewport_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(glyph_atlas) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(sprite_atlas) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            if let Some(vb) = self.vertex_buffer.as_ref() {
                pass.set_vertex_buffer(0, vb.slice(..));
            }

            for cmd in &draw_list.cmds {
                if let Some([x, y, w, h]) = cmd.clip {
                    if w == 0 || h == 0 { continue; }
                    let cx = x.min(target_w);
                    let cy = y.min(target_h);
                    let cw = w.min(target_w.saturating_sub(cx));
                    let ch = h.min(target_h.saturating_sub(cy));
                    if cw == 0 || ch == 0 { continue; }
                    pass.set_scissor_rect(cx, cy, cw, ch);
                } else {
                    pass.set_scissor_rect(0, 0, target_w, target_h);
                }
                pass.draw(cmd.vertex_start..(cmd.vertex_start + cmd.vertex_count), 0..1);
            }
        }
        queue.submit([encoder.finish()]);
    }
}
