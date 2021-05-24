# 异步版 VIRTIO 之块设备驱动实现

## 前言
笔者初步实现了一款基于 Rust 语言异步语法的 virtio 块设备驱动，目前在 qemu 平台上结合无相之风团队开发的[飓风内核](https://github.com/HUST-OS/tornado-os)，可以像下面的伪代码一样运行块设备读写任务:  
```Rust
// 下面的接口定义可能和库的具体实现有些差别
pub async fn virtio_test() -> async_driver::Result<()> {
    // 创建 virtio 异步块设备实例
    let async_blk = VirtIOAsyncBlock::new().await?;
    // 读缓冲区，可变，大小为一个扇区（512 字节）
    let mut read_buf = [0u8; 512];
    // 写缓冲区，不可变，大小为一个扇区（512 字节）
    let write_buf = [1u8; 512];
    // 把写缓冲中的数据写到虚拟设备的 0 号扇区上
    async_blk.async_write(0, &write_buf).await?;
    // 从虚拟设备的 0 号扇区上读取数据到读缓冲
    async_blk.async_read(0, &mut read_buf).await?;
    // 比较读写结果
    assert_eq!(read_buf, write_buf);
    // 成功
    Ok(())
}
```

Rust 编译器会将上面的这个异步函数转换成一个 Future，然后这个 Future 会在飓风内核里面被包装成一个**任务**，放到共享调度器里面去运行，伪代码如下：  
```Rust
// 下面的接口定义和具体实现不一致
#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    // 创建一个内核任务
    let task = KernelTask::new(virtio_test());
    // 将任务放入到共享调度器
    shared_scheduler.add_task(task);
    // 执行器执行任务
    Runtime::executor::run_until_idle();
    // 系统退出
    System::shutdown();
}
```
本项目大量参考了[rCore社区](https://github.com/rcore-os)中的[virtio-drivers](https://github.com/rcore-os/virtio-drivers)项目，感谢 rCore 社区！  
笔者在写该项目之前学习了 rCore 教程第三版中关于设备驱动的[章节](https://rcore-os.github.io/rCore-Tutorial-Book-v3/chapter8/2device-driver-2.html)，里面有对 virtio 基础知识非常详尽的讲解，因此本文档不会重复讲述这部分内容，非常推荐读者去浏览一下 rCore 教程相关章节。  

## 初探 virtio
首先我们来思考两个问题：  

1. 什么是 virtio
2. 为什么需要 virtio

第一个问题，virtio 是半虚拟化场景中的一个 IO 传输标准，它的标准文档可以在[这里](https://docs.oasis-open.org/virtio/virtio/v1.1/csprd01/virtio-v1.1-csprd01.html)找到。可以理解为半虚拟化场景中客户机和硬件资源之间的数据传输需要一些通用标准（或协议）来完成，virtio 就是这样一套标准。  
然后第二个问题，virtio 如上所述是一个标准，标准一般会有个作用就是提高兼容性，减少开发工作量。对于 virtio 来说，如果没有这个标准的话，那么开发者就需要为每个虚拟设备单独实现一个设备驱动，如果有 N 个虚拟设备，每个驱动程序的平均代码量是 M，那就需要开发者贡献 N x M 大小的代码量。但是如果有 virtio 标准的话，开发者就可以针对这个标准写一套框架（假设框架的代码量为 A），然后再基于这个框架去写每个虚拟设备的驱动程序（平均代码量为 M - A），这样一来总代码量就是 A + N x (M - A) = N x M - (N - 1) x A，因此在虚拟设备数量较多的情况下，该标准可以减少开发者们的工作量。  

virtio 架构大概可以分为三层：  
+ 操作系统中各种设备前端驱动
+ 中间层，数据传输的桥梁
+ 后端虚拟设备

> 网上很多博客会分四层，中间是 virtio 和 virtio-ring，但笔者写代码的时候没感受到中间分为两层的必要，因此这里分为三层，个人感觉这个区别不影响实现

设备前端驱动有块设备驱动(virtio_blk)，网卡驱动(virtio-net)等等，后端就是各种具体的硬件实现。笔者实现的是前端驱动部分。更多详细信息请查阅 rCore 教程第三版。  


## virtio 块设备架构图
笔者实现的是各种虚拟设备驱动中的块设备驱动，下面是一张架构图，其中包含一些具体实现：  


## BlockFuture 的设计

## 在飓风内核上运行