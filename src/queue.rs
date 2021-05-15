/// 虚拟队列相关实现
/// ref: https://github.com/rcore-os/virtio-drivers/blob/master/src/queue.rs
/// thanks!

use volatile::Volatile;
use bitflags::bitflags;
use core::mem::size_of;
use crate::util::align_up_page;

use super::dma::DMA;
use super::mmio::VirtIOHeader;
use super::config::*;
use super::*;

type AvailableRing = Ring<Volatile<u16>>;
type UsedRing = Ring<UsedElement>;

/// Virtio 中的虚拟队列接口，前后端通信的桥梁
#[repr(C)]
pub struct VirtQueue<'virtio> {
    /// DMA 空间
    dma: DMA,
    /// 描述符表
    descriptor_table: &'virtio mut [Descriptor],
    /// 可用环
    avail_ring: &'virtio mut AvailableRing,
    /// 已用环
    used_ring: &'virtio mut UsedRing,
    /// 虚拟队列索引值
    /// 一个虚拟设备实现可能有多个虚拟队列
    queue_index: u32,
    /// 虚拟队列长度
    queue_size: u16,
    /// 已经使用的队列项目数
    used_num: u16,
    /// 空闲描述符链表头
    /// 初始时所有描述符通过 next 指针依次相连形成空闲链表
    free_desc_head: u16,
    /// 可用环的索引值
    avail_index: u16,
    /// 设备上次已取的已用环元素的位置
    last_used_index: u16
}

impl VirtQueue<'_> {
    pub fn new(
        header: &mut VirtIOHeader, index: usize, size: u16
    ) -> Result<Self> {
        if header.queue_used(index as u32) {
            return Err(VirtIOError::QueueInUsed(index));
        }
        if !size.is_power_of_two() || header.max_queue_size() < size as u32 {
            return Err(VirtIOError::InvalidParameter);
        }
        let queue_layout = VirtQueueMemLayout::new(size);
        let dma = DMA::new(queue_layout.mem_size / PAGE_SIZE)?;
        println!("[virtio] DMA address: {:#x}", dma.start_physical_address());

        // 在 MMIO 接口中设置虚拟队列的相关信息
        header.queue_set(
            index as u32, size as u32, PAGE_SIZE as u32, dma.ppn() as u32
        );

        // 描述符表起始地址
        let desc_table = unsafe {
            core::slice::from_raw_parts_mut(dma.start_virtual_address() as *mut Descriptor, size as usize)
        };
        // 可用环起始地址
        let avail_ring = unsafe {
            &mut *((dma.start_virtual_address() + queue_layout.avail_ring_offset) as *mut AvailableRing)
        };
        // 已用环起始地址
        let used_ring = unsafe {
            &mut *((dma.start_virtual_address() + queue_layout.used_ring_offset) as *mut UsedRing)
        };

        // 将空描述符连成链表
        for i in 0..(size - 1) {
            desc_table[i as usize].next.write(i + 1);
        }

        Ok(VirtQueue {
            dma,
            descriptor_table: desc_table,
            avail_ring,
            used_ring,
            queue_size: size,
            queue_index: index as u32,
            used_num: 0,
            free_desc_head: 0,
            avail_index: 0,
            last_used_index: 0
        })
    }

    
}

/// 虚拟队列内存布局信息
struct VirtQueueMemLayout {
    /// 可用环地址偏移
    avail_ring_offset: usize,
    /// 已用环地址偏移
    used_ring_offset: usize,
    /// 总大小
    mem_size: usize
}

impl VirtQueueMemLayout {
    fn new(queue_size: u16) -> Self {
        assert!(
            queue_size.is_power_of_two(),
            "[virtio] queue size must be a power off 2"
        );
        let q_size = queue_size as usize;
        let descriptors_size = size_of::<Descriptor>() * q_size;
        let avail_ring_size = size_of::<u16>() * (3 + q_size);
        let used_ring_size = size_of::<u16>() * 3 + size_of::<UsedElement>() * q_size;
        VirtQueueMemLayout {
            avail_ring_offset: descriptors_size,
            used_ring_offset: align_up_page(descriptors_size + avail_ring_size),
            mem_size: align_up_page(descriptors_size + avail_ring_size) + align_up_page(used_ring_size)
        }
    }
}

/// 描述符
#[repr(C, align(16))]
#[derive(Debug)]
pub struct Descriptor {
    /// buffer 的物理地址
    pub paddr: Volatile<u64>,
    /// buffer 的长度
    len: Volatile<u32>,
    /// 标识
    flags: Volatile<DescriptorFlags>,
    /// 下一个描述符的指针
    next: Volatile<u16>
}

bitflags! {
    /// 描述符的标识
    struct DescriptorFlags: u16 {
        const NEXT = 1;
        const WRITE = 2;
        const INDIRECT = 4;
    }
}


/// 环
/// 通过泛型对可用环和已用环进行统一抽象
#[repr(C)]
#[derive(Debug)]
struct Ring<Entry: Sized> {
    /// 与通知机制相关
    flags: Volatile<u16>,
    idx: Volatile<u16>,
    ring:  [Entry; VIRT_QUEUE_SIZE],
    // unused
    event: Volatile<u16>
}

/// 已用环中的项
#[repr(C)]
#[derive(Debug)]
struct UsedElement {
    id: Volatile<u32>,
    len: Volatile<u32>
}
