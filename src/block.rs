/// 虚拟块设备前端驱动
/// ref: https://github.com/rcore-os/virtio-drivers/blob/master/src/blk.rs
/// thanks!
/// 
/// BlockFuture 的 Send 和 Sync：
/// + inner 成员是 Send 和 Sync 的，_req_type 和 response 成员暂时不会用到，因此从成员变量看来是 Send 和 Sync 的
/// + poll 方法借助了 Mutex 实现内部可变性，在并发场景下多个 poll 操作一起运行的时候，有锁机制保证操作的原子性，因此是 Sync 的
/// 因此个人觉得 BLockFuture 是 Send 和 Sync 的
/// 
/// VirtioBlock 设计需求分析：
/// + 需要在并发场景下执行 async_read 或 async_write 或 ack_interrupt 操作，
/// 因此这三个方法都必须是 &self 而不能是 &mut self，因此通过 Mutex 提供内部可变性，并保证并发安全
/// + 需要想清楚哪些操作必须是原子的，必须按顺序来，否则会出问题
/// + 比如多个协程都需要执行 async_read，这时候需要往虚拟队列中添加描述符，然后通知设备，
/// 如果添加描述符和通知设备两个操作不是原子的话，可能会出问题。（这里可能两个操作不应该是原子的，只是举个例子，说明系统里面可能会有这样的情况）
/// 
/// todo: 弄清楚哪些操作需要同步，哪些部分需要加锁


use bitflags::bitflags;
use volatile::Volatile;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::ptr::NonNull;
use spin::Mutex;
use alloc::sync::Arc;
use super::mmio::VirtIOHeader;
use super::queue::VirtQueue;
use super::util::AsBuf;
use super::config::*;
use super::*;

pub struct BlockFuture {
    /// 请求类型
    /// 0 表示读，1 表示写
    /// unused
    _req_type: u8,
    /// 该块设备的内部结构，用于 poll 操作的时候判断请求是否完成
    /// 如果完成了也会对这里的值做相关处理
    inner: Arc<Mutex<VirtIOBlockInner>>,
    /// 块设备的回应，用于 poll 操作的时候从这里读取请求被处理的状态
    /// unused
    response: NonNull<()>,
}

impl Future for BlockFuture {
    type Output = Result<()>;
    // warn: 这里需要仔细考虑操作的原子性
    // 目前的飓风内核里面的实现是对所有 Future 的操作加锁了，因此暂时不用考虑
    // 将来的正式发布版本需要考虑这个问题
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // 这里对 status 的判断有很奇怪的 bug，先不对其进行判断
        let resp: NonNull<BlockResp> = self.response.cast();
        let status = unsafe { *&resp.as_ref().status };
        let mut inner = self.inner.lock();
        let (_h, q) = inner.header_and_queue_mut();
        match q.can_pop() {
            true => {
                q.pop_used()?;
                Poll::Ready(Ok(()))
            }
            false => {
                // 这里不进行唤醒，直接返回 pending
                // 外部中断到来的时候在内核里面唤醒
                Poll::Pending
            }
        }
        
    }
}

unsafe impl Send for BlockFuture {}
unsafe impl Sync for BlockFuture {}

/// 虚拟块设备
pub struct VirtIOBlock {
    /// 块设备的内部内容
    inner: Arc<Mutex<VirtIOBlockInner>>,
    /// 容量
    capacity: usize
}

/// 并发场景中经常需要 VirtIOHeader 和 VirtQueue 共同完成一些原子操作
/// 因此把这两者放到一个结构体里面
pub struct VirtIOBlockInner {
    /// MMIO 头部
    pub header: &'static mut VirtIOHeader,
    /// 虚拟队列
    pub queue: VirtQueue
}

impl VirtIOBlockInner {
    pub fn header_and_queue(&self) -> (&VirtIOHeader, &VirtQueue) {
        (self.header, &self.queue)
    }

    pub fn header_and_queue_mut(&mut self) -> (&mut VirtIOHeader, &mut VirtQueue) {
        (&mut self.header, &mut self.queue)
    }
}

impl VirtIOBlock {
    /// 以异步方式创建虚拟块设备驱动
    pub async fn async_new(header: &'static mut VirtIOHeader) -> Result<VirtIOBlock> {
        if !header.verify() {
            return Err(VirtIOError::HeaderVerifyError);
        }
        header.begin_init(|f| {
            let features = BlockFeature::from_bits_truncate(f);
            println!("[virtio] block device features: {:?}", features);
            // 对这些 features 进行谈判
            let supported_featuers = BlockFeature::empty();
            (features & supported_featuers).bits()
        });

        // 读取配置空间
        let config = unsafe {
            &mut *(header.config_space() as *mut BlockConfig)
        };
        println!("[virtio] config: {:?}", config);
        println!(
            "[virtio] found a block device of size {} KB",
            config.capacity.read() / 2
        );

        let queue = VirtQueue::async_new(
            header, 0, VIRT_QUEUE_SIZE as u16
        ).await?;

        header.finish_init();

        let inner = VirtIOBlockInner { header, queue };

        Ok(VirtIOBlock {
            inner: Arc::new(Mutex::new(inner)),
            capacity: config.capacity.read() as usize
        })
    }

    pub fn new(header: &'static mut VirtIOHeader) -> Result<Self> {
        if !header.verify() {
            return Err(VirtIOError::HeaderVerifyError);
        }
        header.begin_init(|f| {
            let features = BlockFeature::from_bits_truncate(f);
            println!("[virtio] block device features: {:?}", features);
            // 对这些 features 进行谈判
            let supported_featuers = BlockFeature::empty();
            (features & supported_featuers).bits()
        });

        // 读取配置空间
        let config = unsafe {
            &mut *(header.config_space() as *mut BlockConfig)
        };
        println!("[virtio] config: {:?}", config);
        println!(
            "[virtio] found a block device of size {} KB",
            config.capacity.read() / 2
        );

        let queue = VirtQueue::new(
            header, 0, VIRT_QUEUE_SIZE as u16
        )?;

        header.finish_init();

        let inner = VirtIOBlockInner { header, queue };
        Ok(VirtIOBlock {
            inner: Arc::new(Mutex::new(inner)),
            capacity: config.capacity.read() as usize
        })
    }

    /// 通知设备 virtio 外部中断已经处理完成
    pub fn ack_interrupt(&self) -> bool {
        self.inner.lock().header.ack_interrupt()
    }

    /// 以异步方式读取一个块
    /// todo: 仔细考虑这里的操作原子性
    pub fn async_read(&self, block_id: usize, buf: &mut [u8]) -> BlockFuture {
        if buf.len() != BLOCK_SIZE {
            panic!("[virtio] buffer size must equal to block size - 512!");
        }
        let req = BlockReq {
            type_: BlockReqType::In,
            reserved: 0,
            sector: block_id as u64
        };
        let mut resp = BlockResp::default();
        let mut inner = self.inner.lock();
        let (h, q) = inner.header_and_queue_mut();
        q.add_buf(&[req.as_buf()], &[buf, resp.as_buf_mut()])
            .expect("[virtio] virtual queue add buf error");
    
        h.notify(0);

        BlockFuture {
            _req_type: 0,
            inner: Arc::clone(&self.inner),
            response: NonNull::new(&resp as *const _ as *mut ()).unwrap()
        }
    }

    /// 以异步方式写入一个块
    /// todo: 仔细考虑这里的操作原子性
    pub fn async_write(&self, block_id: usize, buf: &[u8]) -> BlockFuture {
        if buf.len() != BLOCK_SIZE {
            panic!("[virtio] buffer size must equal to block size - 512!");
        }
        let req = BlockReq {
            type_: BlockReqType::Out,
            reserved: 0,
            sector: block_id as u64,
        };
        let mut resp = BlockResp::default();
        let mut inner = self.inner.lock();
        let (h, q) = inner.header_and_queue_mut();
        q.add_buf(&[req.as_buf(), buf], &[resp.as_buf_mut()])
            .expect("[virtio] virtual queue add buf error");

        h.notify(0);
        
        BlockFuture {
            _req_type: 1,
            inner: Arc::clone(&self.inner),
            response: NonNull::new(&resp as *const _ as *mut ()).unwrap()
        }
    }

    pub fn read_block(&self, block_id: usize, buf: &mut [u8]) -> Result<()> {
        if buf.len() != BLOCK_SIZE {
            panic!("[virtio] buffer size must equal to block size - 512!");
        }
        let req = BlockReq {
            type_: BlockReqType::In,
            reserved: 0,
            sector: block_id as u64,
        };
        let mut resp = BlockResp::default();

        let mut inner = self.inner.lock();
        let (h, q) = inner.header_and_queue_mut();

        q.add_buf(&[req.as_buf()], &[buf, resp.as_buf_mut()])
            .expect("[virtio] virtual queue add buf error");
        
        h.notify(0);
        
        while !q.can_pop() {}
        q.pop_used()?;
        match resp.status {
            BlockRespStatus::Ok => Ok(()),
            _ => Err(VirtIOError::IOError)
        }
    }

    pub fn write_block(&self, block_id: usize, buf: &[u8]) -> Result<()> {
        if buf.len() != BLOCK_SIZE {
            panic!("[virtio] buffer size must equal to block size - 512!");
        }
        let req = BlockReq {
            type_: BlockReqType::Out,
            reserved: 0,
            sector: block_id as u64,
        };
        let mut resp = BlockResp::default();
        
        let mut inner = self.inner.lock();
        let (h, q) = inner.header_and_queue_mut();

        q.add_buf(&[req.as_buf(), buf], &[resp.as_buf_mut()])
            .expect("[virtio] virtual queue add buf error");
        
        h.notify(0);
        
        while !q.can_pop() {}
        q.pop_used()?;
        match resp.status {
            BlockRespStatus::Ok => Ok(()),
            _ => Err(VirtIOError::IOError)
        }
    }

    /// 处理 virtio 外部中断
    /// todo: 仔细考虑这里的操作原子性
    pub unsafe fn handle_interrupt(&self) -> Result<InterruptRet> {
        let mut inner = self.inner.lock();
        let (h, q) = inner.header_and_queue_mut();
        if !q.can_pop() {
            return Err(VirtIOError::IOError);
        }
        h.ack_interrupt();
        let (index, _len) = q.next_used()?;
        let desc = q.descriptor(index as usize);
        let desc_va = virtio_phys_to_virt(desc.paddr.read() as usize);
        let req = &*(desc_va as *const BlockReq);
        let ret = match req.type_ {
            BlockReqType::In => InterruptRet::Read(req.sector as usize),
            BlockReqType::Out => InterruptRet::Write(req.sector as usize),
            _ => InterruptRet::Other
        };
        Ok(ret)
    }
}

bitflags! {
    struct BlockFeature: u64 { 
        /// Device supports request barriers. (legacy)
        const BARRIER       = 1 << 0;
        /// Maximum size of any single segment is in `size_max`.
        const SIZE_MAX      = 1 << 1;
        /// Maximum number of segments in a request is in `seg_max`.
        const SEG_MAX       = 1 << 2;
        /// Disk-style geometry specified in geometry.
        const GEOMETRY      = 1 << 4;
        /// Device is read-only.
        const RO            = 1 << 5;
        /// Block size of disk is in `blk_size`.
        const BLK_SIZE      = 1 << 6;
        /// Device supports scsi packet commands. (legacy)
        const SCSI          = 1 << 7;
        /// Cache flush command support.
        const FLUSH         = 1 << 9;
        /// Device exports information on optimal I/O alignment.
        const TOPOLOGY      = 1 << 10;
        /// Device can toggle its cache between writeback and writethrough modes.
        const CONFIG_WCE    = 1 << 11;
        /// Device can support discard command, maximum discard sectors size in
        /// `max_discard_sectors` and maximum discard segment number in
        /// `max_discard_seg`.
        const DISCARD       = 1 << 13;
        /// Device can support write zeroes command, maximum write zeroes sectors
        /// size in `max_write_zeroes_sectors` and maximum write zeroes segment
        /// number in `max_write_zeroes_seg`.
        const WRITE_ZEROES  = 1 << 14;

        // device independent
        const NOTIFY_ON_EMPTY       = 1 << 24; // legacy
        const ANY_LAYOUT            = 1 << 27; // legacy
        const RING_INDIRECT_DESC    = 1 << 28;
        const RING_EVENT_IDX        = 1 << 29;
        const UNUSED                = 1 << 30; // legacy
        const VERSION_1             = 1 << 32; // detect legacy

        // the following since virtio v1.1
        const ACCESS_PLATFORM       = 1 << 33;
        const RING_PACKED           = 1 << 34;
        const IN_ORDER              = 1 << 35;
        const ORDER_PLATFORM        = 1 << 36;
        const SR_IOV                = 1 << 37;
        const NOTIFICATION_DATA     = 1 << 38;
    }
}

/// 块设备配置
#[repr(C)]
#[derive(Debug)]
struct BlockConfig {
    /// 扇区数目
    capacity: Volatile<u64>,
    size_max: Volatile<u32>,
    seg_max: Volatile<u32>,
    cylinders: Volatile<u16>,
    heads: Volatile<u8>,
    sectors: Volatile<u8>,
    /// 扇区大小
    sector_size: Volatile<u32>,
    physical_block_exp: Volatile<u8>,
    alignment_offset: Volatile<u8>,
    min_io_size: Volatile<u16>,
    opt_io_size: Volatile<u32>,
    // ... ignored
}

/// 块设备请求
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct BlockReq {
    type_: BlockReqType,
    reserved: u32,
    sector: u64
}

/// 块设备回应
#[repr(C)]
#[derive(Debug)]
struct BlockResp {
    status: BlockRespStatus,
}

/// 块设备请求类型
#[repr(C)]
#[derive(Debug, Clone, Copy)]
enum BlockReqType {
    In = 0,
    Out = 1,
    Flush = 4,
    Discard = 11,
    WriteZeroes = 13,
}

/// 块设备回应状态
#[repr(u8)]
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum BlockRespStatus {
    Ok = 0,
    IoErr = 1,
    Unsupported = 2,
    NotReady = 3,
}

impl Default for BlockResp {
    fn default() -> Self {
        BlockResp {
            status: BlockRespStatus::NotReady,
        }
    }
}

unsafe impl AsBuf for BlockReq {}
unsafe impl AsBuf for BlockResp {}

/// 中断响应返回值
pub enum InterruptRet {
    /// 读请求完成的块
    Read(usize),
    /// 写请求完成的块
    Write(usize),
    /// 其他
    Other
}

extern "C" {
    /// 内核提供的物理地址到虚拟地址的转换函数
    fn virtio_phys_to_virt(paddr: usize) -> usize;
}