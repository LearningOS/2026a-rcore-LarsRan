//! Task management implementation
//!
//! Everything about task management, like starting and switching tasks, is
//! implemented here. A single global [`TaskManager`] controls all tasks.

mod context;
mod switch;
#[allow(clippy::module_inception)]
mod task;

use crate::loader::{get_app_data, get_num_app};
use crate::mm::MapPermission;
use crate::sync::UPSafeCell;
use crate::trap::TrapContext;
use alloc::vec::Vec;
pub use context::TaskContext;
use lazy_static::*;
use switch::__switch;
pub use task::{TaskControlBlock, TaskStatus};

/// The task manager, where all the tasks are managed.
pub struct TaskManager {
    /// total number of tasks
    num_app: usize,
    /// use inner value to get mutable access
    inner: UPSafeCell<TaskManagerInner>,
}

/// The task manager inner in 'UPSafeCell'
struct TaskManagerInner {
    /// task list
    tasks: Vec<TaskControlBlock>,
    /// id of current `Running` task
    current_task: usize,
}

lazy_static! {
    /// a `TaskManager` global instance through lazy_static!
    pub static ref TASK_MANAGER: TaskManager = {
        println!("init TASK_MANAGER");
        let num_app = get_num_app();
        println!("num_app = {}", num_app);
        let mut tasks: Vec<TaskControlBlock> = Vec::new();
        for i in 0..num_app {
            tasks.push(TaskControlBlock::new(get_app_data(i), i));
        }
        TaskManager {
            num_app,
            inner: unsafe {
                UPSafeCell::new(TaskManagerInner {
                    tasks,
                    current_task: 0,
                })
            },
        }
    };
}

impl TaskManager {
    /// Run the first task in the task list.
    fn run_first_task(&self) -> ! {
        let mut inner = self.inner.exclusive_access();
        let next_task = &mut inner.tasks[0];
        next_task.task_status = TaskStatus::Running;
        let next_task_cx_ptr = &next_task.task_cx as *const TaskContext;
        drop(inner);
        let mut _unused = TaskContext::zero_init();
        unsafe {
            __switch(&mut _unused as *mut _, next_task_cx_ptr);
        }
        panic!("unreachable in run_first_task!");
    }

    /// Change the current running task's status to ready.
    fn mark_current_suspended(&self) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].task_status = TaskStatus::Ready;
    }

    /// Change the current running task's status to exited.
    fn mark_current_exited(&self) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].task_status = TaskStatus::Exited;
    }

    /// Find the next ready task.
    fn find_next_task(&self) -> Option<usize> {
        let inner = self.inner.exclusive_access();
        let current = inner.current_task;
        (current + 1..current + self.num_app + 1)
            .map(|id| id % self.num_app)
            .find(|id| inner.tasks[*id].task_status == TaskStatus::Ready)
    }

    /// Get the current running task's page-table token.
    fn get_current_token(&self) -> usize {
        let inner = self.inner.exclusive_access();
        inner.tasks[inner.current_task].get_user_token()
    }

    /// Get the current running task's trap context.
    fn get_current_trap_cx(&self) -> &'static mut TrapContext {
        let inner = self.inner.exclusive_access();
        inner.tasks[inner.current_task].get_trap_cx()
    }

    /// Change the current 'Running' task's program break.
    pub fn change_current_program_brk(&self, size: i32) -> Option<usize> {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].change_program_brk(size)
    }

    /// Map a range in the current task's address space.
    fn mmap_current_task(&self, start: usize, len: usize, permission: MapPermission) -> bool {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur]
            .memory_set
            .mmap_area(start, len, permission)
    }

    /// Unmap an exact range from the current task's address space.
    fn munmap_current_task(&self, start: usize, len: usize) -> bool {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].memory_set.munmap_area(start, len)
    }

    /// Get the current task's count for `syscall_id`.
    fn get_current_syscall_count(&self, syscall_id: usize) -> usize {
        let inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].get_syscall_count(syscall_id)
    }

    /// Record an invocation of `syscall_id` by the current task.
    fn increase_current_syscall_count(&self, syscall_id: usize) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].record_syscall(syscall_id);
    }

    /// Switch from the current running task to the next ready task.
    fn run_next_task(&self) {
        if let Some(next) = self.find_next_task() {
            let mut inner = self.inner.exclusive_access();
            let current = inner.current_task;
            inner.tasks[next].task_status = TaskStatus::Running;
            inner.current_task = next;
            let current_task_cx_ptr = &mut inner.tasks[current].task_cx as *mut TaskContext;
            let next_task_cx_ptr = &inner.tasks[next].task_cx as *const TaskContext;
            drop(inner);
            unsafe {
                __switch(current_task_cx_ptr, next_task_cx_ptr);
            }
        } else {
            panic!("All applications completed!");
        }
    }
}

/// Run the first task in task list.
pub fn run_first_task() {
    TASK_MANAGER.run_first_task();
}

/// Switch the current running task to the next ready task.
fn run_next_task() {
    TASK_MANAGER.run_next_task();
}

/// Mark the current running task ready.
fn mark_current_suspended() {
    TASK_MANAGER.mark_current_suspended();
}

/// Mark the current running task exited.
fn mark_current_exited() {
    TASK_MANAGER.mark_current_exited();
}

/// Suspend the current 'Running' task and run the next task in task list.
pub fn suspend_current_and_run_next() {
    mark_current_suspended();
    run_next_task();
}

/// Exit the current 'Running' task and run the next task in task list.
pub fn exit_current_and_run_next() {
    mark_current_exited();
    run_next_task();
}

/// Get the current 'Running' task's token.
pub fn current_user_token() -> usize {
    TASK_MANAGER.get_current_token()
}

/// Get the current 'Running' task's trap contexts.
pub fn current_trap_cx() -> &'static mut TrapContext {
    TASK_MANAGER.get_current_trap_cx()
}

/// Change the current 'Running' task's program break.
pub fn change_program_brk(size: i32) -> Option<usize> {
    TASK_MANAGER.change_current_program_brk(size)
}

/// Map a range in the current task's address space.
pub fn current_task_mmap(start: usize, len: usize, permission: MapPermission) -> bool {
    TASK_MANAGER.mmap_current_task(start, len, permission)
}

/// Unmap an exact range from the current task's address space.
pub fn current_task_munmap(start: usize, len: usize) -> bool {
    TASK_MANAGER.munmap_current_task(start, len)
}

/// Get the syscall count of the current task.
pub fn get_current_syscall_count(syscall_id: usize) -> usize {
    TASK_MANAGER.get_current_syscall_count(syscall_id)
}

/// Increase the syscall count of the current task.
pub fn increase_current_syscall_count(syscall_id: usize) {
    TASK_MANAGER.increase_current_syscall_count(syscall_id);
}
