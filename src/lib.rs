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
mod block;

pub type Result<T = ()> = core::result::Result<T, VirtIOError>;

/// 虚拟设备错误
#[derive(Debug)]
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
    UsedRingNotReady,
    /// Header 检查错误
    HeaderVerifyError,
    /// 数据传输错误
    /// 出现在虚拟设备返回一个状态不是 Ok 的回应
    /// 和数据没准备好却进入了外部中断处理方法
    IOError,
}
