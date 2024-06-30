use wayland_server::{
    protocol::{wl_keyboard, wl_pointer, wl_seat, wl_touch},
    Resource as _,
};

use crate::compositor::State;

impl wayland_server::GlobalDispatch<wl_seat::WlSeat, ()> for State {
    fn bind(
        _state: &mut Self,
        _handle: &wayland_server::DisplayHandle,
        _client: &wayland_server::Client,
        resource: wayland_server::New<wl_seat::WlSeat>,
        _global_data: &(),
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        let wl_seat = data_init.init(resource, ());
        wl_seat.capabilities(wl_seat::Capability::Keyboard | wl_seat::Capability::Pointer);
    }
}

impl wayland_server::Dispatch<wl_seat::WlSeat, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        resource: &wl_seat::WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
        match request {
            wl_seat::Request::GetPointer { id } => {
                let wl_pointer = data_init.init(id, ());
                state.default_seat.get_pointer(wl_pointer);
            }
            wl_seat::Request::GetKeyboard { id } => {
                let wl_keyboard = data_init.init(id, ());

                // We disable client-side key repeat handling, and instead
                // simulate it.
                wl_keyboard.repeat_info(0, i32::MAX);

                state.default_seat.get_keyboard(wl_keyboard);
            }
            wl_seat::Request::GetTouch { id } => {
                resource.post_error(
                    wl_seat::Error::MissingCapability,
                    "No touch capability advertized.",
                );
            }
            _ => (),
        }
    }
}

impl wayland_server::Dispatch<wl_pointer::WlPointer, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_pointer::WlPointer,
        request: wl_pointer::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_pointer::WlPointer,
        _data: &(),
    ) {
        state.default_seat.destroy_pointer(resource.clone());
    }
}

impl wayland_server::Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn request(
        state: &mut Self,
        _client: &wayland_server::Client,
        _resource: &wl_keyboard::WlKeyboard,
        request: wl_keyboard::Request,
        _data: &(),
        _dhandle: &wayland_server::DisplayHandle,
        data_init: &mut wayland_server::DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_keyboard::WlKeyboard,
        _data: &(),
    ) {
        state.default_seat.destroy_keyboard(resource.clone());
    }
}
