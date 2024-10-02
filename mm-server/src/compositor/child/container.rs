// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    ffi::{OsStr, OsString},
    fs::OpenOptions,
    os::{
        fd::{AsFd, BorrowedFd, FromRawFd as _, OwnedFd},
        unix::process::CommandExt as _,
    },
    path::{Path, PathBuf},
    process::Command,
    str::FromStr as _,
};

use anyhow::{anyhow, Context as _};
use pathsearch::find_executable_in_path;
use rand::distributions::{Alphanumeric, DistString as _};
use rustix::{
    event::EventfdFlags,
    fs::{mkdirat, openat, symlinkat, Gid, Mode, OFlags, Uid, CWD as AT_FDCWD},
    io::{fcntl_dupfd_cloexec, read, write, Errno},
    mount::{
        fsconfig_create, fsconfig_set_flag, fsconfig_set_string, fsmount, fsopen, move_mount,
        open_tree, FsMountFlags, FsOpenFlags, MountAttrFlags, MoveMountFlags, OpenTreeFlags,
    },
    process::{getgid, getuid, set_parent_process_death_signal, Pid, Signal},
};
use tracing::debug;

use crate::config::{self, AppConfig};

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
];

struct UnbufferedStderr<'a>(BorrowedFd<'a>);

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

unsafe fn _must<T>(op: &str, res: rustix::io::Result<T>) -> T {
    loop {
        match res {
            Ok(v) => return v,
            Err(Errno::INTR) => continue,
            Err(e) => {
                #[cfg(debug_assertions)]
                {
                    use std::fmt::Write as _;
                    let mut stderr = UnbufferedStderr(rustix::stdio::stderr());

                    let _ = std::writeln!(stderr, "[PRE-EXEC] {op}: {e}");
                    let _ = std::writeln!(stderr);
                }

                std::process::abort();
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
    envs: Vec<(OsString, OsString)>,

    tmp_stderr: Option<OwnedFd>,

    extern_home_path: Option<PathBuf>,
    intern_home_path: PathBuf,
    clear_home: bool,

    intern_run_path: PathBuf,
    extern_run_path: PathBuf,

    additional_bind_mounts: Vec<(PathBuf, PathBuf)>,
    internal_bind_mounts: Vec<(PathBuf, PathBuf, bool)>,

    uid: Uid,
    gid: Gid,
}

impl Container {
    pub fn new(app_config: AppConfig) -> anyhow::Result<Self> {
        let mut args = app_config.command.clone();
        let exe = args.remove(0);
        let exe_path =
            find_executable_in_path(&exe).ok_or(anyhow!("command {:?} not in PATH", &exe))?;

        let mut envs: Vec<(OsString, OsString)> = app_config.env.clone().into_iter().collect();

        let mut child_cmd = Command::new(exe_path);
        child_cmd.args(args);

        if let Some(path) = std::env::var_os("PATH") {
            envs.push(("PATH".into(), path));
        }

        let uid = getuid();
        let gid = getgid();

        let intern_run_path: OsString = format!("/run/user/{}", uid.as_raw()).try_into().unwrap();
        envs.push(("XDG_RUNTIME_DIR".into(), intern_run_path.clone()));

        let extern_run_path = std::env::temp_dir().join(format!(
            "mm.{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 16),
        ));
        std::fs::create_dir_all(&extern_run_path)?;

        let intern_home_path: OsString = std::env::var_os("HOME").unwrap_or("/home/mm".into());
        envs.push(("HOME".into(), intern_home_path.clone()));

        debug!(home_mode = ?app_config.home_isolation_mode, "using home mode");
        let (extern_home_path, clear_home) = match app_config.home_isolation_mode {
            config::HomeIsolationMode::Unisolated => (None, false),
            config::HomeIsolationMode::Tmpfs => (None, true),
            config::HomeIsolationMode::Permanent(path) => {
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

    pub fn set_env<K, V>(&mut self, key: K, val: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.envs
            .push((key.as_ref().to_owned(), val.as_ref().to_owned()))
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

        // TODO
        self.child_cmd.env_clear().envs(self.envs.clone());
        debug!(cmd = ?self.child_cmd, "spawning child process");

        let eventfd = rustix::event::eventfd(0, EventfdFlags::empty()).context("eventfd")?;

        // clone off a child process, which does some setup before execing the
        // app.
        let child_stderr = self.tmp_stderr.take();
        let child_pid = match unsafe { args.call().context("clone3")? } {
            0 => unsafe { self.child_after_fork(child_stderr, eventfd, &mut mounts) },
            pid => pid,
        };

        set_uid_map(child_pid, self.uid, self.gid).context("failed to set uid/gid map")?;

        // Unfreeze the child.
        write(eventfd, &1_u64.to_ne_bytes())?;

        let child_pidfd = unsafe { OwnedFd::from_raw_fd(child_pidfd) };
        Ok(super::ChildHandle {
            pid: Pid::from_raw(child_pid).unwrap(),
            pidfd: child_pidfd,
            run_path: self.extern_run_path,
        })
    }

    // signal safety dictates what we can do here, and it's not a lot.
    unsafe fn child_after_fork(
        mut self,
        stderr: Option<OwnedFd>,
        eventfd: OwnedFd,
        bind_mounts: &mut [(PathBuf, PathBuf, bool, Option<OwnedFd>)],
    ) -> ! {
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

        // Wait for mmserver to tell us it's go time.
        let mut buf = [0; 8];
        loop {
            match read(&eventfd, &mut buf) {
                Ok(_) => break,
                Err(Errno::AGAIN) | Err(Errno::INTR) => continue,
                Err(_) => std::process::abort(),
            }
        }

        preexec_debug!("starting container setup");

        // Mount /proc first.
        must!(mount_fs(
            "proc",
            "/proc",
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
            "tmpfs",
            "/dev",
            MountAttrFlags::MOUNT_ATTR_NOEXEC | MountAttrFlags::MOUNT_ATTR_STRICTATIME,
            &[("mode", "0755")],
        ));

        must!(mount_fs(
            "tmpfs",
            "/dev/shm",
            MountAttrFlags::MOUNT_ATTR_NOEXEC
                | MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NODEV,
            &[("mode", "1777"), ("size", "512m")],
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
            "devpts",
            "/dev/pts",
            MountAttrFlags::MOUNT_ATTR_NOEXEC | MountAttrFlags::MOUNT_ATTR_NOSUID,
            &[
                ("newinstance", ""),
                ("ptmxmode", "0666"),
                ("mode", "0620"),
                // TODO: do we need to add a tty group?
                // ("gid", "5"),
            ],
        ));

        must!(mount_fs(
            "tmpfs",
            "/run",
            MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NODEV
                | MountAttrFlags::MOUNT_ATTR_RELATIME,
            &[("mode", "0700"), ("size", "1g")],
        ));

        must!(mount_fs(
            "tmpfs",
            "/tmp",
            MountAttrFlags::MOUNT_ATTR_NOSUID
                | MountAttrFlags::MOUNT_ATTR_NOEXEC
                | MountAttrFlags::MOUNT_ATTR_NOATIME,
            &[("mode", "0777"), ("size", "1g")],
        ));

        if self.clear_home {
            must!(mount_fs(
                "tmpfs",
                "/home",
                MountAttrFlags::MOUNT_ATTR_NOSUID
                    | MountAttrFlags::MOUNT_ATTR_NOEXEC
                    | MountAttrFlags::MOUNT_ATTR_NOATIME,
                &[("mode", "0777"), ("size", "1g")],
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

        must!(mkdirat(
            AT_FDCWD,
            "/run/user",
            Mode::from_bits(0o700).unwrap()
        ));
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
                "bind-mounting {} (ext) to {}",
                _src_path.display(),
                dst_path.display()
            );

            let detached_mount_fd = mount_fd.take().unwrap();

            if *is_dir {
                let _ = mkdirat(AT_FDCWD, &*dst_path, Mode::empty());
            } else {
                must!(touch(&*dst_path));
            }

            must!(reattach_mount(detached_mount_fd, dst_path));
        }

        // Symlink /dev/fd -> /proc/self/fd, etc.
        must!(symlinkat("/proc/self/fd", AT_FDCWD, "/dev/fd"));
        must!(symlinkat("/proc/self/fd/0", AT_FDCWD, "/dev/stdin"));
        must!(symlinkat("/proc/self/fd/1", AT_FDCWD, "/dev/stdout"));
        must!(symlinkat("/proc/self/fd2", AT_FDCWD, "/dev/stderr"));

        // Finally, internal bind mounts.
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
                must!(touch(dst_path));
            }

            must!(reattach_mount(fd, dst_path));
        }

        // TODO: Install seccomp handlers here.

        // We don't trust std::os::Command's env handling, because sometimes
        // it allocates.
        // libc::clearenv();
        // for (k, v) in &self.envs {
        //     libc::setenv
        // }

        // If successful, this never returns.
        let e = self.child_cmd.exec();

        preexec_debug!("execve failed: {e}");
        std::process::abort();
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

unsafe fn touch(path: impl AsRef<Path>) -> rustix::io::Result<()> {
    let _ = openat(
        AT_FDCWD,
        path.as_ref(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC,
        Mode::empty(),
    )?;

    Ok(())
}

unsafe fn detach_mount(path: impl AsRef<Path>) -> rustix::io::Result<OwnedFd> {
    preexec_debug!("open_tree {}", path.as_ref().display());
    open_tree(
        AT_FDCWD,
        path.as_ref(),
        OpenTreeFlags::OPEN_TREE_CLONE
            | OpenTreeFlags::AT_RECURSIVE
            | OpenTreeFlags::OPEN_TREE_CLOEXEC,
    )
}

unsafe fn reattach_mount(fd: OwnedFd, path: impl AsRef<Path>) -> rustix::io::Result<()> {
    move_mount(
        fd.as_fd(),
        "",
        AT_FDCWD,
        path.as_ref(),
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
}

unsafe fn mount_fs(
    fstype: &str,
    dst: &str,
    options: MountAttrFlags,
    configs: &[(&str, &str)],
) -> rustix::io::Result<()> {
    preexec_debug!("mounting {fstype} on {dst}");

    let fsfd = fsopen(fstype, FsOpenFlags::FSOPEN_CLOEXEC)?;

    for (k, v) in configs {
        if v.is_empty() {
            fsconfig_set_flag(fsfd.as_fd(), *k)?;
        } else {
            fsconfig_set_string(fsfd.as_fd(), *k, *v)?;
        }
    }

    fsconfig_create(fsfd.as_fd())?;
    let tmpfs_fd = fsmount(fsfd.as_fd(), FsMountFlags::FSMOUNT_CLOEXEC, options)?;

    let _ = mkdirat(AT_FDCWD, dst, Mode::empty());
    move_mount(
        tmpfs_fd.as_fd(),
        c"",
        AT_FDCWD,
        dst,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    )?;

    Ok(())
}
