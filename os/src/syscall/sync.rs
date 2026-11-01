use crate::sync::{Condvar, Mutex, MutexBlocking, MutexSpin, Semaphore};
use crate::task::{
    block_current_and_run_next, current_process, current_task, ProcessControlBlock,
    TaskControlBlock,
};
use crate::timer::{add_timer, get_time_ms};
use alloc::{sync::Arc, vec::Vec};

/// Return value used when a resource request would make the process deadlock.
const DEADLOCK_RETURN: isize = -0xDEAD;

/// Check whether waiting for `mutex` would close a wait-for cycle.
fn mutex_request_would_deadlock(
    process: &Arc<ProcessControlBlock>,
    requester: usize,
    mutex: &Arc<dyn Mutex>,
) -> bool {
    let mut owner = mutex.owner();
    let mut visited = Vec::new();
    while let Some(owner_tid) = owner {
        if owner_tid == requester || visited.iter().any(|tid| *tid == owner_tid) {
            return true;
        }
        visited.push(owner_tid);
        let owner_task = {
            let process_inner = process.inner_exclusive_access();
            process_inner
                .tasks
                .get(owner_tid)
                .and_then(|task| task.as_ref().map(Arc::clone))
        };
        let owner_task = match owner_task {
            Some(task) => task,
            None => return false,
        };
        let waiting_mutex = owner_task.inner_exclusive_access().waiting_mutex;
        let waiting_mutex = match waiting_mutex {
            Some(id) => id,
            None => return false,
        };
        let next_mutex = {
            let process_inner = process.inner_exclusive_access();
            process_inner
                .mutex_list
                .get(waiting_mutex)
                .and_then(|mutex| mutex.as_ref().map(Arc::clone))
        };
        owner = match next_mutex {
            Some(mutex) => mutex.owner(),
            None => return false,
        };
    }
    false
}

/// Check a semaphore request with the Banker's safety algorithm.
fn semaphore_request_is_safe(
    process: &Arc<ProcessControlBlock>,
    requester: usize,
    requested_semaphore: usize,
) -> bool {
    let (semaphores, tasks, allocations, needs) = {
        let process_inner = process.inner_exclusive_access();
        let semaphores: Vec<_> = process_inner.semaphore_list.clone();
        let tasks: Vec<Option<Arc<TaskControlBlock>>> = process_inner.tasks.clone();
        let resource_count = semaphores.len();
        let mut allocations = Vec::new();
        for tid in 0..tasks.len() {
            let mut allocation = process_inner
                .semaphore_allocations
                .get(tid)
                .cloned()
                .unwrap_or_default();
            allocation.resize(resource_count, 0);
            allocations.push(allocation);
        }
        let mut needs = Vec::new();
        for (tid, task) in tasks.iter().enumerate() {
            let need = if tid == requester {
                Some(requested_semaphore)
            } else {
                task.as_ref()
                    .and_then(|task| task.inner_exclusive_access().waiting_semaphore)
            };
            needs.push(need);
        }
        (semaphores, tasks, allocations, needs)
    };

    let resource_count = semaphores.len();
    let mut work = Vec::new();
    for semaphore in semaphores.iter() {
        let Some(semaphore) = semaphore else {
            work.push(0);
            continue;
        };
        let available = semaphore.inner.exclusive_access().count;
        work.push(if available > 0 { available as usize } else { 0 });
    }
    let mut finished = Vec::new();
    finished.resize(tasks.len(), false);
    loop {
        let mut progressed = false;
        for tid in 0..tasks.len() {
            if tasks[tid].is_none() || finished[tid] {
                continue;
            }
            let can_continue = match needs[tid] {
                Some(resource) if resource < resource_count => work[resource] > 0,
                Some(_) => false,
                None => true,
            };
            if can_continue {
                for resource in 0..resource_count {
                    work[resource] = work[resource].saturating_add(allocations[tid][resource]);
                }
                finished[tid] = true;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    tasks
        .iter()
        .enumerate()
        .all(|(tid, task)| task.is_none() || finished[tid])
}
/// sleep syscall
pub fn sys_sleep(ms: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_sleep",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let expire_ms = get_time_ms() + ms;
    let task = current_task().unwrap();
    add_timer(expire_ms, task);
    block_current_and_run_next();
    0
}
/// mutex create syscall
pub fn sys_mutex_create(blocking: bool) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mutex: Option<Arc<dyn Mutex>> = if !blocking {
        Some(Arc::new(MutexSpin::new()))
    } else {
        Some(Arc::new(MutexBlocking::new()))
    };
    let mut process_inner = process.inner_exclusive_access();
    if let Some(id) = process_inner
        .mutex_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.mutex_list[id] = mutex;
        id as isize
    } else {
        process_inner.mutex_list.push(mutex);
        process_inner.mutex_list.len() as isize - 1
    }
}
/// mutex lock syscall
pub fn sys_mutex_lock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_lock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = task.process.upgrade().unwrap();
    let tid = task.inner_exclusive_access().res.as_ref().unwrap().tid;
    let (mutex, deadlock_detect_enabled) = {
        let process_inner = process.inner_exclusive_access();
        let mutex = match process_inner
            .mutex_list
            .get(mutex_id)
            .and_then(|mutex| mutex.as_ref())
        {
            Some(mutex) => Arc::clone(mutex),
            None => return -1,
        };
        (mutex, process_inner.deadlock_detect_enabled)
    };
    if deadlock_detect_enabled && mutex_request_would_deadlock(&process, tid, &mutex) {
        return DEADLOCK_RETURN;
    }
    task.inner_exclusive_access().waiting_mutex = Some(mutex_id);
    mutex.lock();
    task.inner_exclusive_access().waiting_mutex = None;
    0
}
/// mutex unlock syscall
pub fn sys_mutex_unlock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_unlock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let mutex = match process_inner
        .mutex_list
        .get(mutex_id)
        .and_then(|mutex| mutex.as_ref())
    {
        Some(mutex) => Arc::clone(mutex),
        None => return -1,
    };
    drop(process_inner);
    drop(process);
    mutex.unlock();
    0
}
/// semaphore create syscall
pub fn sys_semaphore_create(res_count: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .semaphore_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.semaphore_list[id] = Some(Arc::new(Semaphore::new(res_count)));
        id
    } else {
        process_inner
            .semaphore_list
            .push(Some(Arc::new(Semaphore::new(res_count))));
        process_inner.semaphore_list.len() - 1
    };
    id as isize
}
/// semaphore up syscall
pub fn sys_semaphore_up(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_up",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = task.process.upgrade().unwrap();
    let tid = task.inner_exclusive_access().res.as_ref().unwrap().tid;
    let sem = {
        let process_inner = process.inner_exclusive_access();
        match process_inner
            .semaphore_list
            .get(sem_id)
            .and_then(|semaphore| semaphore.as_ref())
        {
            Some(semaphore) => Arc::clone(semaphore),
            None => return -1,
        }
    };
    let woken_task = sem.up();
    let mut process_inner = process.inner_exclusive_access();
    let resource_count = process_inner.semaphore_list.len();
    if let Some(allocation) = process_inner
        .semaphore_allocations
        .get_mut(tid)
        .and_then(|allocation| allocation.get_mut(sem_id))
    {
        *allocation = allocation.saturating_sub(1);
    }
    if let Some(woken_task) = woken_task {
        let waiter_tid = woken_task
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid;
        woken_task.inner_exclusive_access().waiting_semaphore = None;
        while process_inner.semaphore_allocations.len() <= waiter_tid {
            let mut allocation = Vec::new();
            allocation.resize(resource_count, 0);
            process_inner.semaphore_allocations.push(allocation);
        }
        let allocation = &mut process_inner.semaphore_allocations[waiter_tid];
        allocation.resize(resource_count, 0);
        allocation[sem_id] = allocation[sem_id].saturating_add(1);
    }
    0
}
/// semaphore down syscall
pub fn sys_semaphore_down(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_down",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = task.process.upgrade().unwrap();
    let tid = task.inner_exclusive_access().res.as_ref().unwrap().tid;
    let (sem, deadlock_detect_enabled) = {
        let process_inner = process.inner_exclusive_access();
        let sem = match process_inner
            .semaphore_list
            .get(sem_id)
            .and_then(|semaphore| semaphore.as_ref())
        {
            Some(semaphore) => Arc::clone(semaphore),
            None => return -1,
        };
        (sem, process_inner.deadlock_detect_enabled)
    };
    if deadlock_detect_enabled && !semaphore_request_is_safe(&process, tid, sem_id) {
        return DEADLOCK_RETURN;
    }
    task.inner_exclusive_access().waiting_semaphore = Some(sem_id);
    let blocked = sem.down();
    task.inner_exclusive_access().waiting_semaphore = None;
    let mut process_inner = process.inner_exclusive_access();
    let resource_count = process_inner.semaphore_list.len();
    while process_inner.semaphore_allocations.len() <= tid {
        let mut allocation = Vec::new();
        allocation.resize(resource_count, 0);
        process_inner.semaphore_allocations.push(allocation);
    }
    let allocation = &mut process_inner.semaphore_allocations[tid];
    allocation.resize(resource_count, 0);
    if !blocked {
        allocation[sem_id] = allocation[sem_id].saturating_add(1);
    }
    0
}
/// condvar create syscall
pub fn sys_condvar_create() -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .condvar_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.condvar_list[id] = Some(Arc::new(Condvar::new()));
        id
    } else {
        process_inner
            .condvar_list
            .push(Some(Arc::new(Condvar::new())));
        process_inner.condvar_list.len() - 1
    };
    id as isize
}
/// condvar signal syscall
pub fn sys_condvar_signal(condvar_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_signal",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    drop(process_inner);
    condvar.signal();
    0
}
/// condvar wait syscall
pub fn sys_condvar_wait(condvar_id: usize, mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_wait",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    condvar.wait(mutex);
    0
}
/// enable deadlock detection syscall
///
/// YOUR JOB: Implement deadlock detection, but might not all in this syscall
pub fn sys_enable_deadlock_detect(enabled: usize) -> isize {
    if enabled > 1 {
        return -1;
    }
    trace!("kernel: sys_enable_deadlock_detect {}", enabled);
    current_process()
        .inner_exclusive_access()
        .deadlock_detect_enabled = enabled == 1;
    0
}
