//! Minimal Vulkan back end.
//!
//! One `Gfx` owns the instance / device / pipeline and is shared by every
//! monitor window; each window then owns a `Surface` (swap chain, framebuffers,
//! per-image uniform buffers and descriptors).
//!
//! The scene is drawn with a single three-vertex fullscreen triangle and a
//! purely procedural fragment shader, so there is no vertex buffer, no texture,
//! no depth buffer and no blending.

use std::ffi::{c_char, CStr, CString};

use ash::{vk, Device, Entry, Instance};

const SPV_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene.spv"));

/// Mirrors `struct Globals` in `shaders/scene.wgsl` exactly (std140 rules).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Globals {
    pub resolution: [f32; 2],   // 0
    pub time: f32,              // 8
    pub eye_scale_px: f32,      // 12
    /// Top-left corner of the 160x160 eye space, in pixels.  The eye centre is
    /// therefore view_origin + 80 * eye_scale_px.
    pub view_origin: [f32; 2],  // 16
    pub lash_angle: f32,        // 24
    pub lid_center_y: f32,      // 28
    pub lid_curve: f32,         // 32
    pub sclera_scale: f32,      // 36
    pub l3_scale: f32,          // 40
    pub l2_scale: f32,          // 44
    pub l1_scale: f32,          // 48
    pub flicker: f32,           // 52
    pub gaze: [f32; 2],         // 56
    pub pulse_phase: f32,       // 64
    pub pulse_cycle: f32,       // 68
    pub glitch_mode: f32,       // 72
    pub glitch_amp: f32,        // 76
    pub bright: f32,            // 80
    pub contrast: f32,          // 84
    pub skew: f32,              // 88
    pub gx: f32,                // 92
    // Layout must match the Globals struct in shaders/scene.wgsl under std140
    // rules: vec4 members force 16-byte alignment, so every 16-byte member here
    // has to start on a multiple of 16 as well.
    pub slice_offsets: [f32; 4],// 96
    pub slice_offset5: f32,     // 112
    pub glitch_seed: f32,       // 116
    pub slice_edges: [f32; 4],  // 120
    pub eye_opacity: f32,       // 136
    pub spare0: f32,            // 140
}

impl Default for Globals {
    fn default() -> Self {
        Globals {
            resolution: [1.0, 1.0],
            time: 0.0,
            eye_scale_px: 3.0,
            view_origin: [0.0, 0.0],
            lash_angle: 0.0,
            lid_center_y: 100.0,
            lid_curve: 8.25,
            sclera_scale: 0.985,
            l3_scale: 1.0,
            l2_scale: 1.0,
            l1_scale: 1.0,
            flicker: 0.0,
            gaze: [0.0, 0.0],
            pulse_phase: -1.0,
            pulse_cycle: 4.0,
            glitch_mode: 0.0,
            glitch_amp: 0.0,
            bright: 1.0,
            contrast: 1.0,
            skew: 0.0,
            gx: 0.0,
            slice_offsets: [0.0; 4],
            slice_offset5: 0.0,
            glitch_seed: 1.0,
            slice_edges: [46.0, 80.0, 114.0, 148.0],
            eye_opacity: 1.0,
            spare0: 0.0,
        }
    }
}

const GLOBALS_SIZE: usize = std::mem::size_of::<Globals>();

/// Upper bound on swap chain images per window; also the descriptor pool size.
const MAX_SWAPCHAIN_IMAGES: u32 = 16;

// ---------------------------------------------------------------- helpers --

fn spv_words() -> &'static [u32] {
    debug_assert!(SPV_BYTES.len() % 4 == 0);
    // The build script emits little-endian words; every supported target is LE.
    unsafe { std::slice::from_raw_parts(SPV_BYTES.as_ptr() as *const u32, SPV_BYTES.len() / 4) }
}

unsafe fn cstr_array(names: &[&'static CStr]) -> Vec<*const c_char> {
    names.iter().map(|n| n.as_ptr()).collect()
}

// -------------------------------------------------------------------- Gfx --

pub struct Gfx {
    pub entry: Entry,
    pub instance: Instance,
    pub pdev: vk::PhysicalDevice,
    pub device: Option<Device>,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub surface_loader: ash::khr::surface::Instance,
    swapchain_loader: Option<ash::khr::swapchain::Device>,

    descriptor_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    render_pass: vk::RenderPass,
    command_pool: vk::CommandPool,
    format: Option<vk::Format>,
}

impl Gfx {
    pub unsafe fn new() -> Result<Gfx, String> {
        let entry = Entry::load().map_err(|e| format!("无法加载 Vulkan 运行时 (vulkan-1.dll): {e}"))?;

        let app_name = CString::new("FairyScreenSaver").unwrap();
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(&app_name)
            .api_version(vk::API_VERSION_1_0);

        let want = [ash::khr::surface::NAME, ash::khr::win32_surface::NAME];
        let ext_ptrs = cstr_array(&want);
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&ext_ptrs);
        let instance = entry
            .create_instance(&instance_info, None)
            .map_err(|e| format!("创建 Vulkan 实例失败: {e}"))?;

        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);

        Ok(Gfx {
            entry,
            instance,
            pdev: vk::PhysicalDevice::null(),
            device: None,
            queue: vk::Queue::null(),
            queue_family: 0,
            surface_loader,
            swapchain_loader: None,
            descriptor_layout: vk::DescriptorSetLayout::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            render_pass: vk::RenderPass::null(),
            command_pool: vk::CommandPool::null(),
            format: None,
        })
    }

    pub unsafe fn create_surface(&self, hwnd: isize, hinstance: isize) -> Result<vk::SurfaceKHR, String> {
        let loader = ash::khr::win32_surface::Instance::new(&self.entry, &self.instance);
        let info = vk::Win32SurfaceCreateInfoKHR::default().hinstance(hinstance).hwnd(hwnd);
        loader
            .create_win32_surface(&info, None)
            .map_err(|e| format!("创建窗口表面失败: {e}"))
    }

    pub unsafe fn destroy_surface(&self, surface: vk::SurfaceKHR) {
        self.surface_loader.destroy_surface(surface, None);
    }

    pub fn dev(&self) -> &Device {
        self.device.as_ref().expect("Vulkan device 尚未创建")
    }

    pub fn swapchain(&self) -> &ash::khr::swapchain::Device {
        self.swapchain_loader.as_ref().expect("Vulkan device 尚未创建")
    }

    /// Picks the first physical device able to both draw and present on `surface`.
    pub unsafe fn select_device(&mut self, surface: vk::SurfaceKHR) -> Result<(), String> {
        let devices = self
            .instance
            .enumerate_physical_devices()
            .map_err(|e| format!("枚举显卡失败: {e}"))?;

        let mut best: Option<(i32, vk::PhysicalDevice, u32)> = None;
        for pdev in devices {
            let props = self.instance.get_physical_device_properties(pdev);
            if props.api_version < vk::API_VERSION_1_0 {
                continue;
            }
            let families = self.instance.get_physical_device_queue_family_properties(pdev);
            for (i, fam) in families.iter().enumerate() {
                if !fam.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                    continue;
                }
                let i = i as u32;
                let present = match self.surface_loader.get_physical_device_surface_support(
                    pdev, i, surface,
                ) {
                    Ok(true) => true,
                    _ => false,
                };
                if !present {
                    continue;
                }
                let score = match props.device_type {
                    vk::PhysicalDeviceType::DISCRETE_GPU => 1000,
                    vk::PhysicalDeviceType::INTEGRATED_GPU => 500,
                    vk::PhysicalDeviceType::VIRTUAL_GPU => 200,
                    _ => 100,
                };
                if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                    best = Some((score, pdev, i));
                }
                break;
            }
        }

        let Some((_, pdev, family)) = best else {
            return Err("没有找到同时支持图形与显示的 Vulkan 设备".to_string());
        };
        crate::diag::log(&format!("  device: family={family}"));
        self.pdev = pdev;
        self.queue_family = family;

        let ext = [ash::khr::swapchain::NAME];
        let ext_ptrs = cstr_array(&ext);
        let priorities = [1.0f32];
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue_info))
            .enabled_extension_names(&ext_ptrs);
        let device = self
            .instance
            .create_device(pdev, &device_info, None)
            .map_err(|e| format!("创建设备失败: {e}"))?;
        self.queue = device.get_device_queue(family, 0);
        self.swapchain_loader = Some(ash::khr::swapchain::Device::new(&self.instance, &device));
        self.device = Some(device);
        Ok(())
    }

    /// Names of every Vulkan adapter present, for the diagnostics dump.
    pub unsafe fn vulkan_devices(&self) -> Vec<String> {
        let Ok(devices) = self.instance.enumerate_physical_devices() else {
            return Vec::new();
        };
        devices
            .into_iter()
            .map(|p| {
                let props = self.instance.get_physical_device_properties(p);
                let name = CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy().into_owned();
                format!("{name} ({:?})", props.device_type)
            })
            .collect()
    }

    pub unsafe fn device_name(&self) -> String {
        if self.pdev == vk::PhysicalDevice::null() {
            return String::new();
        }
        let props = self.instance.get_physical_device_properties(self.pdev);
        let name = CStr::from_ptr(props.device_name.as_ptr());
        name.to_string_lossy().into_owned()
    }

    /// Builds the render pass, pipeline and descriptor machinery for `format`.
    pub unsafe fn init_pipeline(&mut self, format: vk::Format) -> Result<(), String> {
        if self.format == Some(format) {
            return Ok(());
        }

        // ---- render pass -------------------------------------------------
        let color = vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
        let color_ref = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color_ref));
        let dependency = vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
        let attachments = [color];
        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(std::slice::from_ref(&subpass))
            .dependencies(std::slice::from_ref(&dependency));
        self.render_pass = self
            .dev()
            .create_render_pass(&render_pass_info, None)
            .map_err(|e| format!("创建 render pass 失败: {e}"))?;

        // ---- descriptor set layout ---------------------------------------
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT);
        let bindings = [binding];
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        self.descriptor_layout = self
            .dev()
            .create_descriptor_set_layout(&layout_info, None)
            .map_err(|e| format!("创建描述符布局失败: {e}"))?;

        // ---- pipeline layout ---------------------------------------------
        let set_layouts = [self.descriptor_layout];
        let pl_info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        self.pipeline_layout = self
            .dev()
            .create_pipeline_layout(&pl_info, None)
            .map_err(|e| format!("创建管线布局失败: {e}"))?;

        // ---- shader modules ----------------------------------------------
        let words = spv_words();
        let module_info = vk::ShaderModuleCreateInfo::default().code(words);
        let module = self
            .dev()
            .create_shader_module(&module_info, None)
            .map_err(|e| format!("创建着色器模块失败: {e}"))?;

        let vs_name = CString::new("vs_main").unwrap();
        let fs_name = CString::new("fs_main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(module)
                .name(&vs_name),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(&fs_name),
        ];

        // ---- fixed function state ----------------------------------------
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default()
            .attachments(std::slice::from_ref(&color_blend_attachment));
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(self.pipeline_layout)
            .render_pass(self.render_pass)
            .subpass(0);

        let pipelines = self
            .dev()
            .create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
            .map_err(|(_, e)| format!("创建图形管线失败: {e}"))?;
        self.pipeline = pipelines[0];
        self.dev().destroy_shader_module(module, None);

        // ---- command pool -------------------------------------------------
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(self.queue_family);
        self.command_pool = self
            .dev()
            .create_command_pool(&pool_info, None)
            .map_err(|e| format!("创建命令池失败: {e}"))?;

        self.format = Some(format);
        Ok(())
    }

    pub fn render_pass(&self) -> vk::RenderPass {
        self.render_pass
    }
    pub fn pipeline(&self) -> vk::Pipeline {
        self.pipeline
    }
    pub fn pipeline_layout(&self) -> vk::PipelineLayout {
        self.pipeline_layout
    }
    pub fn descriptor_layout(&self) -> vk::DescriptorSetLayout {
        self.descriptor_layout
    }
    pub fn command_pool(&self) -> vk::CommandPool {
        self.command_pool
    }

    pub unsafe fn destroy(&mut self) {
        if let Some(device) = self.device.take() {
            let _ = device.device_wait_idle();
            device.destroy_command_pool(self.command_pool, None);
            if self.pipeline != vk::Pipeline::null() {
                device.destroy_pipeline(self.pipeline, None);
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                device.destroy_pipeline_layout(self.pipeline_layout, None);
            }
            if self.descriptor_layout != vk::DescriptorSetLayout::null() {
                device.destroy_descriptor_set_layout(self.descriptor_layout, None);
            }
            if self.render_pass != vk::RenderPass::null() {
                device.destroy_render_pass(self.render_pass, None);
            }
            device.destroy_device(None);
        }
        self.instance.destroy_instance(None);
    }
}

// ----------------------------------------------------------------- Surface --

pub struct Surface {
    pub surface: vk::SurfaceKHR,
    pub swapchain: vk::SwapchainKHR,
    pub views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,
    pub buffers: Vec<vk::Buffer>,
    pub memories: Vec<vk::DeviceMemory>,
    pub mapped: Vec<*mut u8>,
    pub sets: Vec<vk::DescriptorSet>,
    pub command: vk::CommandBuffer,
    pool: vk::DescriptorPool,
    pub image_available: vk::Semaphore,
    pub render_finished: vk::Semaphore,
    pub in_flight: vk::Fence,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
}

impl Surface {
    pub unsafe fn new(
        gfx: &Gfx,
        surface: vk::SurfaceKHR,
        format: vk::Format,
        extent: vk::Extent2D,
    ) -> Result<Surface, String> {
        let mut s = Surface {
            surface,
            swapchain: vk::SwapchainKHR::null(),
            views: Vec::new(),
            framebuffers: Vec::new(),
            buffers: Vec::new(),
            memories: Vec::new(),
            mapped: Vec::new(),
            sets: Vec::new(),
            command: vk::CommandBuffer::null(),
            pool: vk::DescriptorPool::null(),
            image_available: vk::Semaphore::null(),
            render_finished: vk::Semaphore::null(),
            in_flight: vk::Fence::null(),
            extent,
            format,
        };
        // A pool per window keeps a resize from disturbing other windows.
        let size = vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(MAX_SWAPCHAIN_IMAGES);
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(MAX_SWAPCHAIN_IMAGES)
            .pool_sizes(std::slice::from_ref(&size));
        s.pool = gfx
            .dev()
            .create_descriptor_pool(&pool_info, None)
            .map_err(|e| format!("创建描述符池失败: {e}"))?;

        s.build_swapchain(gfx, extent)?;

        let sem = vk::SemaphoreCreateInfo::default();
        s.image_available = gfx.dev().create_semaphore(&sem, None).map_err(|e| e.to_string())?;
        s.render_finished = gfx.dev().create_semaphore(&sem, None).map_err(|e| e.to_string())?;
        let fence = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        s.in_flight = gfx.dev().create_fence(&fence, None).map_err(|e| e.to_string())?;

        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(gfx.command_pool())
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        s.command = gfx.dev().allocate_command_buffers(&alloc).map_err(|e| e.to_string())?[0];
        Ok(s)
    }

    unsafe fn pick_format(gfx: &Gfx, surface: vk::SurfaceKHR) -> Result<vk::Format, String> {
        let formats = gfx
            .surface_loader
            .get_physical_device_surface_formats(gfx.pdev, surface)
            .map_err(|e| format!("查询表面格式失败: {e}"))?;
        let preferred = [
            vk::Format::B8G8R8A8_UNORM,
            vk::Format::B8G8R8A8_SRGB,
            vk::Format::R8G8B8A8_UNORM,
            vk::Format::R8G8B8A8_SRGB,
        ];
        for want in preferred {
            if formats.iter().any(|f| f.format == want && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR) {
                return Ok(want);
            }
        }
        formats
            .first()
            .map(|f| f.format)
            .ok_or_else(|| "表面没有可用格式".to_string())
    }

    pub unsafe fn preferred_format(gfx: &Gfx, surface: vk::SurfaceKHR) -> Result<vk::Format, String> {
        Self::pick_format(gfx, surface)
    }

    unsafe fn build_swapchain(&mut self, gfx: &Gfx, extent: vk::Extent2D) -> Result<(), String> {
        let caps = gfx
            .surface_loader
            .get_physical_device_surface_capabilities(gfx.pdev, self.surface)
            .map_err(|e| format!("查询表面能力失败: {e}"))?;

        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: extent.width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: extent.height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let image_count = {
            let want = caps.min_image_count + 1;
            if caps.max_image_count == 0 {
                want
            } else {
                want.min(caps.max_image_count)
            }
        };

        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(image_count)
            .image_format(self.format)
            .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(self.swapchain);

        let new_chain = gfx
            .swapchain()
            .create_swapchain(&info, None)
            .map_err(|e| format!("创建交换链失败: {e}"))?;
        if self.swapchain != vk::SwapchainKHR::null() {
            gfx.swapchain().destroy_swapchain(self.swapchain, None);
        }
        self.swapchain = new_chain;
        self.extent = extent;

        let images = gfx
            .swapchain()
            .get_swapchain_images(self.swapchain)
            .map_err(|e| format!("获取交换链图像失败: {e}"))?;

        // Rebuild views / framebuffers / uniform buffers for the new image set.
        self.release_image_resources(gfx);

        let view_info = vk::ImageViewCreateInfo::default()
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(self.format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });

        for &image in &images {
            let view = gfx
                .dev()
                .create_image_view(&view_info.image(image), None)
                .map_err(|e| e.to_string())?;
            self.views.push(view);

            let fb_info = vk::FramebufferCreateInfo::default()
                .render_pass(gfx.render_pass())
                .attachments(std::slice::from_ref(&view))
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            let fb = gfx.dev().create_framebuffer(&fb_info, None).map_err(|e| e.to_string())?;
            self.framebuffers.push(fb);

            let buf_info = vk::BufferCreateInfo::default()
                .size(GLOBALS_SIZE as u64)
                .usage(vk::BufferUsageFlags::UNIFORM_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let buffer = gfx.dev().create_buffer(&buf_info, None).map_err(|e| e.to_string())?;
            let req = gfx.dev().get_buffer_memory_requirements(buffer);
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(find_memory_type(
                    gfx,
                    req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )?);
            let memory = gfx.dev().allocate_memory(&alloc_info, None).map_err(|e| e.to_string())?;
            gfx.dev().bind_buffer_memory(buffer, memory, 0).map_err(|e| e.to_string())?;
            let ptr = gfx.dev().map_memory(memory, 0, req.size, vk::MemoryMapFlags::empty()).map_err(|e| e.to_string())? as *mut u8;
            self.buffers.push(buffer);
            self.memories.push(memory);
            self.mapped.push(ptr);
        }

        // Descriptor sets, one per swap chain image, recycled from our pool.
        gfx.dev()
            .reset_descriptor_pool(self.pool, vk::DescriptorPoolResetFlags::empty())
            .map_err(|e| format!("重置描述符池失败: {e}"))?;
        let layouts = vec![gfx.descriptor_layout(); images.len()];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.pool)
            .set_layouts(&layouts);
        self.sets = gfx.dev().allocate_descriptor_sets(&alloc).map_err(|e| e.to_string())?;

        for (i, &set) in self.sets.iter().enumerate() {
            let buffer_info = vk::DescriptorBufferInfo::default()
                .buffer(self.buffers[i])
                .offset(0)
                .range(GLOBALS_SIZE as u64);
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(std::slice::from_ref(&buffer_info));
            gfx.dev().update_descriptor_sets(&[write], &[]);
        }
        Ok(())
    }

    unsafe fn release_image_resources(&mut self, gfx: &Gfx) {
        self.sets.clear(); // reclaimed wholesale by the pool reset
        for fb in self.framebuffers.drain(..) {
            gfx.dev().destroy_framebuffer(fb, None);
        }
        for view in self.views.drain(..) {
            gfx.dev().destroy_image_view(view, None);
        }
        for i in 0..self.buffers.len() {
            gfx.dev().unmap_memory(self.memories[i]);
            gfx.dev().destroy_buffer(self.buffers[i], None);
            gfx.dev().free_memory(self.memories[i], None);
        }
        self.buffers.clear();
        self.memories.clear();
        self.mapped.clear();
    }

    pub unsafe fn resize(&mut self, gfx: &Gfx, extent: vk::Extent2D) -> Result<(), String> {
        if extent.width == 0 || extent.height == 0 {
            return Ok(());
        }
        let _ = gfx.dev().device_wait_idle();
        self.build_swapchain(gfx, extent)
    }

    pub unsafe fn draw(&mut self, gfx: &Gfx, globals: &Globals) -> Result<bool, String> {
        gfx.dev()
            .wait_for_fences(&[self.in_flight], true, u64::MAX)
            .map_err(|e| e.to_string())?;

        let acquired = gfx.swapchain().acquire_next_image(
            self.swapchain,
            u64::MAX,
            self.image_available,
            vk::Fence::null(),
        );
        let image_index = match acquired {
            Ok((idx, _)) => idx as usize,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(e) => return Err(format!("获取交换链图像失败: {e}")),
        };

        gfx.dev().reset_fences(&[self.in_flight]).map_err(|e| e.to_string())?;

        std::ptr::copy_nonoverlapping(
            globals as *const Globals as *const u8,
            self.mapped[image_index],
            GLOBALS_SIZE,
        );

        gfx.dev()
            .reset_command_buffer(self.command, vk::CommandBufferResetFlags::empty())
            .map_err(|e| e.to_string())?;
        let begin = vk::CommandBufferBeginInfo::default();
        gfx.dev().begin_command_buffer(self.command, &begin).map_err(|e| e.to_string())?;

        let clear = vk::ClearValue {
            color: vk::ClearColorValue { float32: [0.027, 0.063, 0.110, 1.0] },
        };
        let rp_begin = vk::RenderPassBeginInfo::default()
            .render_pass(gfx.render_pass())
            .framebuffer(self.framebuffers[image_index])
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: self.extent,
            })
            .clear_values(std::slice::from_ref(&clear));
        gfx.dev().cmd_begin_render_pass(self.command, &rp_begin, vk::SubpassContents::INLINE);

        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: self.extent.width as f32,
            height: self.extent.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: self.extent,
        };
        gfx.dev().cmd_set_viewport(self.command, 0, &[viewport]);
        gfx.dev().cmd_set_scissor(self.command, 0, &[scissor]);
        gfx.dev().cmd_bind_pipeline(self.command, vk::PipelineBindPoint::GRAPHICS, gfx.pipeline());
        gfx.dev().cmd_bind_descriptor_sets(
            self.command,
            vk::PipelineBindPoint::GRAPHICS,
            gfx.pipeline_layout(),
            0,
            &[self.sets[image_index]],
            &[],
        );
        gfx.dev().cmd_draw(self.command, 3, 1, 0, 0);
        gfx.dev().cmd_end_render_pass(self.command);
        gfx.dev().end_command_buffer(self.command).map_err(|e| e.to_string())?;

        let wait_stage = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(std::slice::from_ref(&self.image_available))
            .wait_dst_stage_mask(&wait_stage)
            .command_buffers(std::slice::from_ref(&self.command))
            .signal_semaphores(std::slice::from_ref(&self.render_finished));
        gfx.dev()
            .queue_submit(gfx.queue, &[submit], self.in_flight)
            .map_err(|e| format!("提交命令失败: {e}"))?;

        let swapchains = [self.swapchain];
        let indices = [image_index as u32];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(std::slice::from_ref(&self.render_finished))
            .swapchains(&swapchains)
            .image_indices(&indices);
        match gfx.swapchain().queue_present(gfx.queue, &present) {
            Ok(false) => {}
            Ok(true) => return Ok(false), // suboptimal: rebuild on the next frame
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(e) => return Err(format!("呈现失败: {e}")),
        }
        Ok(true)
    }

    pub unsafe fn destroy(&mut self, gfx: &Gfx) {
        if self.swapchain != vk::SwapchainKHR::null() {
            let _ = gfx.dev().device_wait_idle();
            self.release_image_resources(gfx);
            gfx.swapchain().destroy_swapchain(self.swapchain, None);
            self.swapchain = vk::SwapchainKHR::null();
        }
        if self.in_flight != vk::Fence::null() {
            gfx.dev().destroy_fence(self.in_flight, None);
        }
        if self.render_finished != vk::Semaphore::null() {
            gfx.dev().destroy_semaphore(self.render_finished, None);
        }
        if self.image_available != vk::Semaphore::null() {
            gfx.dev().destroy_semaphore(self.image_available, None);
        }
        if self.command != vk::CommandBuffer::null() {
            gfx.dev().free_command_buffers(gfx.command_pool(), &[self.command]);
        }
        if self.pool != vk::DescriptorPool::null() {
            gfx.dev().destroy_descriptor_pool(self.pool, None);
        }
        if self.surface != vk::SurfaceKHR::null() {
            gfx.surface_loader.destroy_surface(self.surface, None);
        }
    }
}

unsafe fn find_memory_type(gfx: &Gfx, bits: u32, want: vk::MemoryPropertyFlags) -> Result<u32, String> {
    let props = gfx.instance.get_physical_device_memory_properties(gfx.pdev);
    for i in 0..props.memory_type_count {
        let supported = bits & (1 << i) != 0;
        let flags = props.memory_types[i as usize].property_flags;
        if supported && flags.contains(want) {
            return Ok(i);
        }
    }
    Err("找不到合适的显存类型".to_string())
}
