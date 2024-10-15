// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{collections::BTreeMap, path::PathBuf, str::FromStr as _, sync::Arc, time};

use fuser as fuse;
use libc::EBADF;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use parking_lot::Mutex;
use tracing::{debug, warn};

use super::DeviceState;

const ENOENT: i32 = rustix::io::Errno::NOENT.raw_os_error();

const STATIC_DIRS: &[&str] = &[
    "sys",
    "sys/devices",
    "sys/devices/virtual",
    "sys/devices/virtual/input",
    "sys/class",
    "sys/class/input",
    "run",
    "run/udev",
    "run/udev/data",
];

// Must match the indexes above. Verified with a test.
const SYS_DEVICES_VIRTUAL_INPUT: u64 = static_ino(3);
const SYS_CLASS_INPUT: u64 = static_ino(5);
const UDEV_DATA: u64 = static_ino(8);

const UDEV_INPUT_DATA: &str = r#"E:ID_INPUT=1
E:ID_INPUT_JOYSTICK=1
E:ID_BUS=usb
G:seat
G:uaccess
Q:seat
Q:uaccess
V:1
"#;

const STATIC_TTL: time::Duration = time::Duration::MAX;
const SHORT_TTL: time::Duration = time::Duration::ZERO;

const fn static_ino(idx: usize) -> u64 {
    idx as u64 + fuse::FUSE_ROOT_ID + 1
}

fn static_path(ino: u64) -> Option<&'static str> {
    let idx = ino - fuse::FUSE_ROOT_ID - 1;
    STATIC_DIRS.get(idx as usize).copied()
}

#[derive(Clone)]
struct StaticEntry {
    parent_ino: u64,
    child_inos: Vec<u64>,
    relpath: PathBuf,
    attr: fuse::FileAttr,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
enum InodeType {
    /// The directory in /sys/devices/virtual/input.
    InputDir = 0,
    /// The symlink in /sys/class/input.
    InputSymlink = 1,
    /// A file containing device info, at
    /// /sys/devices/virtual/input/{dev}/uevent.
    Uevent = 2,
    /// A symlink back to /sys/class/input.
    SubsystemSymlink = 3,
    /// The data file in /run/udev/data.
    DataFile = 4,
}

/// We use the following encoding for inodes, to make lookup easy:
///  - The first 32 bytes are the short ID of the device.
///  - The last 32 bytes are the inode type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceInode {
    short_id: u32,
    inode_type: InodeType,
}

impl DeviceInode {
    fn make(short_id: u32, inode_type: InodeType) -> u64 {
        Self {
            short_id,
            inode_type,
        }
        .into()
    }
}

impl From<DeviceInode> for u64 {
    fn from(value: DeviceInode) -> Self {
        let res = (value.short_id as u64) << 32;
        res | u32::from(value.inode_type) as u64
    }
}

impl TryFrom<u64> for DeviceInode {
    type Error = <InodeType as TryFromPrimitive>::Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let short_id = (value >> 32) as u32;
        let ty = value as u32;
        ty.try_into().map(|inode_type| DeviceInode {
            short_id,
            inode_type,
        })
    }
}

/// A FUSE filesystem designed to fool libudev. All incoming paths are intended
/// to be absolute. The following paths are emulated:
///   - /sys/devices/virtual/input: contains folders for each virtual input
///     device.
///   - /sys/class/input: contains symlinks to the above device entries.
///   - /run/udev/data: contains "c{major}:{minor}" files with metadata on each
///     device.
pub struct UdevFs {
    state: Arc<Mutex<super::InputManagerState>>,
    static_inodes: BTreeMap<u64, StaticEntry>, // Indexed by inode.
}

impl UdevFs {
    pub fn new(state: Arc<Mutex<super::InputManagerState>>) -> Self {
        let ctime = time::SystemTime::now();

        let mut static_inodes: BTreeMap<u64, StaticEntry> = BTreeMap::new();

        for (idx, entry) in STATIC_DIRS.iter().enumerate() {
            let mut relpath = PathBuf::from_str(entry).unwrap();
            let ino = static_ino(idx);

            // Attempt to find the parent.
            let mut parent_ino = fuse::FUSE_ROOT_ID;
            for (prev_idx, prev_p) in STATIC_DIRS[..idx].iter().enumerate().rev() {
                if let Ok(p) = relpath.strip_prefix(prev_p) {
                    parent_ino = static_ino(prev_idx);
                    relpath = p.to_owned();
                    break;
                }
            }

            if let Some(parent) = static_inodes.get_mut(&parent_ino) {
                parent.child_inos.push(ino);
            }

            assert_eq!(
                relpath.components().count(),
                1,
                "failed to find parent for {}",
                relpath.display()
            );
            static_inodes.insert(
                ino,
                StaticEntry {
                    parent_ino,
                    child_inos: Vec::new(),
                    relpath,
                    attr: make_dir_attr(ino, ctime),
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
        let Some(name) = name.to_str() else {
            reply.error(ENOENT);
            return;
        };

        if parent == SYS_DEVICES_VIRTUAL_INPUT {
            let guard = self.state.lock();
            for dev in &guard.devices {
                if name == dev.devname {
                    let ino = DeviceInode::make(dev.short_id, InodeType::InputDir);
                    reply.entry(&SHORT_TTL, &make_dir_attr(ino, dev.plugged), 0);
                    return;
                }
            }
        }

        if parent == SYS_CLASS_INPUT {
            let guard = self.state.lock();
            for dev in &guard.devices {
                if name == dev.devname {
                    let ino = DeviceInode::make(dev.short_id, InodeType::InputSymlink);
                    reply.entry(&SHORT_TTL, &make_symlink_attr(ino, dev.plugged), 0);
                    return;
                }
            }
        }

        if parent == UDEV_DATA {
            let guard = self.state.lock();
            for dev in &guard.devices {
                if name == format!("c13:{}", dev.counter) {
                    let ino = DeviceInode::make(dev.short_id, InodeType::DataFile);
                    reply.entry(
                        &SHORT_TTL,
                        &make_file_attr(ino, dev.plugged, UDEV_INPUT_DATA.len()),
                        0,
                    );
                    return;
                }
            }
        }

        for entry in self.static_inodes.values() {
            if entry.parent_ino == parent && name == entry.relpath.as_os_str() {
                reply.entry(&STATIC_TTL, &entry.attr, 0);
                return;
            }
        }

        let Ok(DeviceInode {
            short_id,
            inode_type,
        }) = parent.try_into()
        else {
            reply.error(ENOENT);
            return;
        };

        let guard = self.state.lock();
        let Some(dev) = guard.find_device(short_id) else {
            reply.error(ENOENT);
            return;
        };

        match (inode_type, name) {
            (InodeType::InputDir, "uevent") => {
                let content = make_uevent(dev);
                let ino = DeviceInode::make(short_id, InodeType::Uevent);
                reply.entry(
                    &SHORT_TTL,
                    &make_file_attr(ino, dev.plugged, content.len()),
                    0,
                );

                return;
            }
            (InodeType::InputDir, "subsystem") => {
                let ino = DeviceInode::make(short_id, InodeType::SubsystemSymlink);
                reply.entry(&SHORT_TTL, &make_symlink_attr(ino, dev.plugged), 0);
                return;
            }
            (InodeType::InputDir, "dev") => (), // TODO
            _ => (),
        }

        warn!(parent = static_path(parent), name, "udevfs lookup failed");
        reply.error(ENOENT);
    }

    fn getattr(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        _fh: Option<u64>,
        reply: fuser::ReplyAttr,
    ) {
        if let Some(entry) = self.static_inodes.get(&ino) {
            reply.attr(&STATIC_TTL, &entry.attr);
            return;
        }

        let Ok(DeviceInode {
            short_id,
            inode_type,
        }) = ino.try_into()
        else {
            reply.error(ENOENT);
            return;
        };

        let guard = self.state.lock();
        let Some(dev) = guard.find_device(short_id) else {
            reply.error(ENOENT);
            return;
        };

        let attr = match inode_type {
            InodeType::InputDir => make_dir_attr(ino, dev.plugged),
            InodeType::InputSymlink => make_symlink_attr(ino, dev.plugged),
            InodeType::SubsystemSymlink => make_symlink_attr(ino, dev.plugged),
            InodeType::Uevent => {
                let contents = make_uevent(dev);
                make_file_attr(ino, dev.plugged, contents.len())
            }
            InodeType::DataFile => make_file_attr(ino, dev.plugged, UDEV_INPUT_DATA.len()),
        };

        reply.attr(&SHORT_TTL, &attr);
    }

    fn readlink(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyData) {
        let Ok(DeviceInode {
            short_id,
            inode_type,
        }) = ino.try_into()
        else {
            reply.error(ENOENT);
            return;
        };

        let guard = self.state.lock();
        let Some(dev) = guard.find_device(short_id) else {
            reply.error(ENOENT);
            return;
        };

        match inode_type {
            InodeType::InputSymlink => {
                let dst = format!("/sys/devices/virtual/input/{}", dev.devname);
                reply.data(dst.as_bytes());
            }
            InodeType::SubsystemSymlink => {
                reply.data(b"/sys/class/input");
            }
            _ => reply.error(ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        _fh: u64,
        _offset: i64,
        _size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        if self.static_inodes.contains_key(&ino) {
            return reply.error(EBADF);
        }

        let Ok(DeviceInode {
            short_id,
            inode_type,
        }) = ino.try_into()
        else {
            reply.error(ENOENT);
            return;
        };

        let guard = self.state.lock();
        let Some(dev) = guard.find_device(short_id) else {
            reply.error(ENOENT);
            return;
        };

        match inode_type {
            InodeType::Uevent => {
                let contents = make_uevent(dev);
                reply.data(contents.as_bytes())
            }
            InodeType::DataFile => reply.data(UDEV_INPUT_DATA.as_bytes()),
            _ => reply.error(ENOENT),
        };
    }

    fn readdir(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        if ino == SYS_DEVICES_VIRTUAL_INPUT {
            todo!() // It doesn't seem like udev ever does this.
        }

        if ino == SYS_CLASS_INPUT {
            let guard = self.state.lock();

            for (
                idx,
                DeviceState {
                    short_id, devname, ..
                },
            ) in guard.devices.iter().skip(offset as usize).enumerate()
            {
                debug!(devname, "udev is enumerating device");

                if reply.add(
                    DeviceInode::make(*short_id, InodeType::InputSymlink),
                    (idx as i64) + 1,
                    fuse::FileType::Symlink,
                    devname,
                ) {
                    break;
                }
            }

            reply.ok();
            return;
        }

        if let Some(entry) = self.static_inodes.get(&ino).cloned() {
            for child_ino in entry.child_inos {
                let child_entry = self.static_inodes.get(&child_ino).unwrap();
                if reply.add(
                    child_ino,
                    0,
                    fuse::FileType::Directory,
                    child_entry.relpath.file_name().unwrap(),
                ) {
                    break;
                };
            }

            reply.ok();
            return;
        }

        reply.error(ENOENT);
    }

    fn access(
        &mut self,
        _req: &fuser::Request<'_>,
        _ino: u64,
        _mask: i32,
        reply: fuser::ReplyEmpty,
    ) {
        reply.ok()
    }

    fn release(
        &mut self,
        _req: &fuser::Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        reply.ok()
    }
}

fn make_uevent(dev: &DeviceState) -> String {
    format!(
        "MAJOR=13\nMINOR={}\nDEVNAME=input/{}\n",
        dev.counter, dev.devname
    )
}

const fn make_file_attr(ino: u64, t: time::SystemTime, len: usize) -> fuse::FileAttr {
    fuser::FileAttr {
        ino,
        size: len as u64,
        blocks: 0,
        atime: t,
        mtime: t,
        ctime: t,
        crtime: time::SystemTime::UNIX_EPOCH,
        kind: fuse::FileType::RegularFile,
        perm: 0o644,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

const fn make_dir_attr(ino: u64, t: time::SystemTime) -> fuse::FileAttr {
    fuser::FileAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: t,
        mtime: t,
        ctime: t,
        crtime: time::SystemTime::UNIX_EPOCH,
        kind: fuse::FileType::Directory,
        perm: 0o777,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}
fn make_symlink_attr(ino: u64, t: time::SystemTime) -> fuse::FileAttr {
    fuser::FileAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: t,
        mtime: t,
        ctime: t,
        crtime: time::SystemTime::UNIX_EPOCH,
        kind: fuse::FileType::Symlink,
        perm: 0o777,
        nlink: 1,
        uid: 0,
        gid: 0,
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
        assert_eq!(static_path(UDEV_DATA), Some("run/udev/data"));
    }

    #[test]
    fn inode_serde() {
        let inode = DeviceInode {
            short_id: 12345,
            inode_type: InodeType::InputSymlink,
        };

        let v: u64 = inode.into();
        assert_eq!(v, 53021371269121);

        assert_eq!(DeviceInode::try_from(v), Ok(inode));
    }
}
