//! The Vulkan renderer. Haiku has no Vulkan WSI, so each frame renders into
//! an offscreen BGRA image, is copied into a host-visible buffer, and the
//! window shim blits that into a BBitmap. On the UTM VM this runs on the
//! Mac's GPU through Venus (virtio-gpu), via the `libvulkan.so.1` shim.

use std::ffi::CStr;
use std::io::Cursor;
use std::mem::size_of;

use ash::vk;

use crate::scene::{GlyphInstance, RectInstance, Scene};
use crate::text::{Atlas, ATLAS_SIZE};

const COLOR_FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

pub type Result<T> = std::result::Result<T, String>;

trait Check<T> {
    fn check(self, what: &str) -> Result<T>;
}

impl<T> Check<T> for std::result::Result<T, vk::Result> {
    fn check(self, what: &str) -> Result<T> {
        self.map_err(|e| format!("{what}: {e:?}"))
    }
}

struct Buffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    size: u64,
}

struct Target {
    width: u32,
    height: u32,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    readback: Buffer,
}

pub struct Gpu {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    memory_types: vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor: vk::DescriptorSet,
    rect_pipeline: vk::Pipeline,
    glyph_pipeline: vk::Pipeline,
    sampler: vk::Sampler,
    atlas_image: vk::Image,
    atlas_memory: vk::DeviceMemory,
    atlas_view: vk::ImageView,
    atlas_staging: Buffer,
    atlas_ready: bool,
    rects: Buffer,
    glyphs: Buffer,
    target: Option<Target>,
    pub device_name: String,
}

impl Gpu {
    pub fn new() -> Result<Gpu> {
        crate::trace("gpu: loading libvulkan.so.1");
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("loading libvulkan.so.1: {e}"))?;
        let app = vk::ApplicationInfo::default()
            .application_name(c"Temino")
            .engine_name(c"Temino")
            .api_version(vk::API_VERSION_1_1);
        let instance_info = vk::InstanceCreateInfo::default().application_info(&app);
        let instance = unsafe { entry.create_instance(&instance_info, None) }.check("vkCreateInstance")?;

        crate::trace("gpu: instance created");
        let physicals = unsafe { instance.enumerate_physical_devices() }.check("vkEnumeratePhysicalDevices")?;
        let (physical, family) = physicals
            .iter()
            .find_map(|&device| {
                let families = unsafe { instance.get_physical_device_queue_family_properties(device) };
                families
                    .iter()
                    .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                    .map(|family| (device, family as u32))
            })
            .ok_or("no Vulkan device with a graphics queue")?;
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        let device_name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }.to_string_lossy().into_owned();
        let memory_types = unsafe { instance.get_physical_device_memory_properties(physical) };

        let priorities = [1.0];
        let queue_info =
            [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];
        let device_info = vk::DeviceCreateInfo::default().queue_create_infos(&queue_info);
        let device = unsafe { instance.create_device(physical, &device_info, None) }.check("vkCreateDevice")?;
        let queue = unsafe { device.get_device_queue(family, 0) };
        crate::trace(&format!("gpu: device created on {device_name}"));

        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(family);
        let pool = unsafe { device.create_command_pool(&pool_info, None) }.check("vkCreateCommandPool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command = unsafe { device.allocate_command_buffers(&alloc) }.check("vkAllocateCommandBuffers")?[0];
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.check("vkCreateFence")?;

        let render_pass = create_render_pass(&device)?;

        let binding = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let set_layout = unsafe {
            device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&binding), None)
        }
        .check("vkCreateDescriptorSetLayout")?;
        let push = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::VERTEX, offset: 0, size: 8 }];
        let set_layouts = [set_layout];
        let layout_info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts).push_constant_ranges(&push);
        let layout = unsafe { device.create_pipeline_layout(&layout_info, None) }.check("vkCreatePipelineLayout")?;

        let rect_pipeline = create_pipeline(
            &device,
            render_pass,
            layout,
            include_bytes!("../shaders/rect.vert.spv"),
            include_bytes!("../shaders/rect.frag.spv"),
            size_of::<RectInstance>() as u32,
            7,
        )?;
        let glyph_pipeline = create_pipeline(
            &device,
            render_pass,
            layout,
            include_bytes!("../shaders/glyph.vert.spv"),
            include_bytes!("../shaders/glyph.frag.spv"),
            size_of::<GlyphInstance>() as u32,
            4,
        )?;

        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        let sampler = unsafe { device.create_sampler(&sampler_info, None) }.check("vkCreateSampler")?;

        let mut gpu = Gpu {
            _entry: entry,
            instance,
            device,
            memory_types,
            queue,
            pool,
            command,
            fence,
            render_pass,
            layout,
            set_layout,
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor: vk::DescriptorSet::null(),
            rect_pipeline,
            glyph_pipeline,
            sampler,
            atlas_image: vk::Image::null(),
            atlas_memory: vk::DeviceMemory::null(),
            atlas_view: vk::ImageView::null(),
            atlas_staging: Buffer {
                buffer: vk::Buffer::null(),
                memory: vk::DeviceMemory::null(),
                mapped: std::ptr::null_mut(),
                size: 0,
            },
            atlas_ready: false,
            rects: Buffer {
                buffer: vk::Buffer::null(),
                memory: vk::DeviceMemory::null(),
                mapped: std::ptr::null_mut(),
                size: 0,
            },
            glyphs: Buffer {
                buffer: vk::Buffer::null(),
                memory: vk::DeviceMemory::null(),
                mapped: std::ptr::null_mut(),
                size: 0,
            },
            target: None,
            device_name,
        };
        crate::trace("gpu: pipelines created");
        gpu.create_atlas()?;
        crate::trace("gpu: atlas created");
        gpu.rects = gpu.create_buffer(64 * 1024, vk::BufferUsageFlags::VERTEX_BUFFER)?;
        gpu.glyphs = gpu.create_buffer(256 * 1024, vk::BufferUsageFlags::VERTEX_BUFFER)?;
        Ok(gpu)
    }

    fn memory_type(&self, bits: u32, wanted: vk::MemoryPropertyFlags) -> Result<u32> {
        let types = &self.memory_types.memory_types[..self.memory_types.memory_type_count as usize];
        let allowed = |i: usize| bits & (1 << i) != 0;
        types
            .iter()
            .enumerate()
            .position(|(i, t)| allowed(i) && t.property_flags.contains(wanted))
            .or_else(|| {
                // Device-local is a preference, host visibility is not.
                if wanted.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
                    None
                } else {
                    (0..types.len()).find(|&i| allowed(i))
                }
            })
            .map(|i| i as u32)
            .ok_or_else(|| format!("no memory type for {wanted:?} in {bits:#x}"))
    }

    fn create_buffer(&self, size: u64, usage: vk::BufferUsageFlags) -> Result<Buffer> {
        let info = vk::BufferCreateInfo::default().size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE);
        let device = &self.device;
        let buffer = unsafe { device.create_buffer(&info, None) }.check("vkCreateBuffer")?;
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let cached = self
            .memory_type(requirements.memory_type_bits, host | vk::MemoryPropertyFlags::HOST_CACHED)
            .or_else(|_| self.memory_type(requirements.memory_type_bits, host))?;
        let alloc = vk::MemoryAllocateInfo::default().allocation_size(requirements.size).memory_type_index(cached);
        let memory = unsafe { device.allocate_memory(&alloc, None) }.check("vkAllocateMemory (buffer)")?;
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.check("vkBindBufferMemory")?;
        let mapped = unsafe { device.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) }
            .check("vkMapMemory")? as *mut u8;
        Ok(Buffer { buffer, memory, mapped, size })
    }

    fn destroy_buffer(&self, buffer: &Buffer) {
        unsafe {
            self.device.unmap_memory(buffer.memory);
            self.device.destroy_buffer(buffer.buffer, None);
            self.device.free_memory(buffer.memory, None);
        }
    }

    fn create_image(
        &self,
        width: u32,
        height: u32,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Result<(vk::Image, vk::DeviceMemory, vk::ImageView)> {
        let device = &self.device;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { device.create_image(&info, None) }.check("vkCreateImage")?;
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let kind = self.memory_type(requirements.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
        let alloc = vk::MemoryAllocateInfo::default().allocation_size(requirements.size).memory_type_index(kind);
        let memory = unsafe { device.allocate_memory(&alloc, None) }.check("vkAllocateMemory (image)")?;
        unsafe { device.bind_image_memory(image, memory, 0) }.check("vkBindImageMemory")?;
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(color_range());
        let view = unsafe { device.create_image_view(&view_info, None) }.check("vkCreateImageView")?;
        Ok((image, memory, view))
    }

    fn create_atlas(&mut self) -> Result<()> {
        let size = ATLAS_SIZE as u32;
        let (image, memory, view) = self.create_image(
            size,
            size,
            vk::Format::R8_UNORM,
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        )?;
        self.atlas_image = image;
        self.atlas_memory = memory;
        self.atlas_view = view;
        self.atlas_staging =
            self.create_buffer((ATLAS_SIZE * ATLAS_SIZE) as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;

        let device = &self.device;
        let sizes = [vk::DescriptorPoolSize { ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, descriptor_count: 1 }];
        let pool_info = vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes);
        self.descriptor_pool =
            unsafe { device.create_descriptor_pool(&pool_info, None) }.check("vkCreateDescriptorPool")?;
        let layouts = [self.set_layout];
        let alloc =
            vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.descriptor_pool).set_layouts(&layouts);
        self.descriptor = unsafe { device.allocate_descriptor_sets(&alloc) }.check("vkAllocateDescriptorSets")?[0];
        let image_info = [vk::DescriptorImageInfo {
            sampler: self.sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(self.descriptor)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info)];
        unsafe { device.update_descriptor_sets(&write, &[]) };
        Ok(())
    }

    fn ensure_target(&mut self, width: u32, height: u32) -> Result<()> {
        if let Some(target) = &self.target {
            if target.width == width && target.height == height {
                return Ok(());
            }
        }
        if let Some(target) = self.target.take() {
            unsafe {
                self.device.destroy_framebuffer(target.framebuffer, None);
                self.device.destroy_image_view(target.view, None);
                self.device.destroy_image(target.image, None);
                self.device.free_memory(target.memory, None);
            }
            self.destroy_buffer(&target.readback);
        }
        let (image, memory, view) = self.create_image(
            width,
            height,
            COLOR_FORMAT,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let attachments = [view];
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(self.render_pass)
            .attachments(&attachments)
            .width(width)
            .height(height)
            .layers(1);
        let framebuffer = unsafe { self.device.create_framebuffer(&info, None) }.check("vkCreateFramebuffer")?;
        let readback = self.create_buffer(width as u64 * height as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST)?;
        crate::trace(&format!("gpu: target {width}x{height} created"));
        self.target = Some(Target { width, height, image, memory, view, framebuffer, readback });
        Ok(())
    }

    fn upload_instances<T: Copy>(&mut self, which: usize, data: &[T]) -> Result<()> {
        let bytes = std::mem::size_of_val(data) as u64;
        let current = if which == 0 { self.rects.size } else { self.glyphs.size };
        if bytes > current {
            let size = bytes.next_power_of_two();
            let fresh = self.create_buffer(size, vk::BufferUsageFlags::VERTEX_BUFFER)?;
            let old = std::mem::replace(if which == 0 { &mut self.rects } else { &mut self.glyphs }, fresh);
            self.destroy_buffer(&old);
        }
        let buffer = if which == 0 { &self.rects } else { &self.glyphs };
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, buffer.mapped, bytes as usize) };
        Ok(())
    }

    /// Renders `scene` and returns the frame as tightly packed BGRA rows.
    pub fn render(&mut self, scene: &Scene, atlas: &mut Atlas) -> Result<&[u8]> {
        let width = scene.width.max(1.0) as u32;
        let height = scene.height.max(1.0) as u32;
        self.ensure_target(width, height)?;

        // Instance data for both layers lives in one buffer per pipeline.
        let mut rects = Vec::new();
        let mut glyphs = Vec::new();
        let mut ranges = Vec::new();
        for layer in &scene.layers {
            ranges.push(((rects.len(), layer.rects.len()), (glyphs.len(), layer.glyphs.len())));
            rects.extend_from_slice(&layer.rects);
            glyphs.extend_from_slice(&layer.glyphs);
        }
        self.upload_instances(0, &rects)?;
        self.upload_instances(1, &glyphs)?;

        let device = &self.device;
        let command = self.command;
        let target = self.target.as_ref().unwrap();
        unsafe {
            device.reset_command_buffer(command, vk::CommandBufferResetFlags::empty()).check("vkResetCommandBuffer")?;
            let begin = vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            device.begin_command_buffer(command, &begin).check("vkBeginCommandBuffer")?;

            if let Some((lo, hi)) = atlas.dirty.take() {
                let offset = lo * ATLAS_SIZE;
                let length = (hi - lo) * ATLAS_SIZE;
                std::ptr::copy_nonoverlapping(
                    atlas.pixels[offset..].as_ptr(),
                    self.atlas_staging.mapped.add(offset),
                    length,
                );
                let old_layout = if self.atlas_ready {
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                } else {
                    vk::ImageLayout::UNDEFINED
                };
                image_barrier(
                    device,
                    command,
                    self.atlas_image,
                    old_layout,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::SHADER_READ,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::TRANSFER,
                );
                let region = vk::BufferImageCopy {
                    buffer_offset: offset as u64,
                    buffer_row_length: ATLAS_SIZE as u32,
                    buffer_image_height: 0,
                    image_subresource: color_layers(),
                    image_offset: vk::Offset3D { x: 0, y: lo as i32, z: 0 },
                    image_extent: vk::Extent3D { width: ATLAS_SIZE as u32, height: (hi - lo) as u32, depth: 1 },
                };
                device.cmd_copy_buffer_to_image(
                    command,
                    self.atlas_staging.buffer,
                    self.atlas_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
                image_barrier(
                    device,
                    command,
                    self.atlas_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::SHADER_READ,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                );
                self.atlas_ready = true;
            }

            let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: scene.clear.to_array() } }];
            let extent = vk::Extent2D { width, height };
            let pass = vk::RenderPassBeginInfo::default()
                .render_pass(self.render_pass)
                .framebuffer(target.framebuffer)
                .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent })
                .clear_values(&clear);
            device.cmd_begin_render_pass(command, &pass, vk::SubpassContents::INLINE);
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: width as f32,
                height: height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            device.cmd_set_viewport(command, 0, &[viewport]);
            device.cmd_set_scissor(command, 0, &[vk::Rect2D { offset: vk::Offset2D::default(), extent }]);
            let viewport_size = [width as f32, height as f32];
            let push: [u8; 8] = std::mem::transmute(viewport_size);
            for ((rect_first, rect_count), (glyph_first, glyph_count)) in ranges {
                if rect_count > 0 {
                    device.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, self.rect_pipeline);
                    device.cmd_push_constants(command, self.layout, vk::ShaderStageFlags::VERTEX, 0, &push);
                    device.cmd_bind_vertex_buffers(command, 0, &[self.rects.buffer], &[0]);
                    device.cmd_draw(command, 4, rect_count as u32, 0, rect_first as u32);
                }
                if glyph_count > 0 && self.atlas_ready {
                    device.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, self.glyph_pipeline);
                    device.cmd_push_constants(command, self.layout, vk::ShaderStageFlags::VERTEX, 0, &push);
                    device.cmd_bind_descriptor_sets(
                        command,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.layout,
                        0,
                        &[self.descriptor],
                        &[],
                    );
                    device.cmd_bind_vertex_buffers(command, 0, &[self.glyphs.buffer], &[0]);
                    device.cmd_draw(command, 4, glyph_count as u32, 0, glyph_first as u32);
                }
            }
            device.cmd_end_render_pass(command);

            let region = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: color_layers(),
                image_offset: vk::Offset3D::default(),
                image_extent: vk::Extent3D { width, height, depth: 1 },
            };
            device.cmd_copy_image_to_buffer(
                command,
                target.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                target.readback.buffer,
                &[region],
            );
            let to_host = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(target.readback.buffer)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[to_host],
                &[],
            );
            device.end_command_buffer(command).check("vkEndCommandBuffer")?;

            let commands = [command];
            let submit = [vk::SubmitInfo::default().command_buffers(&commands)];
            crate::trace("gpu: submit");
            device.queue_submit(self.queue, &submit, self.fence).check("vkQueueSubmit")?;
            crate::trace("gpu: wait");
            device.wait_for_fences(&[self.fence], true, u64::MAX).check("vkWaitForFences")?;
            device.reset_fences(&[self.fence]).check("vkResetFences")?;
            crate::trace("gpu: frame done");

            let length = width as usize * height as usize * 4;
            Ok(std::slice::from_raw_parts(target.readback.mapped, length))
        }
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(target) = self.target.take() {
                self.device.destroy_framebuffer(target.framebuffer, None);
                self.device.destroy_image_view(target.view, None);
                self.device.destroy_image(target.image, None);
                self.device.free_memory(target.memory, None);
                self.destroy_buffer(&target.readback);
            }
            self.destroy_buffer(&self.rects);
            self.destroy_buffer(&self.glyphs);
            self.destroy_buffer(&self.atlas_staging);
            self.device.destroy_image_view(self.atlas_view, None);
            self.device.destroy_image(self.atlas_image, None);
            self.device.free_memory(self.atlas_memory, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.rect_pipeline, None);
            self.device.destroy_pipeline(self.glyph_pipeline, None);
            self.device.destroy_pipeline_layout(self.layout, None);
            self.device.destroy_descriptor_set_layout(self.set_layout, None);
            self.device.destroy_render_pass(self.render_pass, None);
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

fn color_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

unsafe fn image_barrier(
    device: &ash::Device,
    command: vk::CommandBuffer,
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src_access: vk::AccessFlags,
    dst_access: vk::AccessFlags,
    src_stage: vk::PipelineStageFlags,
    dst_stage: vk::PipelineStageFlags,
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(old)
        .new_layout(new)
        .src_access_mask(src_access)
        .dst_access_mask(dst_access)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range());
    device.cmd_pipeline_barrier(command, src_stage, dst_stage, vk::DependencyFlags::empty(), &[], &[], &[barrier]);
}

fn create_render_pass(device: &ash::Device) -> Result<vk::RenderPass> {
    let attachment = [vk::AttachmentDescription::default()
        .format(COLOR_FORMAT)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)];
    let reference = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
    let subpass = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&reference)];
    let dependencies = [
        vk::SubpassDependency {
            src_subpass: vk::SUBPASS_EXTERNAL,
            dst_subpass: 0,
            src_stage_mask: vk::PipelineStageFlags::TRANSFER,
            dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            src_access_mask: vk::AccessFlags::TRANSFER_READ,
            dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            dependency_flags: vk::DependencyFlags::empty(),
        },
        vk::SubpassDependency {
            src_subpass: 0,
            dst_subpass: vk::SUBPASS_EXTERNAL,
            src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            dst_stage_mask: vk::PipelineStageFlags::TRANSFER,
            src_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            dst_access_mask: vk::AccessFlags::TRANSFER_READ,
            dependency_flags: vk::DependencyFlags::empty(),
        },
    ];
    let info =
        vk::RenderPassCreateInfo::default().attachments(&attachment).subpasses(&subpass).dependencies(&dependencies);
    unsafe { device.create_render_pass(&info, None) }.check("vkCreateRenderPass")
}

fn create_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    vertex: &[u8],
    fragment: &[u8],
    stride: u32,
    attributes: u32,
) -> Result<vk::Pipeline> {
    let module = |bytes: &[u8]| -> Result<vk::ShaderModule> {
        let words = ash::util::read_spv(&mut Cursor::new(bytes)).map_err(|e| format!("bad SPIR-V: {e}"))?;
        unsafe { device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None) }
            .check("vkCreateShaderModule")
    };
    let vertex = module(vertex)?;
    let fragment = module(fragment)?;
    let stages = [
        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vertex).name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(fragment)
            .name(c"main"),
    ];
    let bindings =
        [vk::VertexInputBindingDescription { binding: 0, stride, input_rate: vk::VertexInputRate::INSTANCE }];
    let attributes: Vec<_> = (0..attributes)
        .map(|location| vk::VertexInputAttributeDescription {
            location,
            binding: 0,
            format: vk::Format::R32G32B32A32_SFLOAT,
            offset: location * 16,
        })
        .collect();
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
    let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample =
        vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let blend = [vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::TRUE,
        src_color_blend_factor: vk::BlendFactor::ONE,
        dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        color_blend_op: vk::BlendOp::ADD,
        src_alpha_blend_factor: vk::BlendFactor::ONE,
        dst_alpha_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        alpha_blend_op: vk::BlendOp::ADD,
        color_write_mask: vk::ColorComponentFlags::RGBA,
    }];
    let blend_state = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blend_state)
        .dynamic_state(&dynamic)
        .layout(layout)
        .render_pass(render_pass)
        .subpass(0);
    let pipeline = unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }
        .map_err(|(_, e)| format!("vkCreateGraphicsPipelines: {e:?}"))?[0];
    unsafe {
        device.destroy_shader_module(vertex, None);
        device.destroy_shader_module(fragment, None);
    }
    Ok(pipeline)
}
