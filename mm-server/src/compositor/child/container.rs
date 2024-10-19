// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::OpenOptions,
    io,
    os::{
        fd::{AsFd, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd},
        unix::process::CommandExt as _,
    },
    path::{Path, PathBuf},
    process::Command,
    str::FromStr as _,
    time,
};

use anyhow::{anyhow, Context as _};
use pathsearch::find_executable_in_path;
use rand::distributions::{Alphanumeric, DistString as _};
use rustix::{
    fs::{mkdirat, openat, symlinkat, FileType, Gid, Mode, OFlags, Uid, CWD as AT_FDCWD},
    io::{fcntl_dupfd_cloexec, write, Errno},
    mount::{
        fsconfig_create, fsconfig_set_flag, fsconfig_set_string, fsmount, fsopen, move_mount,
        open_tree, FsMountFlags, FsOpenFlags, MountAttrFlags, MoveMountFlags, OpenTreeFlags,
    },
    process::{getgid, getuid, set_parent_process_death_signal, waitpid, Pid, Signal, WaitOptions},
    thread::{move_into_link_name_space, LinkNameSpaceType},
};
use tracing::debug;

mod ipc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeIsolationMode {
    Unisolated,
    Tmpfs,
    Permanent(PathBuf),
}

impl From<crate::config::HomeIsolationMode> for HomeIsolationMode {
    fn from(value: crate::config::HomeIsolationMode) -> Self {
        match value {
            crate::config::HomeIsolationMode::Unisolated => Self::Unisolated,
            crate::config::HomeIsolationMode::Tmpfs => Self::Tmpfs,
            crate::config::HomeIsolationMode::Permanent(p) => Self::Permanent(p),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DevBindMount {
    path: &'static str,
    is_dir: bool,
}

const DEV_BIND_MOUNTS: &[DevBindMount] = &[
    DevBindMount {
        path: "/dev/null",
        is_dir: false,
    },
    DevBindMount {
        path: "/dev/zero",
        is_dir: false,
    },
    DevBindMount {
        path: "/dev/full",
        is_dir: false,
    },
    DevBindMount {
        path: "/dev/tty",
        is_dir: false,
    },
    DevBindMount {
        path: "/dev/random",
        is_dir: false,
    },
    DevBindMount {
        path: "/dev/urandom",
        is_dir: false,
    },
    DevBindMount {
        path: "/dev/dri",
        is_dir: true,
    },
    DevBindMount {
        path: "/dev/fuse",
        is_dir: false,
    },
];

#[cfg(debug_assertions)]
struct UnbufferedStderr<'a>(BorrowedFd<'a>);

#[cfg(debug_assertions)]
impl<'a> std::fmt::Write for UnbufferedStderr<'a> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        write(self.0, s.as_bytes()).map_err(|_| std::fmt::Error)?;
        Ok(())
    }
}

#[cfg(debug_assertions)]
macro_rules! preexec_debug {
    ($($arg:tt)+) => {
        #[allow(unused_imports)]
        use std::fmt::Write as _;

        let mut stderr = UnbufferedStderr(rustix::stdio::stderr());
        let _ = std::write!(stderr, "[PRE-EXEC] ");
        let _ = std::writeln!(stderr, $($arg)*);
    }
}

#[cfg(not(debug_assertions))]
macro_rules! preexec_debug {
    ($($arg:tt)*) => {};
}

unsafe fn _must<T>(_op: &str, res: rustix::io::Result<T>) -> T {
    loop {
        match res {
            Ok(v) => return v,
            Err(Errno::INTR) => continue,
            Err(_e) => {
                #[cfg(debug_assertions)]
                {
                    use std::fmt::Write as _;
                    let mut stderr = UnbufferedStderr(rustix::stdio::stderr());

                    let _ = std::writeln!(stderr, "[PRE-EXEC] {_op}: {_e}");
                    let _ = std::writeln!(stderr);
                }

                libc::_exit(1);
            }
        }
    }
}

macro_rules! must {
    ($n:ident( $($args:tt)* )) => {{
        let res = $n( $($args)* );
        _must(stringify!($n), res)
    }};
}

type SetupHook = Box<dyn FnOnce(&mut super::ChildHandle) -> anyhow::Result<()>>;

/// A lightweight linux container. Currently we use the following namespaces:
///  - A mount namespace, to mount tmpfs on /dev, /tmp, /run, etc, and
///    potentially to isolate home as well. We don't pivot_root/chroot.
///  - A PID namespace, so that processes get cleaned up when a session ends.
///    Note that we currently don't use a "stub init" process to handle
///    reparenting or reaping, since we don't expect to spawn lots of grandchild
///    processes.
///  - A user namespace, to enable the above. We just map the current user to
///    itself.
///
/// IMPORTANT: This container is not a secure container. Under NO CIRCUMSTANCES
/// should you use it to run untrusted code. Any security benefits are purely
/// incidental; this is more about containing mess (I'm looking at you, Steam).
pub struct Container {
    child_cmd: Command,

    // Note: we don't use Command::env or Command::env_clear, because those
    // cause Command::exec to allocate, which we don't want to do after forking.
    envs: Vec<CString>,

    tmp_stderr: Option<OwnedFd>,

    extern_home_path: Option<PathBuf>,
    intern_home_path: PathBuf,
    clear_home: bool,

    intern_run_path: PathBuf,
    extern_run_path: PathBuf,

    additional_bind_mounts: Vec<(PathBuf, PathBuf)>,
    internal_bind_mounts: Vec<(PathBuf, PathBuf, bool)>,

    // Stores a closure to run before unfreeze.
    setup_hooks: Vec<SetupHook>,

    uid: Uid,
    gid: Gid,
}

impl Container {
    pub fn new(
        mut args: Vec<OsString>,
        home_isolation_mode: HomeIsolationMode,
    ) -> anyhow::Result<Self> {
        let exe = args.remove(0);
        let exe_path =
            find_executable_in_path(&exe).ok_or(anyhow!("command {:?} not in PATH", &exe))?;

        let mut envs = Vec::new();

        let mut child_cmd = Command::new(exe_path);
        child_cmd.current_dir("/");
        child_cmd.args(args);

        if let Some(path) = std::env::var_os("PATH") {
            envs.push(make_putenv("PATH", path));
        }

        let uid = getuid();
        let gid = getgid();

        let intern_run_path: OsString = format!("/run/user/{}", uid.as_raw()).try_into().unwrap();
        envs.push(make_putenv("XDG_RUNTIME_DIR", intern_run_path.clone()));

        let extern_run_path = std::env::temp_dir().join(format!(
            "mm.{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 16),
        ));
        std::fs::create_dir_all(&extern_run_path)?;

        let intern_home_path: OsString = std::env::var_os("HOME").unwrap_or("/home/mm".into());
        envs.push(make_putenv("HOME", intern_home_path.clone()));

        debug!(home_mode = ?home_isolation_mode, "using home mode");
        let (extern_home_path, clear_home) = match home_isolation_mode {
            HomeIsolationMode::Unisolated => (None, false),
            HomeIsolationMode::Tmpfs => (None, true),
            HomeIsolationMode::Permanent(path) => {
                std::fs::create_dir_all(&path).context(format!(
                    "failed to create home directory {}",
                    path.display()
                ))?;

                (Some(path), true)
            }
        };

        Ok(Self {
            child_cmd,
            envs,
            tmp_stderr: None,

            intern_home_path: intern_home_path.into(),
            extern_home_path,
            clear_home,
            intern_run_path: intern_run_path.into(),
            extern_run_path,

            additional_bind_mounts: Vec::new(),
            internal_bind_mounts: Vec::new(),

            setup_hooks: Vec::new(),

            uid,
            gid,
        })
    }

    pub fn intern_run_path(&self) -> &Path {
        &self.intern_run_path
    }

    pub fn extern_run_path(&self) -> &Path {
        &self.extern_run_path
    }

    pub fn bind_mount(&mut self, src: impl AsRef<Path>, dst: impl AsRef<Path>) {
        self.additional_bind_mounts
            .push((src.as_ref().to_owned(), dst.as_ref().to_owned()));
    }

    pub fn internal_bind_mount(&mut self, src: impl AsRef<Path>, dst: impl AsRef<Path>) {
        self.internal_bind_mounts
            .push((src.as_ref().to_owned(), dst.as_ref().to_owned(), true));
    }

    pub fn setup_hook(
        &mut self,
        f: impl FnOnce(&mut super::ChildHandle) -> anyhow::Result<()> + 'static,
    ) {
        self.setup_hooks.push(Box::new(f))
    }

    pub unsafe fn pre_exec(&mut self, f: impl FnMut() -> io::Result<()> + Send + Sync + 'static) {
        self.child_cmd.pre_exec(f);
    }

    pub fn set_env<K, V>(&mut self, key: K, val: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.envs.push(make_putenv(key, val))
    }

    pub fn set_stdout<T: AsFd>(&mut self, stdio: T) -> anyhow::Result<()> {
        let stdout = fcntl_dupfd_cloexec(&stdio, 0)?;
        self.child_cmd.stdout(stdout);

        Ok(())
    }

    pub fn set_stderr<T: AsFd>(&mut self, stdio: T) -> anyhow::Result<()> {
        let stderr = fcntl_dupfd_cloexec(&stdio, 0)?;
        let tmp_stderr = fcntl_dupfd_cloexec(&stdio, 0)?;

        self.child_cmd.stderr(stderr);
        self.tmp_stderr = Some(tmp_stderr);

        Ok(())
    }

    pub fn spawn(mut self) -> anyhow::Result<super::ChildHandle> {
        // Prepare bind mounts.
        let mut mounts = DEV_BIND_MOUNTS
            .iter()
            .map(|m| {
                Ok((
                    PathBuf::from_str(m.path).unwrap(),
                    PathBuf::from_str(m.path).unwrap(),
                    m.is_dir,
                    None,
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        for (src, dst) in self.additional_bind_mounts.drain(..) {
            let is_dir = std::fs::metadata(&src)
                .context("failed to stat bind mount")?
                .is_dir();

            mounts.push((src, dst, is_dir, None))
        }

        let mut child_pidfd = -1;
        let mut args = clone3::Clone3::default();
        args.flag_pidfd(&mut child_pidfd)
            .exit_signal(libc::SIGCHLD as _)
            .flag_newuser()
            .flag_newns()
            .flag_newpid();

        debug!(cmd = ?self.child_cmd, "spawning child process");

        let (barrier, child_barrier) = ipc::EventfdBarrier::new()?;

        // clone off a child process, which does some setup before execing the
        // app.
        let child_stderr = self.tmp_stderr.take();
        let child_pid = match unsafe { args.call().context("clone3")? } {
            0 => unsafe {
                self.child_after_fork(child_stderr.as_ref(), child_barrier, &mut mounts)
            },
            pid => pid,
        };

        let child_pidfd = unsafe { OwnedFd::from_raw_fd(child_pidfd) };

        set_uid_map(child_pid, self.uid, self.gid).context("failed to set uid/gid map")?;

        // Wait for the child to signal that it's ready.
        barrier
            .sync(time::Duration::from_secs(1))
            .context("timed out waiting for forked child (phase 1)")?;

        let mut handle = super::ChildHandle {
            pid: Pid::from_raw(child_pid).unwrap(),
            pidfd: child_pidfd,
            run_path: self.extern_run_path,
        };

        for hook in self.setup_hooks.drain(..) {
            hook(&mut handle)?;
        }

        // Unfreeze the child.
        barrier
            .sync(time::Duration::from_secs(1))
            .context("timed out waiting for forked child (phase 2)")?;

        Ok(handle)
    }

    // Signal safety dictates what we can do here, and it's not a lot. The main
    // thing we avoid is allocations. Note that rustix is added as a dependency
    // without the 'alloc' feature.
    unsafe fn child_after_fork<FD>(
        mut self,
        stderr: Option<FD>,
        barrier: ipc::EventfdBarrier,
        bind_mounts: &mut [(PathBuf, PathBuf, bool, Option<OwnedFd>)],
    ) -> !
    where
        FD: AsFd,
    {
        // See above for how logging is implemented to avoid the possibility of
        // allocation.
        if let Some(fd) = &stderr {
            let _ = rustix::stdio::dup2_stderr(fd.as_fd()); // Replace stderr.
        }

        // Tell the kernel to SIGKILL us when our parent (mmserver) dies. this
        // is particularly important because we're PID 1, so the kernel won't
        // kill on SIGINT/SIGQUIT/etc if the child process doesn't have a signal
        // handler set up for them.
        must!(set_parent_process_death_signal(Some(Signal::Kill)));

        preexec_debug!("starting container setup");

        // Mount /proc first.
        must!(mount_fs(
            c"proc",
            c"/proc",
            MountAttrFlags::MOUNT_ATTR_NOEXEC
                | MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NODEV,
            &[],
        ));

        // Collect detached mounts we want to bind-mount later. We can't
        // allocate a vec, so we fill in the Options in the passed-in vec
        // instead.
        preexec_debug!("collecting detached bind mounts");
        for (src_path, _, _, ref mut device_fd) in bind_mounts.iter_mut() {
            let fd = must!(detach_mount(src_path,));

            *device_fd = Some(fd)
        }

        // Grab a detached mount for the temporary dir we're going to mount as
        // XDG_RUNTIME_DIR.
        let detached_run_fd = must!(detach_mount(&self.extern_run_path,));

        // Grab a detached mount for home, if we're using one.
        let detached_home = self
            .extern_home_path
            .as_ref()
            .map(|p| must!(detach_mount(p)));

        // Mount /dev and a few other filesystems.
        must!(mount_fs(
            c"tmpfs",
            c"/dev",
            MountAttrFlags::MOUNT_ATTR_NOEXEC | MountAttrFlags::MOUNT_ATTR_STRICTATIME,
            &[(c"mode", c"0755")],
        ));

        must!(mount_fs(
            c"tmpfs",
            c"/dev/shm",
            MountAttrFlags::MOUNT_ATTR_NOEXEC
                | MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NODEV,
            &[(c"mode", c"1777"), (c"size", c"512m")],
        ));

        // TODO: this errors with EPERM.
        // must!(mount_fs(
        //     "mqueue",
        //     "/dev/mqueue",
        //     MountAttrFlags::MOUNT_ATTR_NOEXEC
        //         | MountAttrFlags::MOUNT_ATTR_NOSUID
        //         | MountAttrFlags::MOUNT_ATTR_NODEV,
        //     &[],
        // ));

        must!(mount_fs(
            c"devpts",
            c"/dev/pts",
            MountAttrFlags::MOUNT_ATTR_NOEXEC | MountAttrFlags::MOUNT_ATTR_NOSUID,
            &[
                (c"newinstance", c""),
                (c"ptmxmode", c"0666"),
                (c"mode", c"0620"),
                // TODO: do we need to add a tty group?
                // ("gid", "5"),
            ],
        ));

        // Symlink /dev/fd -> /proc/self/fd, etc.
        must!(symlinkat(c"/proc/self/fd", AT_FDCWD, c"/dev/fd"));
        must!(symlinkat(c"/proc/self/fd/0", AT_FDCWD, c"/dev/stdin"));
        must!(symlinkat(c"/proc/self/fd/1", AT_FDCWD, c"/dev/stdout"));
        must!(symlinkat(c"/proc/self/fd/2", AT_FDCWD, c"/dev/stderr"));

        // Prepare /dev/input.
        must!(mkdirat(
            AT_FDCWD,
            "/dev/input",
            Mode::from_bits(0o755).unwrap()
        ));

        must!(mount_fs(
            c"tmpfs",
            c"/run",
            MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NODEV
                | MountAttrFlags::MOUNT_ATTR_RELATIME,
            &[(c"mode", c"0700"), (c"size", c"1g")],
        ));

        must!(mkdirat(
            AT_FDCWD,
            "/run/user",
            Mode::from_bits(0o700).unwrap()
        ));

        must!(mount_fs(
            c"tmpfs",
            c"/tmp",
            MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NOEXEC
                | MountAttrFlags::MOUNT_ATTR_NOATIME,
            &[(c"mode", c"0777"), (c"size", c"1g")],
        ));

        if self.clear_home {
            must!(mount_fs(
                c"tmpfs",
                c"/home",
                MountAttrFlags::MOUNT_ATTR_NOSUID
                    | MountAttrFlags::MOUNT_ATTR_NOEXEC
                    | MountAttrFlags::MOUNT_ATTR_NOATIME,
                &[(c"mode", c"0777"), (c"size", c"1g")],
            ));

            must!(mkdirat(
                AT_FDCWD,
                &self.intern_home_path,
                Mode::from_bits(0o700).unwrap()
            ));
        }

        // Mount XDG_RUNTIME_DIR.
        preexec_debug!(
            "bind-mounting {} to {}",
            self.extern_run_path.display(),
            self.intern_run_path.display()
        );

        must!(mkdirat(AT_FDCWD, &self.intern_run_path, Mode::empty()));
        must!(reattach_mount(detached_run_fd, &self.intern_run_path));

        // Mount HOME.
        if let Some(fd) = detached_home {
            preexec_debug!(
                "bind-mounting {} to {}",
                self.extern_home_path.as_ref().unwrap().display(),
                self.intern_home_path.display()
            );

            must!(reattach_mount(fd, &self.intern_home_path));
        }

        // Attach detached bind mounts, now that the filesystem is prepared.
        for (_src_path, dst_path, is_dir, mount_fd) in bind_mounts {
            preexec_debug!(
                "bind-mounting {} (outside) to {} (inside)",
                _src_path.display(),
                dst_path.display()
            );

            let detached_mount_fd = mount_fd.take().unwrap();

            if *is_dir {
                let _ = mkdirat(AT_FDCWD, &*dst_path, Mode::empty());
            } else {
                must!(touch(&*dst_path, Mode::empty()));
            }

            must!(reattach_mount(detached_mount_fd, dst_path));
        }

        preexec_debug!("finished initial setup, waiting for mmserver");

        // Sync with mmserver.
        must!(sync_barrier(&barrier));
        must!(sync_barrier(&barrier));

        // Finally, internal bind mounts. We do this after syncing with mmserver
        // in case mmserver wants us to bind-mount something it just mounted.
        for (src_path, dst_path, is_dir) in &self.internal_bind_mounts {
            preexec_debug!(
                "bind-mounting {} to {}",
                src_path.display(),
                dst_path.display()
            );

            let fd = must!(detach_mount(src_path));

            if *is_dir {
                let _ = mkdirat(AT_FDCWD, dst_path, Mode::empty());
            } else {
                must!(touch(dst_path, Mode::empty()));
            }

            must!(reattach_mount(fd, dst_path));
        }

        // TODO: Install seccomp handlers here.

        // We don't trust std::os::Command's env handling, because sometimes
        // it allocates.
        libc::clearenv();
        for v in &mut self.envs {
            libc::putenv(v.as_ptr() as *mut _);
        }

        // If successful, this never returns.
        let _e = self.child_cmd.exec();

        preexec_debug!("execve failed: {_e}");
        libc::_exit(1);
    }
}

fn set_uid_map(child_pid: i32, uid: rustix::fs::Uid, gid: rustix::fs::Gid) -> anyhow::Result<()> {
    let uid = uid.as_raw();
    let gid = gid.as_raw();

    write(
        OpenOptions::new()
            .write(true)
            .open(format!("/proc/{}/setgroups", child_pid))?,
        b"deny",
    )
    .context("failed to write setgroups=deny")?;

    write(
        OpenOptions::new()
            .write(true)
            .open(format!("/proc/{}/uid_map", child_pid))
            .context("open failed")?,
        format!("{uid} {uid} 1\n").as_bytes(),
    )
    .context("failed to write uid_map")?;

    write(
        OpenOptions::new()
            .write(true)
            .open(format!("/proc/{}/gid_map", child_pid))
            .context("open failed")?,
        format!("{gid} {gid} 1\n").as_bytes(),
    )
    .context("failed to write gid_map")?;

    Ok(())
}

fn run_in_container<F>(ns_pidfd: impl AsFd, stderr: Option<BorrowedFd<'_>>, f: F) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    let child_pid = unsafe { libc::fork() };
    if child_pid == -1 {
        return Err(io::Error::last_os_error());
    } else if child_pid == 0 {
        unsafe {
            if let Some(fd) = &stderr {
                let _ = rustix::stdio::dup2_stderr(fd.as_fd()); // Replace stderr.
            }

            must!(set_parent_process_death_signal(Some(Signal::Kill)));

            must!(move_into_link_name_space(
                ns_pidfd.as_fd(),
                Some(LinkNameSpaceType::User)
            ));

            must!(move_into_link_name_space(
                ns_pidfd.as_fd(),
                Some(LinkNameSpaceType::Mount)
            ));

            if let Err(_e) = f() {
                preexec_debug!("run_in_container: {_e}");
                libc::_exit(1);
            }

            libc::_exit(0);
        }
    }

    loop {
        match waitpid(
            Some(Pid::from_raw(child_pid).unwrap()),
            WaitOptions::empty(),
        ) {
            Ok(st) => match st {
                Some(st) if st.as_raw() == 0 => return Ok(()),
                _ => return Err(io::Error::other("forked process exited with error")),
            },
            Err(Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

pub(super) fn fuse_mount(
    ns_pidfd: impl AsFd,
    dst: impl AsRef<Path>,
    fsname: String,
    st_mode: u32,
) -> io::Result<OwnedFd> {
    debug!("mounting {fsname} to {}", dst.as_ref().display());

    let (fd_tx, fd_rx) = ipc::fd_oneshot()?;
    let uid = CString::new(format!("{}", getuid().as_raw())).unwrap();
    let gid = CString::new(format!("{}", getgid().as_raw())).unwrap();
    let rootmode = CString::new(format!("{st_mode:o}")).unwrap();

    let is_dir = FileType::from_raw_mode(st_mode) == FileType::Directory;

    run_in_container(ns_pidfd, None, move || {
        let fuse_device_fd = openat(
            AT_FDCWD,
            "/dev/fuse",
            OFlags::RDWR | OFlags::CLOEXEC,
            Mode::empty(),
        )?;

        // Send the fd back to mmserver.
        fd_tx.send_timeout(fuse_device_fd.try_clone()?, time::Duration::from_secs(1))?;

        // format! allocates.
        let mut fd_buf = [0_u8; 32];
        let fd_str = {
            use std::io::Write;
            write!(
                &mut io::Cursor::new(&mut fd_buf[..]),
                "{}",
                fuse_device_fd.as_raw_fd()
            )?;

            CStr::from_bytes_until_nul(&fd_buf[..])
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid FD"))?
        };

        if is_dir {
            let _ = mkdirat(AT_FDCWD, dst.as_ref(), Mode::from_raw_mode(st_mode));
        } else {
            let _ = touch(dst.as_ref(), Mode::from_raw_mode(st_mode));
        }

        let fsfd = fsopen(c"fuse", FsOpenFlags::FSOPEN_CLOEXEC)?;
        fsconfig_set_string(fsfd.as_fd(), c"fd", fd_str)?;
        fsconfig_set_string(fsfd.as_fd(), c"user_id", &uid)?;
        fsconfig_set_string(fsfd.as_fd(), c"group_id", &gid)?;
        fsconfig_set_string(fsfd.as_fd(), c"rootmode", &rootmode)?;
        fsconfig_create(fsfd.as_fd())?;

        let mount_fd = fsmount(
            fsfd.as_fd(),
            FsMountFlags::FSMOUNT_CLOEXEC,
            MountAttrFlags::MOUNT_ATTR_NOEXEC
                | MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NODEV,
        )?;

        move_mount(
            mount_fd.as_fd(),
            c"",
            AT_FDCWD,
            dst.as_ref(),
            MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
        )?;

        Ok(())
    })?;

    fd_rx.recv_timeout(time::Duration::from_secs(1))
}

fn touch(path: impl AsRef<Path>, mode: impl Into<Mode>) -> rustix::io::Result<()> {
    let _ = openat(
        AT_FDCWD,
        path.as_ref(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC,
        mode.into(),
    )?;

    Ok(())
}

fn detach_mount(path: impl AsRef<Path>) -> rustix::io::Result<OwnedFd> {
    open_tree(
        AT_FDCWD,
        path.as_ref(),
        OpenTreeFlags::OPEN_TREE_CLONE
            | OpenTreeFlags::AT_RECURSIVE
            | OpenTreeFlags::OPEN_TREE_CLOEXEC,
    )
}

fn reattach_mount(fd: OwnedFd, path: impl AsRef<Path>) -> rustix::io::Result<()> {
    move_mount(
        fd.as_fd(),
        "",
        AT_FDCWD,
        path.as_ref(),
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
}

fn mount_fs(
    fstype: &CStr,
    dst: &CStr,
    options: MountAttrFlags,
    configs: &[(&CStr, &CStr)],
) -> rustix::io::Result<()> {
    preexec_debug!("mounting {fstype:?} on {dst:?}");

    let fsfd = fsopen(fstype, FsOpenFlags::FSOPEN_CLOEXEC)?;

    for (k, v) in configs {
        if v.is_empty() {
            fsconfig_set_flag(fsfd.as_fd(), *k)?;
        } else {
            fsconfig_set_string(fsfd.as_fd(), *k, *v)?;
        }
    }

    fsconfig_create(fsfd.as_fd())?;
    let mount_fd = fsmount(fsfd.as_fd(), FsMountFlags::FSMOUNT_CLOEXEC, options)?;

    let _ = mkdirat(AT_FDCWD, dst, Mode::empty());
    move_mount(
        mount_fd.as_fd(),
        c"",
        AT_FDCWD,
        dst,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    )?;

    Ok(())
}

// Wrapped in a function for compatibility with the must! macro.
fn sync_barrier(barrier: &ipc::EventfdBarrier) -> rustix::io::Result<()> {
    barrier.sync(time::Duration::from_secs(1))
}

/// Generates a CString in the format key=value, for putenv(3).
fn make_putenv(k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> CString {
    CString::new(format!(
        "{}={}",
        k.as_ref().to_str().unwrap(),
        v.as_ref().to_str().unwrap()
    ))
    .unwrap()
}

#[cfg(test)]
mod test {
    use std::{fs::File, io::Read as _};

    use rustix::pipe::{pipe_with, PipeFlags};

    use crate::compositor::{child::container::HomeIsolationMode, Container};

    #[test_log::test]
    fn echo() -> anyhow::Result<()> {
        let mut container =
            Container::new(vec!["echo".into(), "done".into()], HomeIsolationMode::Tmpfs)?;
        let (pipe_rx, pipe_tx) = pipe_with(PipeFlags::CLOEXEC)?;
        container.set_stdout(pipe_tx)?;

        let mut child = container.spawn()?;
        child.wait()?;

        let mut buf = String::new();
        File::from(pipe_rx).read_to_string(&mut buf)?;

        pretty_assertions::assert_eq!(buf, "done\n");
        Ok(())
    }
}
