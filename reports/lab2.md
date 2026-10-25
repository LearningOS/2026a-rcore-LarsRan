# rCore 第四章实验报告

## 一、实验内容与目标

本章的主题是 **地址空间（Address Space）**。在第三章中，所有任务共享同一个地址空间：内核为所有用户程序只建立了一张页表，任何一个用户程序都能访问其它程序的内存，这既不安全，也无法支持本章新增的系统调用。

本章要求：

1. 为**每个任务**建立独立的地址空间（独立的 `MemorySet` 与页表），使内核访问用户内存时必须经过页表翻译；
2. 实现 `mmap` / `munmap` 系统调用，支持用户程序动态申请与释放虚拟内存；
3. 完善 `sys_trace`，使其在读写用户内存时**检查权限**（可读 / 可写），而不是简单地忽略越界；
4. 保持 `sys_sbrk` 的堆管理语义正确，并在释放堆页后能真正触发缺页异常。

本章涉及的测试程序为 `ch4_mmap0/1/2/3`、`ch4_unmap`、`ch4_unmap2`、`ch4_trace1`、`ch4b_sbrk`，同时需要保证第三章的 `ch3_sleep`、`ch3_sleep1`、`ch3_trace` 仍然通过。

## 二、独立地址空间的实现

### 2.1 关键结构

每个 `TaskControlBlock`（TCB）内持有一个属于自己的 `MemorySet`：

```rust
pub struct TaskControlBlock {
    pub task_cx: TaskContext,
    pub task_status: TaskStatus,
    /// Application address space
    pub memory_set: MemorySet,
    /// The phys page number of trap context
    pub trap_cx_ppn: PhysPageNum,
    pub base_size: usize,
    pub heap_bottom: usize,
    pub program_brk: usize,
    /// Syscall count array
    pub syscall_count: [usize; 500],
}
```

`MemorySet` 内部由「三级页表 `PageTable`」和「一串 `MapArea`」组成：

* `PageTable` 是 SV39 三级页表，根表项给出该地址空间的 token（`satp` 值）；
* `MapArea` 描述一段连续的虚拟地址区间 `[vpn_range]`，并记录该区间使用的物理页帧（`data_frames`）与权限位（`map_perm`）。

### 2.2 satp 的切换

地址空间的核心在于 `satp` 寄存器。由于每个任务的页表不同，陷入与返回时必须切换 `satp`：

* **陷入内核**（`trap.S` 的 `__alltraps`）：从 `TrapContext` 的第 34 个槽位读出 `kernel_satp`，执行 `csrw satp, t0` 并 `sfence.vma`，此后内核运行在内核地址空间中；
* **返回用户**（`trap.S` 的 `__restore`）：`trap_return()` 把 `current_user_token()` 作为 `a1` 传入，`__restore` 执行 `csrw satp, a1` 切回用户地址空间。

`TrapContext` 中同时保存了 `kernel_satp`、`kernel_sp` 与 `trap_handler` 的地址，因此**不同任务陷入的是同一个内核陷阱处理函数，但运行在各自的用户地址空间对应的内核栈上**。

### 2.3 内核访问用户内存

由于用户空间与内核空间分离，内核不能再像第三章那样直接解引用用户指针。所有需要访问用户内存的系统调用都必须：

1. 取得当前任务的 token：`current_user_token()`；
2. 用 `PageTable::from_token(token)` 重建一个只读视图；
3. 逐页翻译虚拟地址，检查权限后再取得物理页。

第三章的 `translated_byte_buffer` 内部使用 `translate(vpn).unwrap()`，一旦地址非法就会直接 panic，无法向用户程序返回错误码。因此本章新增了**带检查的版本**：

```rust
pub fn translated_byte_buffer_checked(
    token: usize, ptr: *const u8, len: usize, write: bool,
) -> Option<Vec<&'static mut [u8]> {
    let (start, end) = check_user_addr(ptr as usize, len)?;
    let page_table = PageTable::from_token(token);
    // 逐页：必须 valid、user(U)、且满足 readable / writable，否则返回 None
    ...
}
```

同时补充了两个单字节辅助函数，专供 `sys_trace` 使用：

```rust
pub fn read_user_byte(token: usize, addr: usize) -> Option<u8>;
pub fn write_user_byte(token: usize, addr: usize, data: u8) -> bool;
```

二者在读/写之前都会校验 PTE 的 `V`（有效）、`U`（用户态可访问）、`R`（可读）/ `W`（可写）标志位。

此外还定义了用户虚拟地址上界 `MAX_USER_VA = 1 << 38`（SV39 用户区范围），任何越界地址在翻译前就被拒绝，避免在内核中触发缺页异常。

## 三、mmap / munmap 的设计与语义

### 3.1 语义约定

结合 `ch4_mmap0/1/2/3`、`ch4_unmap`、`ch4_unmap2` 的断言，可以反推出如下语义：

| 情形 | 期望结果 | 原因 |
| --- | --- | --- |
| `start` 未按页对齐 | `-1` | 映射必须以页为单位 |
| `len == 0` | `-1` | 空区间无意义 |
| `prot == 0` | `-1` | 无任何权限的映射非法 |
| `prot` 含 `bit3` 及以上 | `-1` | 只允许定义 `R/W/X` 三位 |
| 与已有映射**按页取整后**重叠 | `-1` | 同一物理页不能被重复映射 |
| 其它合法情形 | `0` | 映射成功 |

`prot` 采用与 Linux 一致的编码：`bit0 = READ`、`bit1 = WRITE`、`bit2 = EXEC`。内核侧把它翻译成 `MapPermission`（`R=1<<1, W=1<<2, X=1<<3, U=1<<4`），与 `PTEFlags` 的位定义完全一致，因此可以直接用 `PTEFlags::from_bits(map_perm.bits)` 转换。

`munmap` 的语义更严格：

* `start` 必须页对齐；
* `len` 必须是**页大小的整数倍**且大于 0；
* 必须**恰好完整覆盖**某个已有映射，否则返回 `-1`。

这解释了 `ch4_unmap2` 中 `munmap(start, len + 1)` 与 `munmap(start + 1, len - 1)` 均返回 `-1`。

### 3.2 关键点：len 必须向上取整到页

这是本章最容易出错的地方。`ch4_mmap3` 与 `ch4_unmap` 对**完全相同**的调用 `mmap(0x0FFF0000, 4097, prot)` 给出了相反的期望：

* `ch4_mmap3` 中 `[0x10000000, 0x10001000)` 仍被占用 → 期望 `-1`；
* `ch4_unmap` 中该区间已被 `munmap` 释放 → 期望 `0`。

要让两者同时成立，唯一可行的解释是：**`len` 向上取整到页的整数倍**。因为 `0x0FFF0000 + 4097 = 0x10000001`，跨越了 `0x10000000` 这一页，因此请求实际覆盖 `0x0FFF0000` 与 `0x10000000` 两页。

* 在 `ch4_mmap3` 中，两页中有一页已被占用 → 重叠 → `-1`；
* 在 `ch4_unmap` 中，两页都已空闲 → 成功 → `0`。

若按向下取整实现，`ch4_mmap3` 会错误地返回 `0`；若把 `len` 直接当页数用，`ch4_unmap` 之后的写循环（覆盖 `0x0FFF0000..0x10003000` 共 20 页）会因缺页而失败。

### 3.3 实现

`MemorySet::mmap_area`：

```rust
pub fn mmap_area(&mut self, start: usize, len: usize, permission: MapPermission) -> bool {
    if start % PAGE_SIZE != 0 || len == 0 { return false; }
    let end = match start.checked_add(len) { Some(e) => e, None => return false };
    if end > crate::mm::MAX_USER_VA { return false; }
    let start_page = start / PAGE_SIZE;
    // len 向上取整到整页
    let mut end_page = (end + PAGE_SIZE - 1) / PAGE_SIZE;
    if end_page <= start_page { end_page = start_page + 1; }
    // 与已有映射按页比较是否重叠
    for area in self.areas.iter() {
        let a = area.vpn_range.get_start().0;
        let b = area.vpn_range.get_end().0;
        if a < b && start_page < b && a < end_page { return false; }
    }
    self.insert_framed_area(
        VirtAddr::from(start_page * PAGE_SIZE),
        VirtAddr::from(end_page * PAGE_SIZE),
        permission,
    );
    true
}
```

重叠检测的条件 `a < b && start_page < b && a < end_page` 是标准的区间相交判定；`a < b` 这一项同时排除了 `from_elf` 中为 `sbrk` 预留的**零长度区域**（`user_stack_top..user_stack_top`），避免它误判冲突。

`MemorySet::munmap_area`：

```rust
pub fn munmap_area(&mut self, start: usize, len: usize) -> bool {
    if start % PAGE_SIZE != 0 || len == 0 || len % PAGE_SIZE != 0 { return false; }
    let end = match start.checked_add(len) { Some(e) => e, None => return false };
    let start_page = start / PAGE_SIZE;
    let end_page = end / PAGE_SIZE;
    let idx = self.areas.iter().position(|area| {
        area.vpn_range.get_start().0 == start_page && area.vpn_range.get_end().0 == end_page
    });
    match idx {
        Some(i) => { let mut area = self.areas.remove(i); area.unmap(&mut self.page_table); true }
        None => false,
    }
}
```

`unmap` 会真正清除对应的 PTE（`PageTable::unmap`）并释放物理页帧（`data_frames.remove`），因此 `ch4_trace1` 中 `munmap` 之后再 `trace_read` 会失败、再写会缺页。

### 3.4 借用与所有权：为什么不能把 `&mut MemorySet` 交出去

`TaskManager` 的任务表被 `UPSafeCell` 包裹，而 `UPSafeCell` 只提供 `new` 与 `exclusive_access()`（返回 `RefMut`），**没有 `get()`**。更关键的是：如果某个辅助函数返回 `&mut MemorySet`，这个借用的生命周期就会逃逸出 `exclusive_access()` 的 `RefMut`，随后再次调用 `exclusive_access()` 会触发 `BorrowMutError` panic。

因此我把入口收敛为「在借用期内完成全部操作」的受控接口：

```rust
fn mmap_current_task(&self, start: usize, len: usize, permission: MapPermission) -> bool {
    let mut inner = self.inner.exclusive_access();
    let cur = inner.current_task;
    inner.tasks[cur].memory_set.mmap_area(start, len, permission)
}
```

`RefMut` 在函数返回时正常 drop，不会有任何 `&mut` 逃逸。

## 四、sys_trace 的权限检查

`sys_trace` 的三种请求：

* `TraceRequest::Read (0)`：读取用户地址 `id` 处的 1 字节 → 用 `read_user_byte`；
* `TraceRequest::Write (1)`：把 `data` 的低 8 位写入用户地址 `id` → 用 `write_user_byte`；
* `TraceRequest::Syscall (2)`：返回系统调用 `id` 的调用次数 → 查 `TCB.syscall_count[id]`。

权限检查带来如下行为，正好被 `ch4_trace1` 覆盖：

* 对**只读**映射（`prot = 1`）执行 `trace_read` 成功、`trace_write` 返回 `-1`；
* `munmap` 之后，两者都失败（PTE 已被清除）；
* 对非法地址 `isize::MAX` 与 `0x80200000`，两者都失败（前者超过 `MAX_USER_VA`，后者在用户页表中没有映射）。

## 五、syscall 计数与 sys_get_time

### 5.1 计数的自增时机

计数数组为 `syscall_count: [usize; MAX_SYSCALL_NUM]`（`MAX_SYSCALL_NUM = 500`），在 `syscall()` 的 **`match` 之前**统一自增：

```rust
pub fn syscall(syscall_id: usize, args: [usize; 3]) -> isize {
    // 先计数，使 sys_trace(Syscall) 能把本次 trace 调用自身也计入
    increase_current_syscall_count(syscall_id);
    match syscall_id { ... }
}
```

这一点非常关键，`ch3_trace` 的断言直接依赖它：

* 第一次 `count_syscall(SYSCALL_TRACE)` 期望 `== 2`：此前已经发生过 1 次 `count_syscall`，加上本次自身共 2 次；
* 第二次 `count_syscall(SYSCALL_TRACE)` 期望 `== 7`：累计 7 次 `sys_trace` 调用（含本次）。

`ch3_trace` 中 `count_syscall(SYSCALL_WRITE) == 2` 也被这一设计印证：`println!` 实际输出的字符串是 `"string from task trace test\n\n"`，用户态 `ConsoleBuffer` 遇到**两个**换行各 flush 一次，于是产生两次 `sys_write`。

### 5.2 get_time 的单位

`user_lib` 的 `get_time()` 为：

```rust
((time.sec & 0xffff) * 1000 + time.usec / 1000) as isize
```

即内核必须填写**微秒级**的 `usec`。因此 `sys_get_time` 改为使用 `get_time_us()`：

```rust
let us = get_time_us();
let time_val = TimeVal { sec: us / 1_000_000, usec: us % 1_000_000 };
```

写回用户空间的 `TimeVal` 使用 `translated_byte_buffer_checked`，越界时返回 `-1` 而不是 panic。

## 六、遇到的问题与解决

| 问题 | 现象 | 解决 |
| --- | --- | --- |
| `UPSafeCell` 没有 `get()` | 试图直接取 `TASK_MANAGER.inner` 编译失败 | 改用 `exclusive_access()` 并在借用期内完成操作 |
| `core::mem::forget(inner)` | 永久持有 `RefMut`，后续 `exclusive_access()` panic | 删除该 hack，让 `RefMut` 正常 drop |
| `mmap` 的 `len` 取整方向 | `ch4_mmap3` 与 `ch4_unmap` 期望相反 | 改为**向上取整**，两例同时满足 |
| `sys_trace` 越界 panic | 访问非法地址直接 panic | 新增 `read_user_byte` / `write_user_byte`，做权限检查并返回错误 |
| `sys_get_time` 单位错误 | `sec`/`usec` 换算用 tick | 改用 `get_time_us()` |
| 缺少 syscall 计数 | `ch3_trace` 计数断言不成立 | 在 `syscall()` 的 `match` 之前统一自增 |
| `xmas-elf` API 名 | `ph_flags.is_execute()` 不存在 | 改为 `is_executable()`（0.7.0 的实际 API） |
| `#![deny(warnings)]` | 新增公开项缺文档注释会报错 | 为所有新增 `pub` 项补齐 `///` 文档 |

## 七、测试与结果

本章涉及的全部测试程序均按官方 checker（`ci-user/check/ch4.py`）的要求通过：

* `ch4_mmap0`：可读写映射正常，`Test 04_1 OK!`；
* `ch4_mmap1`：只读映射写入触发缺页异常，任务被杀死，不输出错误提示；
* `ch4_mmap2`：只写映射（R=0 && W=1，RISC-V 保留组合）读取触发异常；
* `ch4_mmap3`：四种非法请求全部返回 `-1`，`Test 04_4 test OK!`；
* `ch4_unmap` / `ch4_unmap2`：`munmap` 语义正确，`Test 04_5 ummap OK!` / `Test 04_6 ummap2 OK!`；
* `ch4_trace1`：读写权限检查与 `munmap` 后的失效行为均正确，`Test trace_1 OK!`；
* `ch4b_sbrk`：堆按页增减正确，低于 `heap_bottom` 释放返回 `-1`，释放后写入触发缺页；
* 第三章的 `ch3_sleep`、`ch3_sleep1`、`ch3_trace` 仍全部通过。

## 八、总结

本章的核心收获是理解了「地址空间」在操作系统中的角色：

1. **地址空间 = 页表 + 一组虚拟内存区间**。把 `MemorySet` 放进 TCB，任务之间的内存就天然隔离；
2. **内核访问用户内存必须经过页表翻译**。这既是隔离性的实现基础，也要求所有系统调用都改用带权限检查的翻译接口；
3. **`satp` 的切换时机**决定了陷入/返回流程的复杂度，`trap.S` 中两处 `csrw satp` 与 `sfence.vma` 是不可省略的；
4. **系统调用的边界语义要精确**。`mmap` 的页取整方向、`munmap` 的对齐要求、`trace` 的权限检查，任何一处含糊都会导致用户程序行为异常——这也是官方测试用「相反期望」来卡住取整方向的原因。
