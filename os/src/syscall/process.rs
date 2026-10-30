//! Process management syscalls.

use crate::config::PAGE_SIZE;
use crate::fs::{open_file, OpenFlags};
use crate::mm::{translated_byte_buffer_checked, translated_str, write_user_i32, MapPermission};
use crate::task::{
    add_task, current_task, current_user_token, exit_current_and_run_next,
    suspend_current_and_run_next, wait_current_and_run_next,
};
use crate::timer::get_time_us;

#[repr(C)]
#[derive(Debug)]
/// Wall-clock time represented as seconds and microseconds.
pub struct TimeVal {
    /// Whole seconds elapsed.
    pub sec: usize,
    /// Microseconds within the current second.
    pub usec: usize,
}

/// Exit the current process and submit an exit code.
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// Yield the CPU to another ready process.
pub fn sys_yield() -> isize {
    suspend_current_and_run_next();
    0
}

/// Return the current process identifier.
pub fn sys_getpid() -> isize {
    current_task().unwrap().pid.0 as isize
}

/// Copy the current process and return the child pid to the parent and zero to
/// the child.
pub fn sys_fork() -> isize {
    let current = current_task().unwrap();
    let child = current.fork();
    let child_pid = child.getpid();
    child.inner_exclusive_access().get_trap_cx().x[10] = 0;
    add_task(child);
    child_pid as isize
}

/// Replace the current process image with the executable selected by a user
/// path.
pub fn sys_exec(path: *const u8) -> isize {
    let path = translated_str(current_user_token(), path);
    match open_file(path.as_str(), OpenFlags::RDONLY) {
        Some(inode) => {
            let elf_data = inode.read_all();
            current_task().unwrap().exec(elf_data.as_slice());
            0
        }
        None => -1,
    }
}

/// Wait for a child process and copy its exit code into user memory.
///
/// A negative `pid` waits for any child. The current process blocks until a
/// matching child becomes a zombie.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    loop {
        let task = current_task().unwrap();
        let mut inner = task.inner_exclusive_access();
        if !inner
            .children
            .iter()
            .any(|child| pid == -1 || pid as usize == child.getpid())
        {
            return -1;
        }

        let zombie_idx = inner.children.iter().position(|child| {
            child.inner_exclusive_access().is_zombie()
                && (pid == -1 || pid as usize == child.getpid())
        });
        if let Some(idx) = zombie_idx {
            let child = inner.children[idx].clone();
            let found_pid = child.getpid();
            let exit_code = child.inner_exclusive_access().exit_code;
            if !write_user_i32(inner.memory_set.token(), exit_code_ptr, exit_code) {
                return -1;
            }
            drop(child);
            let child = inner.children.remove(idx);
            debug_assert_eq!(alloc::sync::Arc::strong_count(&child), 1);
            return found_pid as isize;
        }

        drop(inner);
        wait_current_and_run_next(pid);
    }
}

/// Store the current wall-clock time in writable user memory.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let us = get_time_us();
    let time = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let mut buffers = match translated_byte_buffer_checked(
        current_user_token(),
        ts as *const u8,
        core::mem::size_of::<TimeVal>(),
        true,
    ) {
        Some(buffers) => buffers,
        None => return -1,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &time as *const TimeVal as *const u8,
            core::mem::size_of::<TimeVal>(),
        )
    };
    let mut offset = 0;
    for buffer in buffers.iter_mut() {
        let len = buffer.len().min(bytes.len() - offset);
        buffer[..len].copy_from_slice(&bytes[offset..offset + len]);
        offset += len;
    }
    0
}

/// Map a page-aligned user range with read, write and execute permission bits.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    if start % PAGE_SIZE != 0 || len == 0 || prot == 0 || prot & !0b111 != 0 {
        return -1;
    }
    let mut permission = MapPermission::U;
    if prot & 0b001 != 0 {
        permission |= MapPermission::R;
    }
    if prot & 0b010 != 0 {
        permission |= MapPermission::W;
    }
    if prot & 0b100 != 0 {
        permission |= MapPermission::X;
    }
    if current_task().unwrap().mmap(start, len, permission) {
        0
    } else {
        -1
    }
}

/// Unmap an exact page-aligned user range.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    if start % PAGE_SIZE != 0 || len == 0 || len % PAGE_SIZE != 0 {
        return -1;
    }
    if current_task().unwrap().munmap(start, len) {
        0
    } else {
        -1
    }
}

/// Change the current process's program break.
pub fn sys_sbrk(size: i32) -> isize {
    match current_task().unwrap().change_program_brk(size) {
        Some(old_brk) => old_brk as isize,
        None => -1,
    }
}

/// Create a child from an executable without returning into the copied image.
pub fn sys_spawn(path: *const u8) -> isize {
    let path = translated_str(current_user_token(), path);
    let inode = match open_file(path.as_str(), OpenFlags::RDONLY) {
        Some(inode) => inode,
        None => return -1,
    };
    let elf_data = inode.read_all();
    let parent = current_task().unwrap();
    let child = parent.spawn(elf_data.as_slice());
    let child_pid = child.getpid();
    add_task(child);
    child_pid as isize
}

/// Set the current process's scheduling priority.
pub fn sys_set_priority(priority: isize) -> isize {
    if priority < 2 {
        return -1;
    }
    if current_task().unwrap().set_priority(priority as usize) {
        priority
    } else {
        -1
    }
}
