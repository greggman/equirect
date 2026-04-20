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

    /// Import a D3D11-shared BGRA texture directly into wgpu via Vulkan external memory.
    /// Returns `None` if the Vulkan backend is unavailable or import fails.
    pub fn new_gpu(
        device:     &wgpu::Device,
        instance:   &wgpu::Instance,
        adapter:    &wgpu::Adapter,
        width:      u32,
        height:     u32,
        handle_raw: isize,
    ) -> Option<Self> {
        use ash::vk;
        use ash::vk::Handle as _;

        // ── query physical-device memory properties ───────────────────────────
        let mem_props = unsafe {
            let hal_inst = instance.as_hal::<wgpu::hal::vulkan::Api>()?;
            let raw_inst = hal_inst.shared_instance().raw_instance();
            let hal_adapt = adapter.as_hal::<wgpu::hal::vulkan::Api>()?;
            let phys = hal_adapt.raw_physical_device();
            raw_inst.get_physical_device_memory_properties(phys)
        };

        // ── create VkImage + import D3D11 NT handle ───────────────────────────
        // All VK handles are stored as u64 (Send+Sync) so the closure below compiles.
        let (img_raw, mem_raw, destroy_fn, free_fn, dev_raw): (
            u64, u64,
            vk::PFN_vkDestroyImage,
            vk::PFN_vkFreeMemory,
            u64,
        ) = unsafe {
            let hal_dev = device.as_hal::<wgpu::hal::vulkan::Api>()?;
            let raw_dev = hal_dev.raw_device();

            let mut ext_img_info = vk::ExternalMemoryImageCreateInfo {
                handle_types: vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE,
                ..Default::default()
            };
            let image_info = vk::ImageCreateInfo {
                p_next: &mut ext_img_info as *mut _ as *const _,
                image_type: vk::ImageType::TYPE_2D,
                format: vk::Format::B8G8R8A8_SRGB,
                extent: vk::Extent3D { width, height, depth: 1 },
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
                tiling: vk::ImageTiling::OPTIMAL,
                usage: vk::ImageUsageFlags::SAMPLED,
                sharing_mode: vk::SharingMode::EXCLUSIVE,
                initial_layout: vk::ImageLayout::UNDEFINED,
                ..Default::default()
            };
            let image = match raw_dev.create_image(&image_info, None) {
                Ok(i) => i,
                Err(e) => { eprintln!("Video: vkCreateImage: {e}"); return None; }
            };

            let mem_req = raw_dev.get_image_memory_requirements(image);
            let mem_type_idx = match (0..mem_props.memory_type_count).find(|&i| {
                let flags = mem_props.memory_types[i as usize].property_flags;
                (mem_req.memory_type_bits & (1u32 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            }) {
                Some(i) => i,
                None => {
                    raw_dev.destroy_image(image, None);
                    eprintln!("Video: no DEVICE_LOCAL memory type for D3D11 import");
                    return None;
                }
            };

            let mut dedicated = vk::MemoryDedicatedAllocateInfo {
                image,
                ..Default::default()
            };
            let mut import_info = vk::ImportMemoryWin32HandleInfoKHR {
                p_next: &mut dedicated as *mut _ as *const _,
                handle_type: vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE,
                handle: handle_raw,
                ..Default::default()
            };
            let alloc_info = vk::MemoryAllocateInfo {
                p_next: &mut import_info as *mut _ as *const _,
                allocation_size: 0,
                memory_type_index: mem_type_idx,
                ..Default::default()
            };
            let memory = match raw_dev.allocate_memory(&alloc_info, None) {
                Ok(m) => m,
                Err(e) => {
                    raw_dev.destroy_image(image, None);
                    eprintln!("Video: vkAllocateMemory (D3D11 import): {e}");
                    return None;
                }
            };
            if let Err(e) = raw_dev.bind_image_memory(image, memory, 0) {
                raw_dev.free_memory(memory, None);
                raw_dev.destroy_image(image, None);
                eprintln!("Video: vkBindImageMemory: {e}");
                return None;
            }

            (
                image.as_raw(),
                memory.as_raw(),
                raw_dev.fp_v1_0().destroy_image,
                raw_dev.fp_v1_0().free_memory,
                raw_dev.handle().as_raw(),
            )
        };

        // Drop callback: u64 handles + fn ptrs are Send+Sync, so this closure is too.
        let drop_callback: wgpu::hal::DropCallback = Box::new(move || unsafe {
            destroy_fn(vk::Device::from_raw(dev_raw), vk::Image::from_raw(img_raw), std::ptr::null());
            free_fn(vk::Device::from_raw(dev_raw), vk::DeviceMemory::from_raw(mem_raw), std::ptr::null());
        });

        // ── wrap VkImage in a wgpu texture ────────────────────────────────────
        let vk_image = vk::Image::from_raw(img_raw);
        let hal_texture = unsafe {
            let hal_dev = device.as_hal::<wgpu::hal::vulkan::Api>()?;
            hal_dev.texture_from_raw(
                vk_image,
                &wgpu::hal::TextureDescriptor {
                    label: Some("d3d11_shared_bgra"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    usage: wgpu::wgt::TextureUses::RESOURCE,
                    memory_flags: wgpu::hal::MemoryFlags::empty(),
                    view_formats: vec![],
                },
                Some(drop_callback),
                wgpu::hal::vulkan::TextureMemory::External,
            )
        };

        let wgpu_texture = unsafe {
            device.create_texture_from_hal::<wgpu::hal::vulkan::Api>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("d3d11_shared_bgra"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };

        let view = wgpu_texture.create_view(&Default::default());
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
        let uv_view = dummy_tex.create_view(&Default::default());

        crate::vprintln!("Video: GPU texture ({width}×{height} Bgra8UnormSrgb) imported from D3D11");
        Some(Self { texture: wgpu_texture, view, uv_texture: dummy_tex, uv_view, width, height, is_nv12: false })
    }

    /// Upload a decoded frame to GPU texture(s).
    pub fn upload(&self, queue: &wgpu::Queue, frame: &VideoFrame) {
        match &frame.format {
            VideoFormat::GpuReady => {}   // D3D11VP already wrote to the shared texture.
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
