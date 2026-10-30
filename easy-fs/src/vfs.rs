use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    /// The number of this inode, used as the `ino` field of `Stat`.
    inode_id: u32,
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        inode_id: u32,
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            inode_id,
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// The number of this inode
    pub fn inode_id(&self) -> u32 {
        self.inode_id
    }
    /// The number of hard links pointing to this inode
    pub fn nlink(&self) -> u32 {
        self.read_disk_inode(|disk_inode| disk_inode.nlink)
    }
    /// Whether this inode is a directory
    pub fn is_dir(&self) -> bool {
        self.read_disk_inode(|disk_inode| disk_inode.is_dir())
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        if !DirEntry::is_valid_name(name) {
            return None;
        }
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    inode_id,
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        if !DirEntry::is_valid_name(name) {
            return None;
        }
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            new_inode_id,
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    /// Create a hard link named `name` under the current directory, which
    /// points to the same inode as `target`.
    ///
    /// The link count of the target inode is increased by one. Returns `false`
    /// if a file named `name` already exists, or if `name` is invalid.
    pub fn link(&self, name: &str, target: &Arc<Inode>) -> bool {
        if !DirEntry::is_valid_name(name) || target.is_dir() {
            return false;
        }
        let mut fs = self.fs.lock();
        // the name must not be taken already
        let taken = self.read_disk_inode(|disk_inode| {
            assert!(disk_inode.is_dir());
            self.find_inode_id(name, disk_inode).is_some()
        });
        if taken {
            return false;
        }
        // append a new dirent which refers to the target inode
        self.modify_disk_inode(|dir_inode| {
            let file_count = (dir_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // make room for the new dirent
            self.increase_size(new_size as u32, dir_inode, &mut fs);
            let dirent = DirEntry::new(name, target.inode_id());
            dir_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });
        // increase the link count of the target inode
        target.modify_disk_inode(|target_inode| {
            target_inode.nlink = target_inode.nlink.saturating_add(1);
        });
        block_cache_sync_all();
        true
    }
    /// Remove the directory entry named `name` from the current directory.
    ///
    /// The link count of the target inode is decreased by one. If it drops to
    /// zero, the inode itself and all the data blocks it owns are reclaimed.
    ///
    /// Returns `false` if there is no such entry.
    pub fn unlink(&self, name: &str) -> bool {
        if !DirEntry::is_valid_name(name) {
            return false;
        }
        let mut fs = self.fs.lock();
        // locate the entry and the inode it refers to
        let target_id = self.read_disk_inode(|disk_inode| {
            assert!(disk_inode.is_dir());
            self.find_inode_id(name, disk_inode)
        });
        let target_id = match target_id {
            Some(id) => id,
            None => return false,
        };
        let (target_block_id, target_block_offset) = fs.get_disk_inode_pos(target_id);
        let target_is_dir =
            get_block_cache(target_block_id as usize, Arc::clone(&self.block_device))
                .lock()
                .read(target_block_offset, |target_inode: &DiskInode| {
                    target_inode.is_dir()
                });
        if target_is_dir {
            return false;
        }
        // remove the entry from the directory: shift all the following
        // entries one slot to the left and shrink the directory by one entry
        self.modify_disk_inode(|dir_inode| {
            let file_count = (dir_inode.size as usize) / DIRENT_SZ;
            let mut removed = None;
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    dir_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if dirent.name() == name {
                    removed = Some(i);
                    break;
                }
            }
            let removed = match removed {
                Some(i) => i,
                None => return,
            };
            // move the following entries backwards
            for i in removed..file_count - 1 {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    dir_inode.read_at(
                        DIRENT_SZ * (i + 1),
                        dirent.as_bytes_mut(),
                        &self.block_device,
                    ),
                    DIRENT_SZ,
                );
                dir_inode.write_at(DIRENT_SZ * i, dirent.as_bytes(), &self.block_device);
            }
            // shrink the directory by one entry
            dir_inode.size = ((file_count - 1) * DIRENT_SZ) as u32;
        });
        // decrease the link count of the target inode
        let nlink = get_block_cache(target_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(target_block_offset, |target_inode: &mut DiskInode| {
                target_inode.nlink = target_inode.nlink.saturating_sub(1);
                target_inode.nlink
            });
        if nlink == 0 {
            // the last link is gone: reclaim the inode and all its data
            let data_blocks =
                get_block_cache(target_block_id as usize, Arc::clone(&self.block_device))
                    .lock()
                    .modify(target_block_offset, |target_inode: &mut DiskInode| {
                        target_inode.clear_size(&self.block_device)
                    });
            for data_block in data_blocks.into_iter() {
                fs.dealloc_data(data_block);
            }
            fs.dealloc_inode(target_id);
        }
        block_cache_sync_all();
        true
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        size
    }
    /// Flush all cached filesystem blocks to the block device.
    pub fn sync(&self) {
        let _fs = self.fs.lock();
        block_cache_sync_all();
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
}
