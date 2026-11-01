//! Mutex (spin-like and blocking(sleep))

use super::UPSafeCell;
use crate::task::TaskControlBlock;
use crate::task::{block_current_and_run_next, suspend_current_and_run_next};
use crate::task::{current_task, wakeup_task};
use alloc::{collections::VecDeque, sync::Arc};

/// Mutex trait
pub trait Mutex: Sync + Send {
    /// Lock the mutex
    fn lock(&self);
    /// Unlock the mutex
    fn unlock(&self);
    /// Return the task id of the current owner, if the mutex is locked.
    fn owner(&self) -> Option<usize>;
}

/// Spinlock Mutex struct
pub struct MutexSpin {
    inner: UPSafeCell<MutexSpinInner>,
}

/// Mutable state protected by a spin mutex.
pub struct MutexSpinInner {
    locked: bool,
    owner: Option<usize>,
}

impl MutexSpin {
    /// Create a new spinlock mutex
    pub fn new() -> Self {
        Self {
            inner: unsafe {
                UPSafeCell::new(MutexSpinInner {
                    locked: false,
                    owner: None,
                })
            },
        }
    }
}

impl Mutex for MutexSpin {
    /// Lock the spinlock mutex
    fn lock(&self) {
        trace!("kernel: MutexSpin::lock");
        loop {
            let mut inner = self.inner.exclusive_access();
            if inner.locked {
                drop(inner);
                suspend_current_and_run_next();
                continue;
            } else {
                inner.locked = true;
                inner.owner = Some(
                    current_task()
                        .unwrap()
                        .inner_exclusive_access()
                        .res
                        .as_ref()
                        .unwrap()
                        .tid,
                );
                return;
            }
        }
    }

    fn unlock(&self) {
        trace!("kernel: MutexSpin::unlock");
        let mut inner = self.inner.exclusive_access();
        inner.locked = false;
        inner.owner = None;
    }

    fn owner(&self) -> Option<usize> {
        self.inner.exclusive_access().owner
    }
}

/// Blocking Mutex struct
pub struct MutexBlocking {
    inner: UPSafeCell<MutexBlockingInner>,
}

/// Mutable state protected by a blocking mutex.
pub struct MutexBlockingInner {
    locked: bool,
    owner: Option<usize>,
    wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl MutexBlocking {
    /// Create a new blocking mutex
    pub fn new() -> Self {
        trace!("kernel: MutexBlocking::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(MutexBlockingInner {
                    locked: false,
                    owner: None,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }
}

impl Mutex for MutexBlocking {
    /// lock the blocking mutex
    fn lock(&self) {
        trace!("kernel: MutexBlocking::lock");
        let mut mutex_inner = self.inner.exclusive_access();
        if mutex_inner.locked {
            mutex_inner.wait_queue.push_back(current_task().unwrap());
            drop(mutex_inner);
            block_current_and_run_next();
        } else {
            mutex_inner.locked = true;
            mutex_inner.owner = Some(
                current_task()
                    .unwrap()
                    .inner_exclusive_access()
                    .res
                    .as_ref()
                    .unwrap()
                    .tid,
            );
        }
    }

    /// unlock the blocking mutex
    fn unlock(&self) {
        trace!("kernel: MutexBlocking::unlock");
        let mut mutex_inner = self.inner.exclusive_access();
        assert!(mutex_inner.locked);
        if let Some(waking_task) = mutex_inner.wait_queue.pop_front() {
            mutex_inner.owner = Some(
                waking_task
                    .inner_exclusive_access()
                    .res
                    .as_ref()
                    .unwrap()
                    .tid,
            );
            wakeup_task(waking_task);
        } else {
            mutex_inner.locked = false;
            mutex_inner.owner = None;
        }
    }

    fn owner(&self) -> Option<usize> {
        self.inner.exclusive_access().owner
    }
}
