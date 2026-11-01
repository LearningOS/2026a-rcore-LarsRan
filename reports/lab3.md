# rCore 第五章实验报告（lab3）

## 一、功能总结

完成 `fork` 地址空间与 TCB 复制、`exec` 映像替换、`exit` 僵尸化、`waitpid`
阻塞回收与父进程唤醒；实现 `spawn`、跨页时间及 mmap 接口，并加入 stride
优先级调度。第五章本地测例全部通过。

## 二、stride 算法问答题

### 1. 8 位 stride 溢出后会轮到谁执行？

不会轮到理论上的较小者 `p1`。若两者步长均为 10，当前
`p1.stride = 255`、`p2.stride = 250`，则 `p2` 执行一个时间片后：

```text
p2.stride = (250 + 10) mod 256 = 4
```

若直接按无符号整数比较，会得到 `4 < 255`，因此调度器会再次选择 `p2`。原因是
8 位存储发生回绕，真实次序意义上的“比 255 大 5”被解释成了很小的数 4。

### 2. 为什么优先级不小于 2 时，STRIDE_MAX - STRIDE_MIN ≤ BigStride / 2？

每个进程的步长为：

```text
pass = BigStride / priority
```

因为 `priority >= 2`，所以每个进程单次增加的步长满足
`pass <= BigStride / 2`。

初始时所有进程的 `stride` 都为 0，最大值与最小值之差为 0。每次调度只修改当前
最小值所对应进程的 `stride`，且最多增加 `BigStride / 2`；其他进程的值保持不变。
因此更新后的最大值最多为“原最小值 + BigStride / 2”，而新的最小值不会小于原最小值。

所以任意合法更新后：

```text
STRIDE_MAX - STRIDE_MIN <= BigStride / 2
```

这一性质说明所有活动进程的累计值始终落在半个 `BigStride` 的窗口内，因此可以区分
正常的小范围先后关系与因整数溢出产生的回绕。

### 3. 补全考虑溢出后的 `partial_cmp`

比较时不能直接使用普通大小关系，而应计算两个 `u64` 的回绕差值。若
`self` 在模 `2^64` 意义下落后于 `other` 不超过半圈，则 `self` 更小；否则
认为 `self` 更大。题目已保证两个值不相等，因此相等分支不参与实际调度。

```rust
use core::cmp::Ordering;

struct Stride(u64);

impl PartialOrd for Stride {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.0 == other.0 {
            return None;
        }

        const HALF: u64 = 1u64 << 63;
        if self.0.wrapping_sub(other.0) <= HALF {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Greater)
        }
    }
}

impl PartialEq for Stride {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}
```

例如 `self = 4`、`other = 255` 时，回绕差值大于半圈，因此判定 `4 > 255`；
反过来 `255 < 4`，从而避免把回绕后的小数值错误当成新的最小值。

## 三、测试结果

执行 `make -C ci-user test CHAPTER=5` 后，检查器结果为 `15/15`。测试覆盖进程
创建、映像替换、僵尸回收、40 个并发子进程、退出码传递、mmap/munmap 和优先级
调度；`lab1`、`lab2`、`lab3` 均通过检查。

## 四、荣誉准则

我承诺在本次实验中独立完成代码编写与报告撰写，未抄袭他人代码，未将代码分享给
他人，也未让他人代为编写。我理解并遵守学术诚信原则。

## 五、实验评价

本次实验让我把地址空间、陷阱上下文、资源所有权和调度队列联系起来。进程退出与
回收最容易出错的地方，是既要保留退出码，又要在正确时机释放地址空间和唤醒父进程。
stride 调度则说明，整数溢出不仅是实现细节，也会直接改变算法语义。
