use crate::video::decoder::{VideoFormat, VideoFrame};

/// GPU-side texture(s) for one video frame.
///
/// BGRA mode: one `Bgra8UnormSrgb` texture.
/// NV12 mode: a `R8Unorm` luma (Y) texture + a `Rg8Unorm` chroma (UV) texture.
///
/// A 1×1 dummy `Rg8Unorm` texture is always allocated so both bind-group slots
/// can be populated regardless of the active mode.
pub struct VideoTexture {
    texture:     wgpu::Texture,      // Y or BGRA (kept alive)
    pub view:    wgpu::TextureView,
    uv_texture:  wgpu::Texture,      // UV or dummy 1×1 (kept alive)
    pub uv_view: wgpu::TextureView,
    pub width:   u32,
    pub height:  u32,
    pub is_nv12: bool,
}

impl VideoTexture {
    pub fn new(device: &wgpu::Device, width: u32, height: u32, is_nv12: bool) -> Self {
        let (texture, view, uv_texture, uv_view) = if is_nv12 {
            // ── NV12: Y plane (R8Unorm, w×h) + UV plane (Rg8Unorm, w/2 × h/2) ──
            let y_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video_y"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let y_view = y_tex.create_view(&Default::default());

            let uv_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video_uv"),
                size: wgpu::Extent3d {
                    width:  (width  / 2).max(1),
                    height: (height / 2).max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rg8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let uv_view = uv_tex.create_view(&Default::default());

            (y_tex, y_view, uv_tex, uv_view)
        } else {
            // ── BGRA: single Bgra8UnormSrgb texture + dummy 1×1 UV ────────────
            let bgra_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video_frame"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // sRGB so the GPU linearises on sampling (one correct gamma pass).
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let bgra_view = bgra_tex.create_view(&Default::default());

            // Dummy UV slot — never sampled in the BGRA path.
            let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video_uv_dummy"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rg8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let dummy_view = dummy_tex.create_view(&Default::default());

            (bgra_tex, bgra_view, dummy_tex, dummy_view)
        };

        Self { texture, view, uv_texture, uv_view, width, height, is_nv12 }
    }

    /// Upload a decoded frame to GPU texture(s).
    pub fn upload(&self, queue: &wgpu::Queue, frame: &VideoFrame) {
        match &frame.format {
            VideoFormat::Bgra => {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &frame.data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(self.width * 4),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
                );
            }
            VideoFormat::Nv12 { stride, uv_offset } => {
                let stride = *stride as u32;
                let y_bytes = (self.height * stride) as usize;
                let y_data  = &frame.data[..y_bytes];
                let uv_data = &frame.data[*uv_offset..];

                // Y plane — R8Unorm, width × height
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    y_data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),          // stride ≥ width
                        rows_per_image: None,
                    },
                    wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
                );

                // UV plane — Rg8Unorm, (width/2) × (height/2)
                // Each row is `stride` bytes wide (same stride as Y); actual data
                // is (width/2) Rg8 texels = width bytes, padding fills the rest.
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.uv_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    uv_data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),          // stride bytes per chroma row
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width:  self.width  / 2,
                        height: self.height / 2,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
    }
}
