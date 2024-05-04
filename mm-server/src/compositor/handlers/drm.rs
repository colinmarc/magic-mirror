// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use nix::libc;
use smithay::reexports::wayland_server;

use crate::vulkan::VkContext;

use super::State;

#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
mod protocol {
    use smithay::reexports::wayland_server;
    use smithay::reexports::wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server;
        use smithay::reexports::wayland_server::protocol::__interfaces::*;
        use wayland_server::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("src/compositor/protocol/wayland-drm.xml");
    }

    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("src/compositor/protocol/wayland-drm.xml");
}

use protocol::wl_drm;

pub struct DrmState {
    device_path: String,
}

impl DrmState {
    pub fn new(dh: &wayland_server::DisplayHandle, vk: Arc<VkContext>) -> anyhow::Result<Self> {
        let _global = dh.create_global::<State, wl_drm::WlDrm, ()>(2, ());

        let device_path = dev_path(vk.device_info.drm_node)?;
        Ok(Self { device_path })
    }
}

impl wayland_server::GlobalDispatch<wl_drm::WlDrm, (), State> for DrmState {
    fn bind(
        state: &mut State,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_drm::WlDrm>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, State>,
    ) {
        let wl_drm = data_init.init(resource, ());
        wl_drm.device(state.drm_state.device_path.clone());
    }
}

impl wayland_server::Dispatch<wl_drm::WlDrm, (), State> for DrmState {
    fn request(
        _state: &mut State,
        _client: &wayland_server::Client,
        _resource: &wl_drm::WlDrm,
        _request: wl_drm::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, State>,
    ) {
        // no-op
    }
}

impl AsMut<DrmState> for State {
    fn as_mut(&mut self) -> &mut DrmState {
        &mut self.drm_state
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

wayland_server::delegate_dispatch!(State: [wl_drm::WlDrm: ()] => DrmState);
wayland_server::delegate_global_dispatch!(State: [wl_drm::WlDrm: ()] => DrmState);
