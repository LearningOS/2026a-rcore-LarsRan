//!Implementation of [`TaskManager`]
use super::{TaskControlBlock, TaskStatus};
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::*;
///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
    blocked_queue: Vec<Arc<TaskControlBlock>>,
}

/// A weighted stride process scheduler.
impl TaskManager {
    /// Create an empty task manager.
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            blocked_queue: Vec::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }
    /// Take the ready task with the smallest scheduling pass.
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        let idx = self
            .ready_queue
            .iter()
            .enumerate()
            .min_by_key(|(_, task)| task.pass())
            .map(|(idx, _)| idx)?;
        self.ready_queue.remove(idx)
    }
    /// Store a task that is waiting and remove it from scheduling.
    pub fn block(&mut self, task: Arc<TaskControlBlock>) {
        self.blocked_queue.push(task);
    }
    /// Move tasks waiting for `child_pid` back to the ready queue.
    fn wake_waiters(&mut self, child_pid: usize) -> Vec<Arc<TaskControlBlock>> {
        let mut ready = Vec::new();
        let mut waiting = Vec::new();
        for task in self.blocked_queue.drain(..) {
            let should_wake = {
                let mut inner = task.inner_exclusive_access();
                let target = inner.waiting_for;
                let matches = target == Some(-1) || target == Some(child_pid as isize);
                if matches && inner.task_status == TaskStatus::Blocked {
                    inner.task_status = TaskStatus::Ready;
                    inner.waiting_for = None;
                    true
                } else {
                    false
                }
            };
            if should_wake {
                ready.push(task);
            } else {
                waiting.push(task);
            }
        }
        self.blocked_queue = waiting;
        ready
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    task.reserve_quantum();
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.exclusive_access().fetch()
}

/// Block a task until one of its children exits.
pub fn block_task(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.exclusive_access().block(task);
}

/// Wake every task whose wait target accepts `child_pid`.
pub fn wake_waiters_for(child_pid: usize) {
    let ready = TASK_MANAGER.exclusive_access().wake_waiters(child_pid);
    for task in ready {
        add_task(task);
    }
}
