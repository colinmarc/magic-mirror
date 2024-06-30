// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    os::fd::OwnedFd,
    sync::{Arc, RwLock},
};

use drm_fourcc::DrmFourcc;
use wayland_protocols::wp::linux_dmabuf::zv1::server::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1,
};
use wayland_server::{protocol::wl_buffer, Resource as _, WEnum};

use crate::compositor::{
    buffers::{fourcc_bpp, import_dmabuf_buffer, validate_buffer_parameters, PlaneMetadata},
    State,
};

use super::make_u64;

impl wayland_server::GlobalDispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl wayland_server::Dispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
        request: zwp_linux_dmabuf_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            zwp_linux_dmabuf_v1::Request::CreateParams { params_id } => {
                data_init.init(params_id, Arc::new(RwLock::new(Params::Empty)));
            }
            zwp_linux_dmabuf_v1::Request::GetDefaultFeedback { id } => {
                let feedback = data_init.init(id, ());
                state.emit_dmabuf_feedback(&feedback);
            }
            zwp_linux_dmabuf_v1::Request::GetSurfaceFeedback { id, .. } => {
                let feedback = data_init.init(id, ());
                state.emit_dmabuf_feedback(&feedback);
            }
            zwp_linux_dmabuf_v1::Request::Destroy => (),
            _ => (),
        }
    }
}

#[derive(Debug)]
enum Params {
    Empty,
    Config {
        fd: OwnedFd,
        offset: u32,
        stride: u32,
        modifier: u64,
    },
    Done,
}

impl
    wayland_server::Dispatch<
        zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        Arc<RwLock<Params>>,
    > for State
{
    fn request(
        state: &mut Self,
        client: &wayland_server::Client,
        resource: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        request: zwp_linux_buffer_params_v1::Request,
        data: &Arc<RwLock<Params>>,
        dh: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            zwp_linux_buffer_params_v1::Request::Add {
                fd,
                plane_idx,
                offset,
                stride,
                modifier_hi,
                modifier_lo,
            } => {
                if plane_idx > 0 {
                    resource.post_error(
                        zwp_linux_buffer_params_v1::Error::PlaneIdx,
                        "Multiplane images are not supported.",
                    );
                    return;
                }

                let mut params = data.write().unwrap();
                if matches!(*params, Params::Config { .. } | Params::Done) {
                    resource.post_error(
                        zwp_linux_buffer_params_v1::Error::PlaneSet,
                        "Plane 0 already configured.",
                    );
                    return;
                }

                let modifier = make_u64(modifier_hi, modifier_lo);

                if resource.version() >= 4 && !state.cached_dmabuf_feedback.contains(modifier) {
                    resource.post_error(
                        zwp_linux_buffer_params_v1::Error::InvalidFormat,
                        "Unsupported format.",
                    );
                }

                *params = Params::Config {
                    fd,
                    offset,
                    stride,
                    modifier,
                };
            }
            zwp_linux_buffer_params_v1::Request::Create {
                width,
                height,
                format,
                flags,
            } => {
                let mut params = data.write().unwrap();
                let format = match validate_create(&params, width, height, format, flags) {
                    Ok(f) => f,
                    Err((e, s)) => {
                        resource.post_error(e, s);
                        return;
                    }
                };

                let Params::Config { fd, modifier, .. } =
                    std::mem::replace(&mut *params, Params::Done)
                else {
                    unreachable!();
                };

                let res = state.buffers.try_insert_with_key(|k| {
                    let wl_buffer =
                        client.create_resource::<wl_buffer::WlBuffer, _, State>(dh, 1, k)?;

                    import_dmabuf_buffer(state.vk.clone(), wl_buffer, format, modifier.into(), fd)
                });

                if res.is_err() {
                    resource.failed();
                };
            }
            zwp_linux_buffer_params_v1::Request::CreateImmed {
                buffer_id,
                width,
                height,
                format,
                flags,
            } => {
                let mut params = data.write().unwrap();
                let format = match validate_create(&params, width, height, format, flags) {
                    Ok(f) => f,
                    Err((e, s)) => {
                        resource.post_error(e, s);
                        return;
                    }
                };

                let Params::Config { fd, modifier, .. } =
                    std::mem::replace(&mut *params, Params::Done)
                else {
                    unreachable!();
                };

                let res = state.buffers.try_insert_with_key(|k| {
                    let wl_buffer = data_init.init(buffer_id, k);
                    import_dmabuf_buffer(state.vk.clone(), wl_buffer, format, modifier.into(), fd)
                });

                if res.is_err() {
                    resource.post_error(
                        zwp_linux_buffer_params_v1::Error::InvalidWlBuffer,
                        "Import failed.",
                    );
                };
            }
            zwp_linux_buffer_params_v1::Request::Destroy => (),
            _ => (),
        }
    }
}

fn validate_create(
    params: &Params,
    width: i32,
    height: i32,
    format: u32,
    flags: WEnum<zwp_linux_buffer_params_v1::Flags>,
) -> Result<PlaneMetadata, (zwp_linux_buffer_params_v1::Error, String)> {
    if !flags
        .into_result()
        .map(|f| f.is_empty())
        .unwrap_or_default()
    {
        return Err((
            zwp_linux_buffer_params_v1::Error::InvalidFormat,
            "Invalid flags.".to_string(),
        ));
    }

    match *params {
        Params::Empty => {
            return Err((
                zwp_linux_buffer_params_v1::Error::Incomplete,
                "Plane 0 not configured.".to_string(),
            ))
        }
        Params::Done => {
            return Err((
                zwp_linux_buffer_params_v1::Error::AlreadyUsed,
                "Params already consumed.".to_string(),
            ))
        }
        _ => (),
    }

    let format = match DrmFourcc::try_from(format) {
        Ok(format) => format,
        Err(_) => {
            return Err((
                zwp_linux_buffer_params_v1::Error::InvalidFormat,
                "Unknown format.".to_string(),
            ))
        }
    };

    let Some(bpp) = fourcc_bpp(format) else {
        return Err((
            zwp_linux_buffer_params_v1::Error::InvalidFormat,
            "Unsupported format.".to_string(),
        ));
    };

    let Params::Config { offset, stride, .. } = params else {
        unreachable!()
    };

    if let Err(s) = validate_buffer_parameters(*offset as i32, width, height, *stride as i32, bpp) {
        return Err((zwp_linux_buffer_params_v1::Error::InvalidDimensions, s));
    }

    Ok(PlaneMetadata {
        format,
        width: width as u32,
        height: height as u32,
        stride: *stride,
        offset: *offset,
    })
}

impl wayland_server::Dispatch<zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1, ()>
    for State
{
    fn request(
        _state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        _request: zwp_linux_dmabuf_feedback_v1::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }
}
