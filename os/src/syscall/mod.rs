//! Implementation of syscalls
//!
//! The single entry point to all system calls, [`syscall()`], is called
//! whenever userspace executes an `ecall` instruction.

/// Write bytes to a file descriptor.
const SYSCALL_WRITE: usize = 64;
/// Exit the current task.
const SYSCALL_EXIT: usize = 93;
/// Yield the CPU to another ready task.
const SYSCALL_YIELD: usize = 124;
/// Get the current time.
const SYSCALL_GET_TIME: usize = 169;
/// Change the data segment size.
const SYSCALL_SBRK: usize = 214;
/// Unmap a user range.
const SYSCALL_MUNMAP: usize = 215;
/// Map a user range.
const SYSCALL_MMAP: usize = 222;
/// Inspect user memory or syscall counts.
const SYSCALL_TRACE: usize = 410;

mod fs;
mod process;

use fs::*;
use process::*;

use crate::task::increase_current_syscall_count;

/// Handle a system call exception with `syscall_id` and its arguments.
pub fn syscall(syscall_id: usize, args: [usize; 3]) -> isize {
    // Count before dispatch so a tracing call also counts itself.
    increase_current_syscall_count(syscall_id);

    match syscall_id {
        SYSCALL_WRITE => sys_write(args[0], args[1] as *const u8, args[2]),
        SYSCALL_EXIT => sys_exit(args[0] as i32),
        SYSCALL_YIELD => sys_yield(),
        SYSCALL_GET_TIME => sys_get_time(args[0] as *mut TimeVal, args[1]),
        SYSCALL_SBRK => sys_sbrk(args[0] as i32),
        SYSCALL_MUNMAP => sys_munmap(args[0], args[1]),
        SYSCALL_MMAP => sys_mmap(args[0], args[1], args[2]),
        SYSCALL_TRACE => sys_trace(args[0], args[1], args[2]),
        _ => panic!("Unsupported syscall_id: {}", syscall_id),
    }
}
