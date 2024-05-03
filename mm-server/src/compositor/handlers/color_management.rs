// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]

use std::sync::{Arc, Mutex};

use smithay::{
    reexports::wayland_server::{
        backend::GlobalId, protocol::wl_surface, Client, DataInit, Dispatch, DisplayHandle,
        GlobalDispatch, New, Resource, WEnum,
    },
    wayland::compositor,
};

use crate::color::{ColorSpace, Primaries, TransferFunction};

use self::protocol::{
    mesa_color_management_surface_v1::MesaColorManagementSurfaceV1,
    mesa_color_manager_v1::{self, Feature, MesaColorManagerV1, RenderIntent},
    mesa_image_description_creator_params_v1::MesaImageDescriptionCreatorParamsV1,
    mesa_image_description_v1::{Cause, MesaImageDescriptionV1},
};

impl From<TransferFunction> for mesa_color_manager_v1::TransferFunction {
    fn from(tf: TransferFunction) -> Self {
        match tf {
            TransferFunction::Linear => mesa_color_manager_v1::TransferFunction::Linear,
            TransferFunction::Srgb => mesa_color_manager_v1::TransferFunction::Srgb,
            TransferFunction::Pq => mesa_color_manager_v1::TransferFunction::St2084Pq,
        }
    }
}

impl TryFrom<mesa_color_manager_v1::TransferFunction> for TransferFunction {
    type Error = ();

    fn try_from(tf: mesa_color_manager_v1::TransferFunction) -> Result<Self, Self::Error> {
        match tf {
            mesa_color_manager_v1::TransferFunction::Linear => Ok(TransferFunction::Linear),
            mesa_color_manager_v1::TransferFunction::Srgb => Ok(TransferFunction::Srgb),
            mesa_color_manager_v1::TransferFunction::St2084Pq => Ok(TransferFunction::Pq),
            _ => Err(()),
        }
    }
}

impl From<Primaries> for mesa_color_manager_v1::Primaries {
    fn from(p: Primaries) -> Self {
        match p {
            Primaries::Srgb => mesa_color_manager_v1::Primaries::Srgb,
            Primaries::Bt2020 => mesa_color_manager_v1::Primaries::Bt2020,
        }
    }
}

impl TryFrom<mesa_color_manager_v1::Primaries> for Primaries {
    type Error = ();

    fn try_from(p: mesa_color_manager_v1::Primaries) -> Result<Self, Self::Error> {
        match p {
            mesa_color_manager_v1::Primaries::Srgb => Ok(Primaries::Srgb),
            mesa_color_manager_v1::Primaries::Bt2020 => Ok(Primaries::Bt2020),
            _ => Err(()),
        }
    }
}

use super::State;

mod protocol {
    use smithay::reexports::wayland_server;
    use smithay::reexports::wayland_server::protocol::*;

    mod __interfaces {
        use smithay::reexports::wayland_server;
        use wayland_server::backend as wayland_backend;
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "src/compositor/protocol/mesa-color-management-v1.xml"
        );
    }

    use __interfaces::*;
    wayland_scanner::generate_server_code!("src/compositor/protocol/mesa-color-management-v1.xml");
}

const SUPPORTED_TFS: [mesa_color_manager_v1::TransferFunction; 3] = [
    mesa_color_manager_v1::TransferFunction::Linear,
    mesa_color_manager_v1::TransferFunction::Srgb,
    mesa_color_manager_v1::TransferFunction::St2084Pq,
];

const SUPPORTED_PRIMARIES: [mesa_color_manager_v1::Primaries; 2] = [
    mesa_color_manager_v1::Primaries::Srgb,
    mesa_color_manager_v1::Primaries::Bt2020,
];

pub struct ColorManagementGlobal {
    _global: GlobalId,
}

impl ColorManagementGlobal {
    pub fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<State, MesaColorManagerV1, _>(1, ());
        Self { _global: global }
    }
}

struct ColorManagementSurfaceUserData {
    wl_surface: wl_surface::WlSurface,
}

/// Represents a pending image description for a color-managed surface.
#[derive(Debug, Default, Clone)]
pub struct ColorManagementCachedState {
    pub colorspace: Option<ColorSpace>,
}

impl compositor::Cacheable for ColorManagementCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

#[derive(Debug, Default)]
struct ImageDescParams {
    primaries: Option<mesa_color_manager_v1::Primaries>,
    transfer_function: Option<mesa_color_manager_v1::TransferFunction>,
}

impl GlobalDispatch<MesaColorManagerV1, ()> for State {
    fn bind(
        _state: &mut State,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<MesaColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        let color_manager = data_init.init(resource, ());

        color_manager.supported_intent(RenderIntent::Perceptual);
        color_manager.supported_feature(Feature::Parametric);

        for tf in SUPPORTED_TFS.iter().copied() {
            color_manager.supported_tf_named(tf);
        }

        for primaries in SUPPORTED_PRIMARIES.iter().copied() {
            color_manager.supported_primaries_named(primaries);
        }
    }
}

impl Dispatch<MesaColorManagerV1, ()> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        _resource: &MesaColorManagerV1,
        request: <MesaColorManagerV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            protocol::mesa_color_manager_v1::Request::GetOutput { .. } => {
                todo!()
            }
            protocol::mesa_color_manager_v1::Request::GetSurface { id, surface } => {
                data_init.init(
                    id,
                    ColorManagementSurfaceUserData {
                        wl_surface: surface,
                    },
                );
            }
            protocol::mesa_color_manager_v1::Request::NewIccCreator { obj } => {
                data_init.post_error(
                    obj,
                    protocol::mesa_color_manager_v1::Error::UnsupportedFeature,
                    "ICC profiles are not supported.",
                );
            }
            protocol::mesa_color_manager_v1::Request::NewParametricCreator { obj } => {
                data_init.init(obj, Arc::new(Mutex::new(Default::default())));
            }
            protocol::mesa_color_manager_v1::Request::Destroy => (),
        }
    }
}

impl Dispatch<MesaColorManagementSurfaceV1, ColorManagementSurfaceUserData> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        resource: &MesaColorManagementSurfaceV1,
        request: <MesaColorManagementSurfaceV1 as Resource>::Request,
        data: &ColorManagementSurfaceUserData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            protocol::mesa_color_management_surface_v1::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                if render_intent != WEnum::Value(RenderIntent::Perceptual) {
                    resource.post_error(
                        protocol::mesa_color_management_surface_v1::Error::RenderIntent as u32,
                        "Only perceptual render intent is supported",
                    );

                    return;
                }

                if let Some(colorspace) = image_description.data::<ColorSpace>() {
                    compositor::with_states(&data.wl_surface, |states| {
                        states
                            .cached_state
                            .pending::<ColorManagementCachedState>()
                            .colorspace = Some(*colorspace);
                    });
                }
            }
            protocol::mesa_color_management_surface_v1::Request::UnsetImageDescription => {
                compositor::with_states(&data.wl_surface, |states| {
                    states
                        .cached_state
                        .pending::<ColorManagementCachedState>()
                        .colorspace = None;
                });
            }
            protocol::mesa_color_management_surface_v1::Request::GetPreferred { .. } => todo!(),
            protocol::mesa_color_management_surface_v1::Request::Destroy => {
                // XXX: Why does the protocol allow multiple?
                compositor::with_states(&data.wl_surface, |states| {
                    states
                        .cached_state
                        .pending::<ColorManagementCachedState>()
                        .colorspace = None;
                });
            }
        }
    }
}

impl Dispatch<MesaImageDescriptionCreatorParamsV1, Arc<Mutex<ImageDescParams>>> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        resource: &MesaImageDescriptionCreatorParamsV1,
        request: <MesaImageDescriptionCreatorParamsV1 as Resource>::Request,
        data: &Arc<Mutex<ImageDescParams>>,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            protocol::mesa_image_description_creator_params_v1::Request::SetTfNamed { tf } => {
                match tf {
                    WEnum::Value(tf) if SUPPORTED_TFS.contains(&tf) => {
                        data.lock().unwrap().transfer_function = Some(tf);
                    }
                    _ => {
                        resource.post_error(
                            protocol::mesa_image_description_creator_params_v1::Error::InvalidTf as u32,
                            "Unsupported transfer function",
                        );
                    }
                }
            }
            protocol::mesa_image_description_creator_params_v1::Request::SetPrimariesNamed { primaries } => {
                match primaries {
                    WEnum::Value(p) if SUPPORTED_PRIMARIES.contains(&p) => {
                        data.lock().unwrap().primaries = Some(p);
                    },
                    _ => {
                        resource.post_error(
                            protocol::mesa_image_description_creator_params_v1::Error::InvalidPrimaries as u32,
                            "Unsupported primaries",
                        );
                    }
                }
            }
            protocol::mesa_image_description_creator_params_v1::Request::Create { image_description } => {
                let params = data.lock().unwrap();
                if params.transfer_function.is_none() || params.primaries.is_none() {
                    data_init.post_error(
                        image_description,
                        protocol::mesa_image_description_creator_params_v1::Error::IncompleteSet as u32,
                        "Primaries and transfer function must be set",
                    );

                    return;
                }

                let colorspace = match (params.primaries.unwrap().try_into(), params.transfer_function.unwrap().try_into()) {
                    (Ok(primaries), Ok(transfer_function)) => ColorSpace::from_primaries_and_tf(primaries, transfer_function),
                    _ => None,
                };

                if let Some(colorspace) = colorspace {
                    let image_description = data_init.init(image_description, colorspace);
                    image_description.ready(colorspace as u32);
                } else {
                    // We init and then immediately fail.
                    let image_description = data_init.init(image_description, ColorSpace::Srgb);
                    image_description.failed(Cause::Unsupported, "Unsupported combination of transfer function and primaries".to_string());
                }
            }
            protocol::mesa_image_description_creator_params_v1::Request::SetTfPower { .. } => resource.post_error(
                protocol::mesa_image_description_creator_params_v1::Error::InvalidTf as u32,
                "set_tf_power not supported",
            ),
            protocol::mesa_image_description_creator_params_v1::Request::SetPrimaries { .. } => resource.post_error(
                    protocol::mesa_image_description_creator_params_v1::Error::InvalidPrimaries as u32,
                    "set_primaries not supported",
                ),
            protocol::mesa_image_description_creator_params_v1::Request::SetMasteringDisplayPrimaries {.. } => resource.post_error(
                    protocol::mesa_image_description_creator_params_v1::Error::InvalidMastering as u32,
                    "set_mastering_display_primaries not supported",
                ),
            protocol::mesa_image_description_creator_params_v1::Request::SetMasteringLuminance { .. } => resource.post_error(
                    protocol::mesa_image_description_creator_params_v1::Error::InvalidLuminance as u32,
                    "set_mastering_luminance not supported",
                ),
            protocol::mesa_image_description_creator_params_v1::Request::SetMaxCll { .. } => resource.post_error(
                    protocol::mesa_image_description_creator_params_v1::Error::InconsistentSet as u32,
                    "set_max_cll not supported",
                ),
            protocol::mesa_image_description_creator_params_v1::Request::SetMaxFall { .. } => resource.post_error(
                    protocol::mesa_image_description_creator_params_v1::Error::InconsistentSet as u32,
                    "set_max_fall not supported",
                ),
        }
    }
}

impl Dispatch<MesaImageDescriptionV1, ColorSpace> for State {
    fn request(
        _state: &mut State,
        _client: &Client,
        resource: &MesaImageDescriptionV1,
        request: <MesaImageDescriptionV1 as Resource>::Request,
        _data: &ColorSpace,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            protocol::mesa_image_description_v1::Request::GetInformation { .. } => resource
                .post_error(
                    protocol::mesa_image_description_v1::Error::NoInformation as u32,
                    "No information available",
                ),
            protocol::mesa_image_description_v1::Request::Destroy => (),
        }
    }
}
