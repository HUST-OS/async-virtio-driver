//! 异步 virtio 前端驱动
#![no_std]
#![feature(llvm_asm)]

mod sbi;
mod log;
mod config;
mod util;
mod dma;
mod queue;
mod mmio;

pub type Result<T = ()> = core::result::Result<T, VirtIOError>;

/// 虚拟设备错误
pub enum VirtIOError {
    /// 申请 DMA 空间分配错误
    DMAAllocError,
    /// 虚拟队列已经被占用
    QueueInUsed(usize),
    /// 非法参数
    InvalidParameter,
    /// 溢出
    Overflow,
    /// 已用环没准备好
    UsedRingNotReady
}
