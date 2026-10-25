//! Process management syscalls

use crate::config::PAGE_SIZE;
use crate::mm::{read_user_byte, translated_byte_buffer_checked, write_user_byte, MapPermission};
use crate::task::{
    change_program_brk, current_task_mmap, current_task_munmap, current_user_token,
    exit_current_and_run_next, get_current_syscall_count, suspend_current_and_run_next,
};
use crate::timer::get_time_us;

#[repr(C)]
#[derive(Debug)]
/// Time represented as whole seconds and microseconds.
pub struct TimeVal {
    /// Seconds elapsed.
    pub sec: usize,
    /// Microseconds within the current second.
    pub usec: usize,
}

/// Task exits and submits an exit code.
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// Current task gives up the CPU for other tasks.
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// Get time with second and microsecond precision.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let token = current_user_token();
    let us = get_time_us();
    let time_val = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let mut buf = match translated_byte_buffer_checked(
        token,
        ts as *const u8,
        core::mem::size_of::<TimeVal>(),
        true,
    ) {
        Some(buf) => buf,
        None => return -1,
    };
    let mut offset = 0;
    let src = unsafe {
        core::slice::from_raw_parts(
            &time_val as *const TimeVal as *const u8,
            core::mem::size_of::<TimeVal>(),
        )
    };
    for slice in buf.iter_mut() {
        let len = slice.len().min(src.len() - offset);
        slice[..len].copy_from_slice(&src[offset..offset + len]);
        offset += len;
    }
    0
}

/// Trace a user address or report the current task's syscall count.
///
/// Request `0` reads one user byte, request `1` writes one user byte, and
/// request `2` returns the count for syscall `id`.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    let token = current_user_token();
    match trace_request {
        0 => match read_user_byte(token, id) {
            Some(byte) => byte as isize,
            None => -1,
        },
        1 => {
            if write_user_byte(token, id, data as u8) {
                0
            } else {
                -1
            }
        }
        2 => get_current_syscall_count(id) as isize,
        _ => -1,
    }
}

/// Map a page-aligned user range with Linux-style read/write/execute bits.
///
/// A zero permission mask, an invalid permission bit, a misaligned start, or
/// any overlap with an existing mapping is rejected with `-1`.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel: sys_mmap");
    if start % PAGE_SIZE != 0 || len == 0 || prot == 0 || prot & !0b111 != 0 {
        return -1;
    }
    let mut map_perm = MapPermission::U;
    if prot & 0b001 != 0 {
        map_perm |= MapPermission::R;
    }
    if prot & 0b010 != 0 {
        map_perm |= MapPermission::W;
    }
    if prot & 0b100 != 0 {
        map_perm |= MapPermission::X;
    }
    if current_task_mmap(start, len, map_perm) {
        0
    } else {
        -1
    }
}

/// Unmap an existing mapping that exactly matches the requested page range.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    if start % PAGE_SIZE != 0 || len == 0 || len % PAGE_SIZE != 0 {
        return -1;
    }
    if current_task_munmap(start, len) {
        0
    } else {
        -1
    }
}

/// Change the current task's data segment size.
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
