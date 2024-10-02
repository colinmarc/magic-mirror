// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    collections::HashMap,
    path::PathBuf,
    str::FromStr as _,
    sync::{Arc, Mutex},
    time,
};

use fuser as fuse;

const ENOENT: i32 = rustix::io::Errno::NOENT.raw_os_error();

// const INO_SYS: u64 = 2;
// const INO_SYS_DEVICES: u64 = 3;
// const INO_SYS_DEVICES_VIRTUAL: u64 = 4;
// const INO_SYS_DEVICES_VIRTUAL_INPUT: u64 = 5;
// const INO_SYS_CLASS: u64 = 6;
// const INO_SYS_CLASS_INPUT: u64 = 7;

const STATIC_DIRS: &[&str] = &[
    "sys",
    "sys/devices",
    "sys/devices/virtual",
    "sys/devices/virtual/input",
    "sys/class",
    "sys/class/input",
];

// Must match the indexes above. Verified with a test.
const SYS_DEVICES_VIRTUAL_INPUT: u64 = static_ino(3);
const SYS_CLASS_INPUT: u64 = static_ino(5);

const STATIC_TTL: time::Duration = time::Duration::MAX;

const fn static_ino(idx: usize) -> u64 {
    idx as u64 + fuse::FUSE_ROOT_ID + 1
}

fn static_path(ino: u64) -> Option<&'static str> {
    let idx = ino - fuse::FUSE_ROOT_ID - 1;
    STATIC_DIRS.get(idx as usize).copied()
}

struct StaticEntry {
    ino: u64,
    parent_ino: u64,
    relpath: PathBuf,
    attr: fuse::FileAttr,
}

/// A FUSE filesystem designed to fool libudev. All incoming paths are intended
/// to be absolute. The following paths are emulated:
///   - /sys/devices/virtual/input: contains folders for each virtual input
///     device.
///   - /sys/class/input: contains symlinks to the above device entries.
///   - /run/udev/control: an empty file
///   - /run/udev/data: contains "c{major}:minor" files with metadata on each
///     device.
pub struct UdevFs {
    state: Arc<Mutex<super::InputManagerState>>,
    // (parent, child) -> (relpath, attr)
    static_inodes: HashMap<u64, StaticEntry>,
}

impl UdevFs {
    pub fn new(state: Arc<Mutex<super::InputManagerState>>) -> Self {
        let ctime = time::SystemTime::now();
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();

        let mut static_inodes = HashMap::new();

        for (idx, entry) in STATIC_DIRS.iter().enumerate() {
            let mut relpath = PathBuf::from_str(entry).unwrap();
            let ino = static_ino(idx);

            let mut parent_ino = fuse::FUSE_ROOT_ID;
            for (prev_idx, prev_p) in STATIC_DIRS[..idx].iter().enumerate().rev() {
                if let Ok(p) = relpath.strip_prefix(prev_p) {
                    parent_ino = static_ino(prev_idx);
                    relpath = p.to_owned();
                    break;
                }
            }

            assert_eq!(relpath.components().count(), 1);
            static_inodes.insert(
                ino,
                StaticEntry {
                    ino,
                    parent_ino,
                    relpath,
                    attr: make_dir_attr(ino, ctime, uid, gid),
                },
            );
        }

        Self {
            state,
            static_inodes,
        }
    }
}

impl fuse::Filesystem for UdevFs {
    fn lookup(
        &mut self,
        _req: &fuser::Request<'_>,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        for entry in self.static_inodes.values() {
            if entry.parent_ino == parent && name == entry.relpath.as_os_str() {
                reply.entry(&STATIC_TTL, &entry.attr, 0);
                return;
            }
        }

        reply.error(ENOENT);
    }

    fn readdir(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        reply: fuser::ReplyDirectory,
    ) {
    }

    fn getattr(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        if let Some(entry) = self.static_inodes.get(&ino) {
            reply.attr(&STATIC_TTL, &entry.attr);
            return;
        }

        reply.error(ENOENT);
    }
}

const fn make_dir_attr(ino: u64, t: time::SystemTime, uid: u32, gid: u32) -> fuse::FileAttr {
    fuser::FileAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: t,
        mtime: t,
        ctime: t,
        crtime: time::SystemTime::UNIX_EPOCH,
        kind: fuse::FileType::Directory,
        perm: 0o555,
        nlink: 1,
        uid,
        gid,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn static_inodes_correct() {
        assert_eq!(
            static_path(SYS_DEVICES_VIRTUAL_INPUT),
            Some("sys/devices/virtual/input"),
        );
        assert_eq!(static_path(SYS_CLASS_INPUT), Some("sys/class/input"));
    }
}
