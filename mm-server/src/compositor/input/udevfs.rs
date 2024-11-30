// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
    time,
};

use fuser as fuse;
use libc::EBADF;
use parking_lot::Mutex;
use tracing::{debug, warn};

use super::DeviceState;

const ENOENT: i32 = rustix::io::Errno::NOENT.raw_os_error();

const UDEV_INPUT_DATA: &[u8] = r#"E:ID_INPUT=1
E:ID_INPUT_JOYSTICK=1
E:ID_BUS=usb
G:seat
G:uaccess
Q:seat
Q:uaccess
V:1
"#
.as_bytes();

const ZERO_TTL: time::Duration = time::Duration::ZERO;

#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    attr: fuse::FileAttr,
    /// The associated device ID.
    dev: Option<u64>,
}

struct InodeCache {
    inodes: BTreeMap<u64, Entry>,
    next_inode: u64,
    ctime: time::SystemTime,
}

impl InodeCache {
    fn get_or_insert(
        &mut self,
        p: impl AsRef<Path>,
        mut attr: fuse::FileAttr,
        dev: Option<u64>,
    ) -> fuse::FileAttr {
        for entry in self.inodes.values() {
            if entry.path == p.as_ref() {
                return entry.attr;
            }
        }

        let ino = self.next_inode;
        self.next_inode += 1;

        attr.ino = ino;
        self.inodes.insert(
            ino,
            Entry {
                path: p.as_ref().to_owned(),
                attr,
                dev,
            },
        );

        attr
    }

    fn lookup_name(&self, inode: u64) -> Option<(PathBuf, Option<u64>)> {
        if inode == fuse::FUSE_ROOT_ID {
            return Some((Path::new("/").to_owned(), None));
        }

        self.inodes
            .get(&inode)
            .map(|entry| (entry.path.clone(), entry.dev))
    }

    fn reply_add_dirs<P>(
        &self,
        mut reply: fuse::ReplyDirectory,
        names: impl IntoIterator<Item = P>,
        skip: usize,
    ) where
        P: AsRef<Path>,
    {
        let mut offset = 1_i64;
        for name in names.into_iter().skip(skip) {
            for (ino, entry) in &self.inodes {
                if entry.path == name.as_ref() {
                    if reply.add(
                        *ino,
                        offset,
                        entry.attr.kind,
                        entry.path.file_name().unwrap().to_str().unwrap(),
                    ) {
                        return reply.ok();
                    };

                    offset += 1;
                }
            }
        }

        reply.ok()
    }

    fn cache_dir(&mut self, p: impl AsRef<Path>, dev: Option<u64>) -> fuse::FileAttr {
        let attr = fuse::FileAttr {
            ino: 0,
            size: 0,
            blocks: 0,
            atime: self.ctime,
            mtime: self.ctime,
            ctime: self.ctime,
            crtime: time::SystemTime::UNIX_EPOCH,
            kind: fuse::FileType::Directory,
            perm: 0o777,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 512,
            flags: 0,
        };

        self.get_or_insert(p, attr, dev)
    }

    fn cache_file(&mut self, p: impl AsRef<Path>, dev: Option<u64>, len: usize) -> fuse::FileAttr {
        let attr = fuse::FileAttr {
            ino: 0,
            size: len as u64,
            blocks: 0,
            atime: time::UNIX_EPOCH,
            mtime: time::UNIX_EPOCH,
            ctime: time::UNIX_EPOCH,
            crtime: time::UNIX_EPOCH,
            kind: fuse::FileType::RegularFile,
            perm: 0o777,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 512,
            flags: 0,
        };

        self.get_or_insert(p, attr, dev)
    }

    fn cache_symlink(&mut self, p: impl AsRef<Path>, dev: Option<u64>) -> fuse::FileAttr {
        let attr = fuse::FileAttr {
            ino: 0,
            size: 0,
            blocks: 0,
            atime: self.ctime,
            mtime: self.ctime,
            ctime: self.ctime,
            crtime: time::SystemTime::UNIX_EPOCH,
            kind: fuse::FileType::Symlink,
            perm: 0o777,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 512,
            flags: 0,
        };

        self.get_or_insert(p, attr, dev)
    }
}

/// A FUSE filesystem designed to fool libudev. All incoming paths are intended
/// to be absolute. The following paths are emulated:
///   - /sys/devices/virtual/input: contains folders for each virtual input
///     device. Contains both a top-level folder, inputX, and an eventX folder
///     for the evdev node.
///   - /sys/class/input: contains symlinks to the above device entries.
///   - /sys/class/hidraw: empty, so that no hidraw devices can be found
///   - /run/udev/control: an empty file that indicates udev is running
///   - /run/udev/data: contains "c{major}:{minor}" files with metadata on each
///     device.
pub struct UdevFs {
    state: Arc<Mutex<super::InputManagerState>>,
    tree: InodeCache,
}

impl UdevFs {
    pub fn new(state: Arc<Mutex<super::InputManagerState>>) -> Self {
        Self {
            state,
            tree: InodeCache {
                inodes: Default::default(),
                next_inode: fuse::FUSE_ROOT_ID + 1,
                ctime: time::SystemTime::now(),
            },
        }
    }
}

impl fuse::Filesystem for UdevFs {
    fn lookup(
        &mut self,
        _req: &fuse::Request<'_>,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuse::ReplyEntry,
    ) {
        let Some(name) = name.to_str() else {
            warn!(?name, "invalid lookup name");
            return reply.error(ENOENT);
        };

        let inodes = &mut self.tree;
        let Some((parent_path, dev)) = inodes.lookup_name(parent) else {
            warn!(?parent, ?name, "lookup failed");
            return reply.error(ENOENT);
        };

        debug!(?parent_path, ?name, dev, "lookup");
        match (parent_path.to_str().unwrap(), name, dev) {
            ("/", "sys", _) => reply.entry(&ZERO_TTL, &inodes.cache_dir("/sys", None), 0),
            ("/sys", "class", _) => {
                reply.entry(&ZERO_TTL, &inodes.cache_dir("/sys/class", None), 0)
            }
            ("/sys/class", "input", _) => {
                reply.entry(&ZERO_TTL, &inodes.cache_dir("/sys/class/input", None), 0)
            }
            ("/sys/class/input", name, _) => {
                let Some(dev) = self
                    .state
                    .lock()
                    .device_by_eventname(name)
                    .map(|dev| dev.id)
                else {
                    warn!(name, "device not found in /sys/class/input");
                    return reply.error(ENOENT);
                };

                reply.entry(
                    &ZERO_TTL,
                    &inodes.cache_symlink(parent_path.join(name), Some(dev)),
                    0,
                );
            }
            ("/sys/class", "hidraw", _) => {
                reply.entry(&ZERO_TTL, &inodes.cache_dir("/sys/class/hidraw", None), 0)
            }
            ("/sys", "devices", _) => {
                reply.entry(&ZERO_TTL, &inodes.cache_dir("/sys/devices", None), 0)
            }
            ("/sys/devices", "virtual", _) => reply.entry(
                &ZERO_TTL,
                &inodes.cache_dir("/sys/devices/virtual", None),
                0,
            ),
            ("/sys/devices/virtual", "input", _) => reply.entry(
                &ZERO_TTL,
                &inodes.cache_dir("/sys/devices/virtual/input", None),
                0,
            ),
            ("/sys/devices/virtual/input", name, _) => {
                let Some(dev) = self.state.lock().device_by_devname(name).map(|dev| dev.id) else {
                    warn!(name, "device not found in /sys/devices/virtual/input");
                    return reply.error(ENOENT);
                };

                reply.entry(
                    &ZERO_TTL,
                    &inodes.cache_dir(parent_path.join(name), Some(dev)),
                    0,
                );
            }
            (p, "uevent", Some(dev)) if p.starts_with("/sys/devices/virtual/input") => {
                let guard = self.state.lock();
                let Some(dev) = guard.device_by_id(dev) else {
                    warn!(?p, dev, "device not found in /sys/devices/virtual/input");
                    return reply.error(ENOENT);
                };

                // Inside the device directory, there are two levels of subdirectories.
                let path = parent_path
                    .strip_prefix(Path::new("/sys/devices/virtual/input"))
                    .unwrap();
                if path.as_os_str().is_empty() {
                    unreachable!() // Handled by the case above this one.
                }

                // Distinguish between the inputX uevent and the eventX uevent.
                let content = if path.to_str().unwrap() == dev.devname {
                    make_input_uevent(dev)
                } else if path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("event")
                {
                    make_evdev_uevent(dev)
                } else {
                    warn!(?parent_path, "unrecognized uevent path");
                    return reply.error(ENOENT);
                };

                reply.entry(
                    &ZERO_TTL,
                    &self
                        .tree
                        .cache_file(parent_path.join("uevent"), Some(dev.id), content.len()),
                    0,
                );
            }
            (p, "subsystem", Some(dev)) if p.starts_with("/sys/devices/virtual/input") => {
                reply.entry(
                    &ZERO_TTL,
                    &self
                        .tree
                        .cache_symlink(parent_path.join("subsystem"), Some(dev)),
                    0,
                );
            }
            (p, name, Some(dev))
                if p.starts_with("/sys/devices/virtual/input") && name.starts_with("event") =>
            {
                // This is /sys/devices/virtual/input/inputX/eventX.
                reply.entry(
                    &ZERO_TTL,
                    &inodes.cache_dir(parent_path.join(name), Some(dev)),
                    0,
                );
            }
            ("/", "run", _) => reply.entry(&ZERO_TTL, &inodes.cache_dir("/run", None), 0),
            ("/run", "udev", _) => reply.entry(&ZERO_TTL, &inodes.cache_dir("/run/udev", None), 0),
            ("/run/udev", "control", _) => reply.entry(
                &ZERO_TTL,
                &inodes.cache_file("/run/udev/control", None, 0),
                0,
            ),
            ("/run/udev", "data", _) => {
                reply.entry(&ZERO_TTL, &inodes.cache_dir("/run/udev/data", None), 0)
            }
            ("/run/udev", "udev.conf.d", _) => reply.error(ENOENT),

            ("/run/udev/data", name, _) => {
                let guard = self.state.lock();
                for dev in &guard.devices {
                    if name == format!("c13:{}", dev.counter) {
                        return reply.entry(
                            &ZERO_TTL,
                            &inodes.cache_file(
                                parent_path.join(name),
                                Some(dev.id),
                                UDEV_INPUT_DATA.len(),
                            ),
                            0,
                        );
                    }
                }

                warn!(?name, "no device found in /run/udev/data");
                reply.error(ENOENT);
            }
            (parent_name, name, dev) => {
                warn!(parent_name, name, dev, "udevfs lookup failed");
                reply.error(ENOENT);
            }
        }
    }

    fn getattr(
        &mut self,
        _req: &fuse::Request<'_>,
        ino: u64,
        _fh: Option<u64>,
        reply: fuse::ReplyAttr,
    ) {
        let Some(entry) = self.tree.inodes.get(&ino) else {
            warn!(ino, "lookup failed");
            return reply.error(ENOENT);
        };

        reply.attr(&ZERO_TTL, &entry.attr);
    }

    fn readlink(&mut self, _req: &fuse::Request<'_>, ino: u64, reply: fuse::ReplyData) {
        let Some(entry) = self.tree.inodes.get(&ino) else {
            warn!(ino, "lookup failed");
            return reply.error(ENOENT);
        };

        debug!(path = ?entry.path, "readlink");
        if let Some(name) = matches_prefix_with_name(&entry.path, "/sys/class/input") {
            let guard = self.state.lock();
            let Some(dev) = guard.device_by_eventname(name) else {
                warn!(eventname = ?name, "device not found in /sys/devices/virtual/input");
                return reply.error(ENOENT);
            };

            let dst = Path::new("/sys/devices/virtual/input")
                .join(&dev.devname)
                .join(name);
            debug!(?dst, "returning from readlink");
            reply.data(dst.as_os_str().as_encoded_bytes());
        } else if entry.path.starts_with("/sys/devices")
            && entry.path.file_name() == Some(Path::new("subsystem").as_os_str())
        {
            return reply.data(b"/sys/class/input");
        } else {
            warn!(path = ?entry.path, dev = ?entry.dev, "readlink failed");
            reply.error(ENOENT);
        }
    }

    fn read(
        &mut self,
        _req: &fuse::Request<'_>,
        ino: u64,
        _fh: u64,
        _offset: i64,
        _size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuse::ReplyData,
    ) {
        let Some(entry) = self.tree.inodes.get(&ino) else {
            warn!(ino, "lookup failed");
            return reply.error(EBADF);
        };

        debug!(path = ?entry.path, "read");

        if entry.path.starts_with("/run/udev/data") {
            reply.data(UDEV_INPUT_DATA);
        } else if entry.dev.is_some()
            && entry.path.starts_with("/sys/devices")
            && entry.path.file_name() == Some(Path::new("uevent").as_os_str())
        {
            let guard = self.state.lock();
            let Some(dev) = guard.device_by_id(entry.dev.unwrap()) else {
                warn!(dev = ?entry.dev, "device lookup failed");
                return reply.error(EBADF);
            };

            let mut parent_path = entry.path.clone();
            parent_path.pop();

            if parent_path.file_name() == Some(&dev.eventname) {
                reply.data(&make_evdev_uevent(dev))
            } else if parent_path.file_name() == Some(&dev.devname) {
                reply.data(&make_input_uevent(dev))
            } else {
                warn!(?entry.path, "bad uevent path");
                reply.error(EBADF);
            }
        } else {
            warn!(path = ?entry.path, dev = entry.dev, "read failed");
            reply.error(EBADF);
        }
    }

    fn readdir(
        &mut self,
        _req: &fuse::Request<'_>,
        ino: u64,
        _fh: u64,
        skip: i64,
        mut reply: fuse::ReplyDirectory,
    ) {
        let inodes = &mut self.tree;
        let Some(Entry { path, dev, .. }) = inodes.inodes.get(&ino).cloned() else {
            warn!(ino, "lookup failed");
            return reply.error(EBADF);
        };

        debug!(?path, ?dev, "readdir");

        let skip = skip as usize;
        match (path.to_str().unwrap(), dev) {
            ("/", _) => inodes.reply_add_dirs(reply, ["sys", "run"], skip),
            ("/sys", _) => inodes.reply_add_dirs(reply, ["class", "devices"], skip),
            ("/sys/class", _) => inodes.reply_add_dirs(reply, ["input", "hidraw"], skip),
            ("/sys/class/input", _) => {
                let guard = self.state.lock();

                debug!("udev is enumerating devices in /sys/class/input");
                for (idx, DeviceState { id, eventname, .. }) in
                    guard.devices.iter().skip(skip).enumerate()
                {
                    let attr = inodes.cache_symlink(path.join(eventname), Some(*id));

                    if reply.add(
                        attr.ino,
                        (idx as i64) + 1,
                        fuse::FileType::Symlink,
                        eventname,
                    ) {
                        break;
                    }
                }

                reply.ok();
            }
            ("/sys/class/hidraw", _) => {
                reply.ok() // Empty.
            }
            ("/sys/devices", _) => inodes.reply_add_dirs(reply, ["virtual"], skip),
            ("/sys/devices/virtual", _) => inodes.reply_add_dirs(reply, ["input"], skip),
            ("/sys/devices/virtual/input", _) => {
                let guard = self.state.lock();

                debug!("udev is enumerating devices in /sys/devices/virtual/input");
                for (idx, DeviceState { id, devname, .. }) in
                    guard.devices.iter().skip(skip).enumerate()
                {
                    let attr = inodes.cache_dir(path.join(devname), Some(*id));

                    if reply.add(
                        attr.ino,
                        (idx as i64) + 1,
                        fuse::FileType::Directory,
                        devname,
                    ) {
                        break;
                    }
                }

                reply.ok();
            }
            (_p, Some(_))
                if matches_prefix_with_name(&path, "/sys/devices/virtual/input").is_some() =>
            {
                // Note: this seems not to happen.
                // inodes.reply_add_dirs(reply, ["subsystem", "capabilities",
                // "uevent"], skip)
            }
            ("/run", _) => inodes.reply_add_dirs(reply, ["udev"], skip),
            ("/run/udev", _) => inodes.reply_add_dirs(reply, ["control", "data"], skip),
            ("/run/udev/data", _) => {
                // Note: this seems not to happen.
            }
            _ => {
                warn!(?path, ?dev, "readdir failed");
                reply.error(ENOENT);
            }
        }
    }

    fn access(&mut self, _req: &fuse::Request<'_>, _ino: u64, _mask: i32, reply: fuse::ReplyEmpty) {
        reply.ok()
    }

    fn release(
        &mut self,
        _req: &fuse::Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuse::ReplyEmpty,
    ) {
        reply.ok()
    }
}

fn make_input_uevent(_dev: &DeviceState) -> Vec<u8> {
    // TODO hack
    br#"PRODUCT=3/45e/2ea/408
NAME="Magic Mirror Emulated Controller"
EV=20000b
KEY=7fdb000000000000 0 0 0 0
ABS=3003f
UNIQ="d0:bc:c1:db:1d:2f"
"#
    .to_vec()
}

fn make_evdev_uevent(dev: &DeviceState) -> Vec<u8> {
    format!(
        "MAJOR=13\nMINOR={}\nDEVNAME=input/{}\n",
        dev.counter,
        dev.eventname.to_str().unwrap()
    )
    .as_bytes()
    .to_vec()
}

fn matches_prefix_with_name(p: &Path, prefix: impl AsRef<Path>) -> Option<&OsStr> {
    match p.strip_prefix(prefix).ok()?.components().next() {
        Some(std::path::Component::Normal(devname)) => Some(devname),
        _ => None,
    }
}
