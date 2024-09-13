// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use crate::compositor::{protocols::wl_drm, State};

impl wayland_server::GlobalDispatch<wl_drm::WlDrm, ()> for State {
    fn bind(
        state: &mut State,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_drm::WlDrm>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, State>,
    ) {
        let wl_drm = data_init.init(resource, ());
        wl_drm.device(
            dev_path(state.vk.device_info.drm_node).expect("failed to determine device node"),
        );
    }
}

impl wayland_server::Dispatch<wl_drm::WlDrm, ()> for State {
    fn request(
        _state: &mut State,
        _client: &wayland_server::Client,
        _resource: &wl_drm::WlDrm,
        _request: wl_drm::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, State>,
    ) {
    }
}

pub fn dev_path(dev: libc::dev_t) -> std::io::Result<String> {
    let (major, minor) = unsafe { (libc::major(dev), libc::minor(dev)) };

    assert_eq!(major, 226, "not a DRM device");
    assert!(minor >= 128, "not a render node");

    for f in std::fs::read_dir(format!("/sys/dev/char/{}:{}/device/drm", major, minor))?.flatten() {
        let name = f.file_name();
        let name = name.to_string_lossy();

        if name.starts_with("renderD") {
            let path = format!("/dev/dri/{}", name);
            std::fs::metadata(&path)?;
            return Ok(path);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no render node found",
    ))
}
