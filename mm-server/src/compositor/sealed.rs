// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    ffi::CStr,
    fs::File,
    io::{Seek as _, SeekFrom, Write as _},
    os::fd::{AsFd, AsRawFd, BorrowedFd},
};

use nix::{
    fcntl::{fcntl, FcntlArg, SealFlag},
    sys::memfd::{memfd_create, MemFdCreateFlag},
};

pub struct SealedFile {
    file: File,
    size: usize,
}

impl SealedFile {
    pub fn new(name: impl AsRef<CStr>, contents: &[u8]) -> anyhow::Result<Self> {
        let fd = memfd_create(
            name.as_ref(),
            MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING,
        )?;

        let mut file: File = fd.into();
        file.write_all(contents)?;
        file.flush()?;
        file.seek(SeekFrom::Start(0))?;

        fcntl(
            file.as_raw_fd(),
            FcntlArg::F_ADD_SEALS(
                SealFlag::F_SEAL_SEAL
                    | SealFlag::F_SEAL_WRITE
                    | SealFlag::F_SEAL_SHRINK
                    | SealFlag::F_SEAL_GROW,
            ),
        )?;

        Ok(Self {
            file,
            size: contents.len(),
        })
    }

    pub fn size(&self) -> usize {
        self.size
    }
}

impl AsRawFd for SealedFile {
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        self.file.as_raw_fd()
    }
}

impl AsFd for SealedFile {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}
