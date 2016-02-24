use std::mem;
use std::ptr;
use std::sync::Arc;

use buffer::AbstractBuffer;
use descriptor_set::layout_def::PipelineLayoutDesc;
use descriptor_set::layout_def::DescriptorSetDesc;
use descriptor_set::layout_def::DescriptorWrite;
use descriptor_set::layout_def::DescriptorBind;
use descriptor_set::pool::DescriptorPool;
use device::Device;
use image::AbstractImageView;
use sampler::Sampler;

use OomError;
use VulkanObject;
use VulkanPointers;
use check_errors;
use vk;

/// An actual descriptor set with the resources that are binded to it.
pub struct DescriptorSet<S> {
    set: vk::DescriptorSet,
    pool: Arc<DescriptorPool>,
    layout: Arc<DescriptorSetLayout<S>>,

    // Here we store the resources used by the descriptor set.
    // TODO: for the moment even when a resource is overwritten it stays in these lists
    resources_samplers: Vec<Arc<Sampler>>,
    resources_image_views: Vec<Arc<AbstractImageView>>,
    resources_buffers: Vec<Arc<AbstractBuffer>>,
}

impl<S> DescriptorSet<S> where S: DescriptorSetDesc {
    ///
    /// # Panic
    ///
    /// - Panicks if the pool and the layout were not created from the same `Device`.
    ///
    pub fn new(pool: &Arc<DescriptorPool>, layout: &Arc<DescriptorSetLayout<S>>, init: S::Init)
               -> Result<Arc<DescriptorSet<S>>, OomError>
    {
        unsafe {
            let mut set = try!(DescriptorSet::uninitialized(pool, layout));
            Arc::get_mut(&mut set).unwrap().unchecked_write(layout.description().decode_init(init));
            Ok(set)
        }
    }

    ///
    /// # Panic
    ///
    /// - Panicks if the pool and the layout were not created from the same `Device`.
    ///
    // FIXME: this has to check whether there's still enough room in the pool
    pub unsafe fn uninitialized(pool: &Arc<DescriptorPool>, layout: &Arc<DescriptorSetLayout<S>>)
                                -> Result<Arc<DescriptorSet<S>>, OomError>
    {
        assert_eq!(&**pool.device() as *const Device, &*layout.device as *const Device);

        let vk = pool.device().pointers();

        let set = {
            let infos = vk::DescriptorSetAllocateInfo {
                sType: vk::STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
                pNext: ptr::null(),
                descriptorPool: pool.internal_object(),
                descriptorSetCount: 1,
                pSetLayouts: &layout.layout,
            };

            let mut output = mem::uninitialized();
            try!(check_errors(vk.AllocateDescriptorSets(pool.device().internal_object(), &infos,
                                                        &mut output)));
            output
        };

        Ok(Arc::new(DescriptorSet {
            set: set,
            pool: pool.clone(),
            layout: layout.clone(),

            resources_samplers: Vec::new(),
            resources_image_views: Vec::new(),
            resources_buffers: Vec::new(),
        }))
    }

    /// Modifies a descriptor set.
    ///
    /// The parameter depends on your implementation of `DescriptorSetDesc`.
    ///
    /// This function trusts the implementation of `DescriptorSetDesc` when it comes to making sure
    /// that the correct resource type is written to the correct descriptor.
    pub fn write(&mut self, write: S::Write) {
        let write = self.layout.description().decode_write(write);
        unsafe { self.unchecked_write(write); }
    }

    /// Modifies a descriptor set without checking that the writes are correct.
    pub unsafe fn unchecked_write(&mut self, write: Vec<DescriptorWrite>) {
        let vk = self.pool.device().pointers();

        // TODO: how do we remove the existing resources that are overwritten?

        // This function uses multiple closures which all borrow `self`. In order to satisfy the
        // borrow checker, we extract references to the members here.
        let ref mut self_resources_buffers = self.resources_buffers;
        let ref mut self_resources_samplers = self.resources_samplers;
        let ref mut self_resources_image_views = self.resources_image_views;
        let self_set = self.set;

        // TODO: allocate on stack instead (https://github.com/rust-lang/rfcs/issues/618)
        let buffer_descriptors = write.iter().filter_map(|write| {
            match write.content {
                DescriptorBind::UniformBuffer { ref buffer, offset, size } |
                DescriptorBind::DynamicUniformBuffer { ref buffer, offset, size } => {
                    assert!(buffer.usage_uniform_buffer());
                    self_resources_buffers.push(buffer.clone());
                    Some(vk::DescriptorBufferInfo {
                        buffer: buffer.internal_object(),
                        offset: offset as u64,
                        range: size as u64,
                    })
                },
                DescriptorBind::StorageBuffer { ref buffer, offset, size } |
                DescriptorBind::DynamicStorageBuffer { ref buffer, offset, size } => {
                    assert!(buffer.usage_storage_buffer());
                    self_resources_buffers.push(buffer.clone());
                    Some(vk::DescriptorBufferInfo {
                        buffer: buffer.internal_object(),
                        offset: offset as u64,
                        range: size as u64,
                    })
                },
                _ => None
            }
        }).collect::<Vec<_>>();

        // TODO: allocate on stack instead (https://github.com/rust-lang/rfcs/issues/618)
        let image_descriptors = write.iter().filter_map(|write| {
            match write.content {
                DescriptorBind::Sampler(ref sampler) => {
                    self_resources_samplers.push(sampler.clone());
                    Some(vk::DescriptorImageInfo {
                        sampler: sampler.internal_object(),
                        imageView: 0,
                        imageLayout: 0,
                    })
                },
                DescriptorBind::CombinedImageSampler(ref sampler, ref image, layout) => {
                    assert!(image.usage_sampled());
                    self_resources_samplers.push(sampler.clone());
                    self_resources_image_views.push(image.clone());
                    Some(vk::DescriptorImageInfo {
                        sampler: sampler.internal_object(),
                        imageView: image.internal_object(),
                        imageLayout: layout as u32,
                    })
                },
                DescriptorBind::StorageImage(ref image, layout) => {
                    assert!(image.usage_storage());
                    self_resources_image_views.push(image.clone());
                    Some(vk::DescriptorImageInfo {
                        sampler: 0,
                        imageView: image.internal_object(),
                        imageLayout: layout as u32,
                    })
                },
                DescriptorBind::SampledImage(ref image, layout) => {
                    assert!(image.usage_sampled());
                    self_resources_image_views.push(image.clone());
                    Some(vk::DescriptorImageInfo {
                        sampler: 0,
                        imageView: image.internal_object(),
                        imageLayout: layout as u32,
                    })
                },
                DescriptorBind::InputAttachment(ref image, layout) => {
                    assert!(image.usage_input_attachment());
                    self_resources_image_views.push(image.clone());
                    Some(vk::DescriptorImageInfo {
                        sampler: 0,
                        imageView: image.internal_object(),
                        imageLayout: layout as u32,
                    })
                },
                _ => None
            }
        }).collect::<Vec<_>>();


        // TODO: allocate on stack instead (https://github.com/rust-lang/rfcs/issues/618)
        let mut next_buffer_desc = 0;
        let mut next_image_desc = 0;

        let vk_writes = write.iter().map(|write| {
            let (buffer_info, image_info) = match write.content {
                DescriptorBind::Sampler(_) | DescriptorBind::CombinedImageSampler(_, _ ,_) |
                DescriptorBind::SampledImage(_, _) | DescriptorBind::StorageImage(_, _) |
                DescriptorBind::InputAttachment(_, _) => {
                    let img = image_descriptors.as_ptr().offset(next_image_desc as isize);
                    next_image_desc += 1;
                    (ptr::null(), img)
                },
                //DescriptorBind::UniformTexelBuffer(_) | DescriptorBind::StorageTexelBuffer(_) =>
                DescriptorBind::UniformBuffer { .. } | DescriptorBind::StorageBuffer { .. } |
                DescriptorBind::DynamicUniformBuffer { .. } |
                DescriptorBind::DynamicStorageBuffer { .. } => {
                    let buf = buffer_descriptors.as_ptr().offset(next_buffer_desc as isize);
                    next_buffer_desc += 1;
                    (buf, ptr::null())
                },
            };

            vk::WriteDescriptorSet {
                sType: vk::STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                pNext: ptr::null(),
                dstSet: self_set,
                dstBinding: write.binding,
                dstArrayElement: write.array_element,
                descriptorCount: 1,
                descriptorType: write.content.ty() as u32,
                pImageInfo: image_info,
                pBufferInfo: buffer_info,
                pTexelBufferView: ptr::null(),      // TODO:
            }
        }).collect::<Vec<_>>();

        debug_assert_eq!(next_buffer_desc, buffer_descriptors.len());
        debug_assert_eq!(next_image_desc, image_descriptors.len());

        if !vk_writes.is_empty() {
            vk.UpdateDescriptorSets(self.pool.device().internal_object(),
                                    vk_writes.len() as u32, vk_writes.as_ptr(), 0, ptr::null());
        }
    }
}

unsafe impl<S> VulkanObject for DescriptorSet<S> {
    type Object = vk::DescriptorSet;

    #[inline]
    fn internal_object(&self) -> vk::DescriptorSet {
        self.set
    }
}

impl<S> Drop for DescriptorSet<S> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let vk = self.pool.device().pointers();
            vk.FreeDescriptorSets(self.pool.device().internal_object(),
                                  self.pool.internal_object(), 1, &self.set);
        }
    }
}


/// Implemented on all `DescriptorSet` objects. Hides the template parameters.
pub unsafe trait AbstractDescriptorSet: ::VulkanObjectU64 {}
unsafe impl<S> AbstractDescriptorSet for DescriptorSet<S> {}

/// Describes the layout of all descriptors within a descriptor set.
pub struct DescriptorSetLayout<S> {
    layout: vk::DescriptorSetLayout,
    device: Arc<Device>,
    description: S,
}

impl<S> DescriptorSetLayout<S> where S: DescriptorSetDesc {
    pub fn new(device: &Arc<Device>, description: S)
               -> Result<Arc<DescriptorSetLayout<S>>, OomError>
    {
        let vk = device.pointers();

        // TODO: allocate on stack instead (https://github.com/rust-lang/rfcs/issues/618)
        let bindings = description.descriptors().into_iter().map(|desc| {
            vk::DescriptorSetLayoutBinding {
                binding: desc.binding,
                descriptorType: desc.ty.vk_enum(),
                descriptorCount: desc.array_count,
                stageFlags: desc.stages.into(),
                pImmutableSamplers: ptr::null(),        // FIXME: not yet implemented
            }
        }).collect::<Vec<_>>();

        let layout = unsafe {
            let infos = vk::DescriptorSetLayoutCreateInfo {
                sType: vk::STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
                pNext: ptr::null(),
                flags: 0,   // reserved
                bindingCount: bindings.len() as u32,
                pBindings: bindings.as_ptr(),
            };

            let mut output = mem::uninitialized();
            try!(check_errors(vk.CreateDescriptorSetLayout(device.internal_object(), &infos,
                                                           ptr::null(), &mut output)));
            output
        };

        Ok(Arc::new(DescriptorSetLayout {
            layout: layout,
            device: device.clone(),
            description: description,
        }))
    }

    #[inline]
    pub fn description(&self) -> &S {
        &self.description
    }
}

unsafe impl<S> VulkanObject for DescriptorSetLayout<S> {
    type Object = vk::DescriptorSetLayout;

    #[inline]
    fn internal_object(&self) -> vk::DescriptorSetLayout {
        self.layout
    }
}

impl<S> Drop for DescriptorSetLayout<S> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let vk = self.device.pointers();
            vk.DestroyDescriptorSetLayout(self.device.internal_object(), self.layout, ptr::null());
        }
    }
}

/// Implemented on all `DescriptorSetLayout` objects. Hides the template parameters.
pub unsafe trait AbstractDescriptorSetLayout: ::VulkanObjectU64 {}
unsafe impl<S> AbstractDescriptorSetLayout for DescriptorSetLayout<S> {}

/// A collection of `DescriptorSetLayout` structs.
// TODO: push constants.
pub struct PipelineLayout<P> {
    device: Arc<Device>,
    layout: vk::PipelineLayout,
    description: P,
    layouts: Vec<Arc<AbstractDescriptorSetLayout>>,     // TODO: is it necessary to keep the layouts alive? check the specs
}

impl<P> PipelineLayout<P> where P: PipelineLayoutDesc {
    /// Creates a new `PipelineLayout`.
    pub fn new(device: &Arc<Device>, description: P, layouts: P::DescriptorSetLayouts)
               -> Result<Arc<PipelineLayout<P>>, OomError>
    {
        let vk = device.pointers();

        let layouts = description.decode_descriptor_set_layouts(layouts);
        // TODO: allocate on stack instead (https://github.com/rust-lang/rfcs/issues/618)
        let layouts_ids = layouts.iter().map(|l| {
            // FIXME: check that they belong to the same device
            ::VulkanObjectU64::internal_object(&**l)
        }).collect::<Vec<_>>();

        let layout = unsafe {
            let infos = vk::PipelineLayoutCreateInfo {
                sType: vk::STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
                pNext: ptr::null(),
                flags: 0,   // reserved
                setLayoutCount: layouts_ids.len() as u32,
                pSetLayouts: layouts_ids.as_ptr(),
                pushConstantRangeCount: 0,      // TODO: unimplemented
                pPushConstantRanges: ptr::null(),    // TODO: unimplemented
            };

            let mut output = mem::uninitialized();
            try!(check_errors(vk.CreatePipelineLayout(device.internal_object(), &infos,
                                                      ptr::null(), &mut output)));
            output
        };

        Ok(Arc::new(PipelineLayout {
            device: device.clone(),
            layout: layout,
            description: description,
            layouts: layouts,
        }))
    }

    #[inline]
    pub fn description(&self) -> &P {
        &self.description
    }
}

unsafe impl<P> VulkanObject for PipelineLayout<P> {
    type Object = vk::PipelineLayout;

    #[inline]
    fn internal_object(&self) -> vk::PipelineLayout {
        self.layout
    }
}

impl<P> Drop for PipelineLayout<P> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let vk = self.device.pointers();
            vk.DestroyDescriptorSetLayout(self.device.internal_object(), self.layout, ptr::null());
        }
    }
}
