/// 虚拟队列相关实现
/// ref: https://github.com/rcore-os/virtio-drivers/blob/master/src/queue.rs
/// thanks!

use volatile::Volatile;
use bitflags::bitflags;
use core::{mem::size_of, sync::atomic::{fence, Ordering}};
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
    /// 已经使用的描述符数目
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

    pub async fn async_new<'virtio>(
        header: &mut VirtIOHeader, index: usize, size: u16
    ) -> Result<VirtQueue<'virtio>> {
        if header.queue_used(index as u32) {
            return Err(VirtIOError::QueueInUsed(index));
        }
        if !size.is_power_of_two() || header.max_queue_size() < size as u32 {
            return Err(VirtIOError::InvalidParameter);
        }
        let queue_layout = VirtQueueMemLayout::new(size);
        let dma = DMA::alloc(queue_layout.mem_size / PAGE_SIZE).await;
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

    pub fn print_desc_table(&self) {
        self.descriptor_table.iter().for_each(|x| {
            println!("{:#x?}", x);
        });
    }
    pub fn print_avail_ring(&self) {
        self.avail_ring.ring.iter().for_each(|x| {
            print!("{} ", x.read());
        });
        println!("");
    }

    pub fn print_used_ring(&self) {
        self.used_ring.ring.iter().for_each(|x| {
            print!("{:#x?} ", x);
        });
        println!("");
    }

    /// 添加 buffers 到虚拟队列，返回一个 token
    pub fn add_buf(&mut self, inputs: &[&[u8]], outputs: &[&mut [u8]]) -> Result<u16> {
        if inputs.is_empty() && outputs.is_empty() {
            return Err(VirtIOError::InvalidParameter);
        }
        if inputs.len() + outputs.len() + self.used_num as usize > self.queue_size as usize {
            // buffer 数量溢出
            return Err(VirtIOError::Overflow);
        }

        // 从空闲描述符表中分配描述符
        let head = self.free_desc_head;
        let mut tail = self.free_desc_head;
        inputs.iter().for_each(|input| {
            let desc = &mut self.descriptor_table[self.free_desc_head as usize];
            // 将 buffer 的信息写入描述符
            desc.set_buf(input);
            // 设置描述符的标识位
            desc.flags.write(DescriptorFlags::NEXT);
            tail = self.free_desc_head;
            self.free_desc_head = desc.next.read();
        });
        outputs.iter().for_each(|output| {
            let desc = &mut self.descriptor_table[self.free_desc_head as usize];
            desc.set_buf(output);
            desc.flags.write(DescriptorFlags::NEXT | DescriptorFlags::WRITE);
            tail = self.free_desc_head;
            self.free_desc_head = desc.next.read();
        });
        // 清除描述符链的最后一个元素的 next 指针
        {
            let desc = &mut self.descriptor_table[tail as usize];
            let mut flags = desc.flags.read();
            flags.remove(DescriptorFlags::NEXT);
            desc.flags.write(flags);
        }
        // 更新已使用描述符数目
        self.used_num += (inputs.len() + outputs.len()) as u16;

        // 将描述符链的头部放入可用环中
        let avail_slot = self.avail_index & (self.queue_size - 1);
        self.avail_ring.ring[avail_slot as usize].write(head);
        
        // write barrier(内存屏障操作？)
        fence(Ordering::SeqCst);

        // 更新可用环的头部
        self.avail_index = self.avail_index.wrapping_add(1);
        self.avail_ring.idx.write(self.avail_index);
        Ok(head)
    }

    /// 是否可以从可用环中弹出没处理的项
    pub fn can_pop(&self) -> bool {
        self.last_used_index != self.used_ring.idx.read()
    }

    /// 可用的空闲描述符数量
    pub fn free_desc_num(&self) -> usize {
        (self.queue_size - self.used_num) as usize
    }

    /// 回收描述符
    /// 该方法将会把需要回收的描述符链放到空闲描述符链的头部
    fn recycle_descriptors(&mut self, mut head: u16) {
        let origin_desc_head = self.free_desc_head;
        self.free_desc_head = head;
        loop {
            let desc = &mut self.descriptor_table[head as usize];
            let flags = desc.flags.read();
            self.used_num -= 1;
            if flags.contains(DescriptorFlags::NEXT) {
                head = desc.next.read();
            } else {
                desc.next.write(origin_desc_head);
                return;
            }
        }
    }

    /// 从已用环中弹出一个 token，并返回长度
    /// ref: linux virtio_ring.c virtqueue_get_buf_ctx
    pub fn pop_used(&mut self) -> Result<(u16, u32)> {
        if !self.can_pop() {
            return Err(VirtIOError::UsedRingNotReady);
        }
        // read barrier
        fence(Ordering::SeqCst);

        let last_used_slot = self.last_used_index &  (self.queue_size - 1);
        let index = self.used_ring.ring[last_used_slot as usize].id.read() as u16;
        let len = self.used_ring.ring[last_used_slot as usize].len.read();

        self.recycle_descriptors(index);
        self.last_used_index = self.last_used_index.wrapping_add(1);

        Ok((index, len))
    }

    /// 从已用环中取出下一个 token，但不弹出
    pub fn next_used(&self) -> Result<(u16, u32)> {
        if !self.can_pop() {
            return Err(VirtIOError::UsedRingNotReady);
        }

        // read barrier
        fence(Ordering::SeqCst);

        let last_used_slot = self.last_used_index &  (self.queue_size - 1);
        let index = self.used_ring.ring[last_used_slot as usize].id.read() as u16;
        let len = self.used_ring.ring[last_used_slot as usize].len.read();

        Ok((index, len))
    }

    pub fn descriptor(&self, index: usize) -> Descriptor {
        self.descriptor_table[index].clone()
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

impl Clone for Descriptor {
    fn clone(&self) -> Self {
        Self {
            paddr : Volatile::<u64>::new(self.paddr.read()),
            len : Volatile::<u32>::new(self.len.read()),
            flags : Volatile::<DescriptorFlags>::new(self.flags.read()),
            next : Volatile::<u16>::new(self.next.read()),
        }
    }
}

impl Descriptor {
    /// 把特定 buffer 的信息写入到描述符
    fn set_buf(&mut self, buf: &[u8]) {
        let buf_paddr = unsafe {
            virtio_virt_to_phys(buf.as_ptr() as usize) as u64
        };
        self.paddr.write(buf_paddr);
        self.len.write(buf.len() as u32);
    }
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

// #[repr(C)]
// #[derive(Debug)]
// struct UsedRing {
//     /// 与通知机制相关
//     flags: Volatile<u16>,
//     idx: Volatile<u16>,
//     ring:  [UsedElement; VIRT_QUEUE_SIZE],
//     // unused
//     event: Volatile<u16>
// }

// #[repr(C)]
// #[derive(Debug)]
// struct AvailableRing {
//     /// 与通知机制相关
//     flags: Volatile<u16>,
//     idx: Volatile<u16>,
//     ring:  [Volatile<u16>; VIRT_QUEUE_SIZE],
//     // unused
//     event: Volatile<u16>
// }

/// 已用环中的项
#[repr(C)]
#[derive(Debug)]
struct UsedElement {
    id: Volatile<u32>,
    len: Volatile<u32>
}

extern "C" {
    fn virtio_virt_to_phys(vaddr: usize) -> usize;
}