//! File and filesystem-related syscalls
use crate::fs::{link, open_file, unlink, OpenFlags, Stat};
use crate::mm::{translated_byte_buffer_checked, translated_str, UserBuffer};
use crate::task::{current_task, current_user_token};

/// Write bytes from readable user memory to the file referred to by `fd`.
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_write", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        if !file.writable() {
            return -1;
        }
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        let buffers = match translated_byte_buffer_checked(token, buf, len, false) {
            Some(buffers) => buffers,
            None => return -1,
        };
        file.write(UserBuffer::new(buffers)) as isize
    } else {
        -1
    }
}

/// Read bytes from the file referred to by `fd` into writable user memory.
pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        if !file.readable() {
            return -1;
        }
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        trace!("kernel: sys_read .. file.read");
        let buffers = match translated_byte_buffer_checked(token, buf, len, true) {
            Some(buffers) => buffers,
            None => return -1,
        };
        file.read(UserBuffer::new(buffers)) as isize
    } else {
        -1
    }
}

/// Open or create the file selected by a user path and return a file
/// descriptor.
pub fn sys_open(path: *const u8, flags: u32) -> isize {
    trace!("kernel:pid[{}] sys_open", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let path = translated_str(token, path);
    let flags = match OpenFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return -1,
    };
    if let Some(inode) = open_file(path.as_str(), flags) {
        let mut inner = task.inner_exclusive_access();
        let fd = inner.alloc_fd();
        inner.fd_table[fd] = Some(inode);
        fd as isize
    } else {
        -1
    }
}

/// Close the file descriptor `fd`.
pub fn sys_close(fd: usize) -> isize {
    trace!("kernel:pid[{}] sys_close", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    let file = inner.fd_table[fd].take();
    drop(inner);
    if let Some(file) = file {
        file.sync();
    }
    0
}

/// Get the status of the file referred to by `fd`.
///
/// The status is written into the user provided `Stat` structure, which may
/// cross a page boundary, so it is copied page by page.
pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    trace!("kernel:pid[{}] sys_fstat", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    let file = match &inner.fd_table[fd] {
        Some(file) => file.clone(),
        None => return -1,
    };
    // release current task TCB manually to avoid multi-borrow
    drop(inner);
    let stat = file.stat();
    // the `Stat` structure lives in the user address space and may cross a
    // page boundary, so it has to be translated page by page
    let mut buf = match translated_byte_buffer_checked(
        token,
        st as *const u8,
        core::mem::size_of::<Stat>(),
        true,
    ) {
        Some(buf) => buf,
        None => return -1,
    };
    let src = unsafe {
        core::slice::from_raw_parts(
            &stat as *const Stat as *const u8,
            core::mem::size_of::<Stat>(),
        )
    };
    let mut offset = 0;
    for slice in buf.iter_mut() {
        let len = slice.len().min(src.len() - offset);
        slice[..len].copy_from_slice(&src[offset..offset + len]);
        offset += len;
    }
    0
}

/// Create a hard link named `new_name` which points to the same inode as
/// `old_name`.
///
/// Only the paths are used: the directory file descriptors are ignored
/// because easy-fs currently has a single root directory, and the flags
/// argument of `linkat` is always zero for the supported use cases.
///
/// Returns 0 on success, -1 if the arguments are invalid, if `old_name` does
/// not exist, if `new_name` is already taken, or if both names are equal.
pub fn sys_linkat(
    _old_dirfd: usize,
    old_name: *const u8,
    _new_dirfd: usize,
    new_name: *const u8,
) -> isize {
    trace!("kernel:pid[{}] sys_linkat", current_task().unwrap().pid.0);
    let token = current_user_token();
    let old_path = translated_str(token, old_name);
    let new_path = translated_str(token, new_name);
    link(old_path.as_str(), new_path.as_str())
}

/// Remove the directory entry named `name` from the root directory.
///
/// The link count of the inode is decreased by one; if it drops to zero the
/// inode and all its data blocks are reclaimed.
///
/// Returns 0 on success, -1 if there is no such entry.
pub fn sys_unlinkat(_dirfd: usize, name: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_unlinkat", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, name);
    unlink(path.as_str())
}
