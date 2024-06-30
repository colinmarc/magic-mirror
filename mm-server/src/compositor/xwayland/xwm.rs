// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    collections::HashMap,
    os::fd::{AsFd as _, BorrowedFd},
};

use hashbrown::HashSet;
use tracing::{debug, trace};
use x11rb::{
    connection::Connection as _,
    cookie::VoidCookie,
    protocol::{
        self,
        composite::ConnectionExt as _,
        xproto::{self, ConnectionExt as _},
    },
    rust_connection::{ConnectionError, DefaultStream, RustConnection as X11Connection},
    wrapper::ConnectionExt as _,
};

use crate::{
    compositor::{
        surface::{self, SurfaceConfiguration},
        State,
    },
    pixel_scale::PixelScale,
};

x11rb::atom_manager! {
    /// Atoms used by the XWM and X11Surface types
    pub Atoms:
    AtomsCookie {
        WL_SURFACE_SERIAL,

        UTF8_STRING,

        WM_HINTS,
        WM_PROTOCOLS,
        WM_TAKE_FOCUS,
        WM_CHANGE_STATE,
        _NET_WM_NAME,
        _NET_WM_MOVERESIZE,
        _NET_WM_STATE_MODAL,

        WM_S0,
        WM_STATE,
        _NET_WM_CM_S0,
        _NET_SUPPORTED,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_FOCUSED,
        _NET_SUPPORTING_WM_CHECK,
    }
}

pub struct XWindow {
    pub id: u32,

    pub serial: Option<u64>,
    pub title: Option<String>,
    pub app_id: Option<String>,

    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,

    pub states: HashSet<xproto::Atom>,

    pub protocols: HashSet<xproto::Atom>,

    pub hint_input: bool,
    pub override_redirect: bool,
    pub mapped: bool, // Whether MapRequest/MapNotify has been recieved.
}

impl std::fmt::Debug for XWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut title = self.title.as_deref().unwrap_or("Untitled");
        if title.is_empty() {
            title = "Untitled";
        }

        let serial = if let Some(s) = self.serial {
            format!(" serial={}", s)
        } else {
            "".to_string()
        };

        let override_redirect = if self.override_redirect { " [OR]" } else { "" };

        write!(
            f,
            "<{} \"{}\"{}{}>",
            self.id, title, serial, override_redirect
        )?;
        Ok(())
    }
}

pub struct Xwm {
    conn: X11Connection,
    atoms: Atoms,
    wm_id: u32,

    screen: xproto::Screen,
    client_list: Vec<u32>,
    client_list_stacking: Vec<u32>,

    pub xwindows: HashMap<u32, XWindow>,
    pub serials: HashMap<u64, u32>,
}

impl Xwm {
    pub fn new(x11_socket: mio::net::UnixStream) -> anyhow::Result<Self> {
        let stream = DefaultStream::from_unix_stream(x11_socket.into())?.0;
        let conn = X11Connection::connect_to_stream(stream, 0)?;
        let atoms = Atoms::new(&conn)?.reply()?;

        let screen = conn.setup().roots[0].clone();

        {
            let font = xproto::FontWrapper::open_font(&conn, "cursor".as_bytes())?;
            let cursor = xproto::CursorWrapper::create_glyph_cursor(
                &conn,
                font.font(),
                font.font(),
                68,
                69,
                0,
                0,
                0,
                u16::MAX,
                u16::MAX,
                u16::MAX,
            )?;

            conn.change_window_attributes(
                screen.root,
                &xproto::ChangeWindowAttributesAux::default()
                    .event_mask(
                        xproto::EventMask::SUBSTRUCTURE_REDIRECT
                            | xproto::EventMask::SUBSTRUCTURE_NOTIFY
                            | xproto::EventMask::PROPERTY_CHANGE,
                        // | xproto::EventMask::FOCUS_CHANGE,
                    )
                    .cursor(cursor.cursor()),
            )?;
        }

        let wm_id = conn.generate_id()?;
        conn.create_window(
            screen.root_depth,
            wm_id,
            screen.root,
            0,
            0,
            10,
            10,
            0,
            xproto::WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &Default::default(),
        )?;

        conn.set_selection_owner(wm_id, atoms.WM_S0, x11rb::CURRENT_TIME)?;
        conn.set_selection_owner(wm_id, atoms._NET_WM_CM_S0, x11rb::CURRENT_TIME)?;
        conn.composite_redirect_subwindows(screen.root, protocol::composite::Redirect::MANUAL)?;

        conn.change_property32(
            xproto::PropMode::REPLACE,
            screen.root,
            atoms._NET_SUPPORTED,
            xproto::AtomEnum::ATOM,
            &[
                atoms._NET_WM_STATE,
                atoms._NET_WM_STATE_MAXIMIZED_HORZ,
                atoms._NET_WM_STATE_MAXIMIZED_VERT,
                atoms._NET_WM_STATE_HIDDEN,
                atoms._NET_WM_STATE_FULLSCREEN,
                atoms._NET_WM_STATE_MODAL,
                atoms._NET_WM_STATE_FOCUSED,
                atoms._NET_ACTIVE_WINDOW,
                atoms._NET_WM_MOVERESIZE,
                atoms._NET_CLIENT_LIST,
                atoms._NET_CLIENT_LIST_STACKING,
            ],
        )?;

        replace_window_list(&conn, screen.root, atoms._NET_ACTIVE_WINDOW, [0])?;
        replace_window_list(&conn, screen.root, atoms._NET_SUPPORTING_WM_CHECK, [wm_id])?;
        replace_window_list(&conn, wm_id, atoms._NET_SUPPORTING_WM_CHECK, [wm_id])?;

        conn.change_property8(
            xproto::PropMode::REPLACE,
            wm_id,
            atoms._NET_WM_NAME,
            atoms.UTF8_STRING,
            "Magic Mirror XWM".as_bytes(),
        )?;

        conn.flush()?;

        Ok(Self {
            conn,
            atoms,
            wm_id,

            screen,
            client_list: Vec::new(),
            client_list_stacking: Vec::new(),

            xwindows: HashMap::new(),
            serials: HashMap::new(),
        })
    }

    pub fn display_fd(&self) -> BorrowedFd {
        self.conn.stream().as_fd()
    }

    pub fn xwindow_for_serial(&self, serial: u64) -> Option<&XWindow> {
        self.serials
            .get(&serial)
            .and_then(|id| self.xwindows.get(id))
    }

    pub fn configure_window(
        &mut self,
        window: u32,
        conf: SurfaceConfiguration,
    ) -> anyhow::Result<()> {
        if let Some(xwin) = self.xwindows.get_mut(&window) {
            trace!(?xwin, ?conf, "configuring xwindow");

            self.conn.configure_window(
                window,
                &xproto::ConfigureWindowAux::default()
                    .x(conf.topleft.x as i32)
                    .y(conf.topleft.y as i32)
                    .width(conf.size.x)
                    .height(conf.size.y)
                    .border_width(0)
                    .stack_mode(xproto::StackMode::ABOVE),
            )?;

            self.conn.send_event(
                false,
                window,
                xproto::EventMask::STRUCTURE_NOTIFY,
                xproto::ConfigureNotifyEvent {
                    response_type: xproto::CONFIGURE_NOTIFY_EVENT,
                    sequence: 0,
                    event: window,
                    window,
                    above_sibling: x11rb::NONE,
                    x: conf.topleft.x as i16,
                    y: conf.topleft.y as i16,
                    width: conf.size.x as u16,
                    height: conf.size.y as u16,
                    border_width: 0,
                    override_redirect: false,
                },
            )?;

            let old_states = xwin.states.clone();

            match conf.visibility {
                surface::Visibility::Occluded => {
                    xwin.states.insert(self.atoms._NET_WM_STATE_HIDDEN);
                    xwin.states.remove(&self.atoms._NET_WM_STATE_FOCUSED);
                }
                surface::Visibility::Visible => {
                    xwin.states.remove(&self.atoms._NET_WM_STATE_FOCUSED);
                    xwin.states.remove(&self.atoms._NET_WM_STATE_HIDDEN);
                }
                surface::Visibility::Active => {
                    xwin.states.remove(&self.atoms._NET_WM_STATE_HIDDEN);
                    xwin.states.insert(self.atoms._NET_WM_STATE_FOCUSED);
                }
            }

            if conf.fullscreen {
                xwin.states.insert(self.atoms._NET_WM_STATE_FULLSCREEN);
            } else {
                xwin.states.remove(&self.atoms._NET_WM_STATE_FULLSCREEN);
            }

            if xwin.states != old_states {
                let values = xwin.states.iter().copied().collect::<Vec<_>>();

                if tracing::event_enabled!(tracing::Level::TRACE) {
                    let names = values
                        .iter()
                        .copied()
                        .map(|atom| get_atom_name(&self.conn, atom))
                        .collect::<Result<Vec<_>, _>>()?;
                    trace!(?xwin, ?names, "setting states");
                }

                self.conn.change_property32(
                    xproto::PropMode::REPLACE,
                    xwin.id,
                    self.atoms._NET_WM_STATE,
                    xproto::AtomEnum::ATOM,
                    &values,
                )?;
            }

            self.conn.flush()?;
        } else {
            debug!(window, "skipping configure for dead window")
        }

        Ok(())
    }

    pub fn set_focus(&self, window: Option<u32>) -> anyhow::Result<()> {
        let Some(xwin) = window.and_then(|id| self.xwindows.get(&id)) else {
            trace!("removing input focus");
            self.conn.set_input_focus(
                xproto::InputFocus::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            )?;
            self.conn.flush()?;
            return Ok(());
        };

        replace_window_list(
            &self.conn,
            self.screen.root,
            self.atoms._NET_ACTIVE_WINDOW,
            [xwin.id],
        )?;

        // "Passive and Locally Active clients set the input field of WM_HINTS
        // to True, which indicates that they require window manager assistance
        // in acquiring the input focus."
        // TODO: for some reason this seems to cause problems, for example for
        // steam context menus, which flicker out immediately.
        if xwin.hint_input {
            trace!(?xwin, "setting input focus");
            self.conn.set_input_focus(
                xproto::InputFocus::POINTER_ROOT,
                xwin.id,
                x11rb::CURRENT_TIME,
            )?;
        }

        // "Windows with the atom WM_TAKE_FOCUS in their WM_PROTOCOLS property
        // may receive a ClientMessage event from the window manager with
        // WM_TAKE_FOCUS..."
        if xwin.protocols.contains(&self.atoms.WM_TAKE_FOCUS) {
            trace!(?xwin, "sending TAKE_FOCUS");

            let event = xproto::ClientMessageEvent::new(
                32,
                xwin.id,
                self.atoms.WM_PROTOCOLS,
                [self.atoms.WM_TAKE_FOCUS, x11rb::CURRENT_TIME, 0, 0, 0],
            );
            self.conn
                .send_event(false, xwin.id, xproto::EventMask::NO_EVENT, event)?;
        }

        self.conn.flush()?;
        Ok(())
    }
}

impl State {
    pub fn dispatch_xwm(&mut self) -> anyhow::Result<()> {
        loop {
            match self.xwm.as_mut().unwrap().conn.poll_for_event()? {
                Some(ev) => handle_event(self, ev)?,
                None => return Ok(()),
            }
        }
    }

    pub fn delayed_map_xwin(&mut self, serial: u64) {
        let Some(xwin) = self.xwm.as_ref().unwrap().xwindow_for_serial(serial) else {
            return;
        };

        let Some(surface_id) = self.xwayland_surface_lookup.get(&serial) else {
            return;
        };

        let display_params = self.effective_display_params();

        let surf = &mut self.surfaces[*surface_id];
        surf.title = xwin.title.clone();
        surf.app_id = xwin.app_id.clone();
        surf.reconfigure(display_params, Some(xwin));

        if let Some(surface::ContentUpdate { buffer, .. }) = surf.content {
            self.map_surface(*surface_id, buffer);
        }
    }
}

fn handle_event(state: &mut State, ev: protocol::Event) -> anyhow::Result<()> {
    trace!(?ev, "x11 event");
    let display_params = state.effective_display_params();
    let xwm = state.xwm.as_mut().unwrap();

    use protocol::Event::*;
    match ev {
        CreateNotify(msg) => {
            if msg.window == xwm.wm_id {
                return Ok(());
            }

            // Track property changes (such as the window title).
            xwm.conn.change_window_attributes(
                msg.window,
                &xproto::ChangeWindowAttributesAux::new()
                    .event_mask(xproto::EventMask::PROPERTY_CHANGE),
            )?;
            xwm.conn.flush()?;

            let title = fetch_string_property(&xwm.conn, msg.window, xwm.atoms._NET_WM_NAME)?;
            let app_id = fetch_class(&xwm.conn, msg.window)?;
            let hints = fetch_hints(&xwm.conn, msg.window)?;
            let protocols = fetch_protocols(&xwm.conn, xwm.atoms.WM_PROTOCOLS, msg.window)?;

            trace!(?hints, ?protocols, "fetched state");

            let xwin = XWindow {
                id: msg.window,
                serial: None,

                title,
                app_id,

                x: msg.x as u32,
                y: msg.y as u32,
                width: msg.width as u32,
                height: msg.height as u32,

                states: HashSet::new(),

                protocols,

                hint_input: hints.and_then(|h| h.input).unwrap_or_default(),
                override_redirect: msg.override_redirect,
                mapped: false,
            };

            debug!(?xwin, "xwindow created");

            xwm.xwindows.insert(msg.window, xwin);
            xwm.conn.flush()?;
        }
        MapRequest(xproto::MapRequestEvent { window, .. }) => {
            if let Some(xwin) = xwm.xwindows.get_mut(&window) {
                // We already map the window on the X11 side; otherwise clients
                // just hang there.
                trace!(?xwin, "mapping xwindow");
                xwm.conn.map_window(window)?;

                let property = [1, 0]; // NORMAL, NONE
                xwm.conn.change_property32(
                    xproto::PropMode::REPLACE,
                    window,
                    xwm.atoms.WM_STATE,
                    xwm.atoms.WM_STATE,
                    &property,
                )?;

                xwm.conn.flush()?;
                xwin.mapped = true;
            }
        }
        MapNotify(xproto::MapNotifyEvent { window, .. }) => {
            if let Some(xwin) = xwm.xwindows.get_mut(&window) {
                trace!(?xwin, "map notify");
                xwin.mapped = true;

                if xwin.override_redirect {
                    // Do nothing.
                } else {
                    xwm.client_list.push(window);
                    xwm.client_list_stacking.push(window);

                    xwm.conn.change_property32(
                        xproto::PropMode::APPEND,
                        xwm.screen.root,
                        xwm.atoms._NET_CLIENT_LIST,
                        xproto::AtomEnum::WINDOW,
                        &[window],
                    )?;

                    xwm.conn.change_property32(
                        xproto::PropMode::APPEND,
                        xwm.screen.root,
                        xwm.atoms._NET_CLIENT_LIST_STACKING,
                        xproto::AtomEnum::WINDOW,
                        &[window],
                    )?;

                    xwm.conn.flush()?;
                }

                if let Some(serial) = xwin.serial {
                    state.raise_x11_surface(serial)
                }
            } else {
                trace!(window, "MapNotify for missing surface");
            }
        }
        ConfigureRequest(msg) => {
            trace!(
                width = msg.width,
                height = msg.height,
                x = msg.x,
                y = msg.y,
                parent = msg.parent,
                sibling = msg.sibling,
                stack_mode = ?msg.stack_mode,
                mask = ?msg.value_mask,
                "configuration request"
            );

            let serial = xwm
                .serials
                .iter()
                .find_map(|(k, v)| if *v == msg.window { Some(k) } else { None });

            if let Some(surf) = serial
                .and_then(|serial| state.xwayland_surface_lookup.get(serial))
                .and_then(|id| state.surfaces.get_mut(*id))
            {
                if let Some(conf) = surf.configuration {
                    xwm.configure_window(msg.window, conf)?;
                    surf.sent_configuration = Some(conf);
                    surf.pending_configure = None;
                }
            } else if let Some(xwin) = xwm.xwindows.get_mut(&msg.window) {
                trace!("sending synthetic configure");
                // Create a synthetic configuration event based on what the
                // window requested.

                if msg.value_mask.contains(xproto::ConfigWindow::X) {
                    xwin.x = msg.x as u32;
                }

                if msg.value_mask.contains(xproto::ConfigWindow::Y) {
                    xwin.y = msg.y as u32;
                }

                if msg.value_mask.contains(xproto::ConfigWindow::WIDTH) {
                    xwin.width = msg.width as u32;
                }

                if msg.value_mask.contains(xproto::ConfigWindow::HEIGHT) {
                    xwin.height = msg.height as u32;
                }

                let conf = SurfaceConfiguration {
                    topleft: (xwin.x, xwin.y).into(),
                    size: (xwin.width, xwin.height).into(),
                    scale: PixelScale::ONE,
                    visibility: surface::Visibility::Visible,
                    fullscreen: false,
                };

                xwm.configure_window(msg.window, conf)?;
            }
        }
        ConfigureNotify(msg) => {
            if let Some(xwin) = xwm.xwindows.get_mut(&msg.window) {
                trace!(
                    ?xwin,
                    x = msg.x,
                    y = msg.y,
                    width = msg.width,
                    height = msg.height,
                    above = msg.above_sibling,
                    or = msg.override_redirect,
                    "configure notify"
                );

                xwin.x = msg.x as u32;
                xwin.y = msg.y as u32;
                xwin.width = msg.width as u32;
                xwin.height = msg.height as u32;
                xwin.override_redirect = msg.override_redirect;

                if let Some(surf) = xwin
                    .serial
                    .and_then(|serial| state.xwayland_surface_lookup.get(&serial))
                    .and_then(|id| state.surfaces.get_mut(*id))
                {
                    surf.reconfigure(display_params, Some(xwin));
                }
            }
        }
        UnmapNotify(msg) => {
            if let Some(xwin) = xwm.xwindows.get_mut(&msg.window) {
                trace!(?xwin, "unmap notify");
                xwin.mapped = false;

                xwm.client_list.retain(|id| *id != xwin.id);
                xwm.client_list_stacking.retain(|id| *id != xwin.id);

                replace_window_list(
                    &xwm.conn,
                    xwm.screen.root,
                    xwm.atoms._NET_CLIENT_LIST,
                    &xwm.client_list,
                )?;

                replace_window_list(
                    &xwm.conn,
                    xwm.screen.root,
                    xwm.atoms._NET_CLIENT_LIST_STACKING,
                    &xwm.client_list_stacking,
                )?;
            }
        }
        DestroyNotify(msg) => {
            if let Some(xwin) = xwm.xwindows.remove(&msg.window) {
                xwm.client_list.retain(|id| *id != xwin.id);
                xwm.client_list_stacking.retain(|id| *id != xwin.id);
                xwm.serials.retain(|_, id| *id != xwin.id);

                replace_window_list(
                    &xwm.conn,
                    xwm.screen.root,
                    xwm.atoms._NET_CLIENT_LIST,
                    &xwm.client_list,
                )?;

                replace_window_list(
                    &xwm.conn,
                    xwm.screen.root,
                    xwm.atoms._NET_CLIENT_LIST_STACKING,
                    &xwm.client_list_stacking,
                )?;
            }
        }
        ClientMessage(msg) if msg.type_ == xwm.atoms.WL_SURFACE_SERIAL => {
            let [lo, hi, ..] = msg.data.as_data32();
            let serial = ((hi as u64) << 32) | lo as u64;

            xwm.serials.insert(serial, msg.window);
            if let Some(xwin) = xwm.xwindows.get_mut(&msg.window) {
                xwin.serial = Some(serial);
                trace!(?xwin, "WL_SURFACE_SERIAL set");

                // This sometimes happens after the surface is committed.
                if xwin.mapped {
                    state.delayed_map_xwin(serial);
                }
            }
        }
        ClientMessage(msg) if msg.type_ == xwm.atoms._NET_WM_STATE => {
            let [action, a, b, ..] = msg.data.as_data32();

            if let Some(xwin) = xwm.xwindows.get_mut(&msg.window) {
                let old_states = xwin.states.clone();
                for value in [a, b] {
                    const REMOVE: u32 = 0;
                    const ADD: u32 = 1;
                    const TOGGLE: u32 = 2;

                    match (action, value) {
                        (_, x11rb::NONE) => (),
                        (REMOVE, v) => {
                            xwin.states.remove(&v);
                        }
                        (ADD, v) => {
                            xwin.states.insert(v);
                        }
                        (TOGGLE, v) => {
                            if xwin.states.contains(&v) {
                                xwin.states.remove(&v);
                            } else {
                                xwin.states.insert(v);
                            }
                        }
                        _ => (),
                    }
                }

                if xwin.states != old_states {
                    let values = xwin.states.iter().copied().collect::<Vec<_>>();

                    if tracing::event_enabled!(tracing::Level::TRACE) {
                        let names = values
                            .iter()
                            .copied()
                            .map(|atom| get_atom_name(&xwm.conn, atom))
                            .collect::<Result<Vec<_>, _>>()?;
                        trace!(?xwin, ?names, "setting states");
                    }

                    xwm.conn.change_property32(
                        xproto::PropMode::REPLACE,
                        xwin.id,
                        xwm.atoms._NET_WM_STATE,
                        xproto::AtomEnum::ATOM,
                        &values,
                    )?;
                }
            }
        }
        ClientMessage(msg) if msg.type_ == xwm.atoms._NET_ACTIVE_WINDOW => {
            if let Some(target) = xwm.xwindows.get(&msg.window) {
                trace!(?target, "_NET_ACTIVE_WINDOW request");
                replace_window_list(
                    &xwm.conn,
                    xwm.screen.root,
                    xwm.atoms._NET_ACTIVE_WINDOW,
                    [target.id],
                )?;
            }
        }
        ClientMessage(msg) => {
            if tracing::event_enabled!(tracing::Level::TRACE) {
                let name = get_atom_name(&xwm.conn, msg.type_)?;
                trace!(window = ?msg.window, atom = name, "ignoring ClientMessage")
            }
        }
        PropertyNotify(msg) => {
            if tracing::event_enabled!(tracing::Level::TRACE) {
                let name = get_atom_name(&xwm.conn, msg.atom)?;
                trace!(xwin = msg.window, state = ?msg.state, atom = name, "property changed");
            }

            if let Some(xwin) = xwm.xwindows.get_mut(&msg.window) {
                match msg.atom {
                    v if v == xwm.atoms._NET_WM_NAME => {
                        xwin.title = fetch_string_property(&xwm.conn, msg.window, v)?;
                        trace!(?xwin, "title changed");
                    }
                    v if v == u32::from(xproto::AtomEnum::WM_CLASS) => {
                        xwin.app_id = fetch_class(&xwm.conn, msg.window)?;
                        trace!(?xwin, class = xwin.app_id, "class changed");
                    }
                    v if v == xwm.atoms.WM_HINTS => {
                        let hints = fetch_hints(&xwm.conn, msg.window)?;
                        trace!(?xwin, ?hints, "hints changed");
                        xwin.hint_input = hints.and_then(|h| h.input).unwrap_or_default();
                    }
                    v if v == xwm.atoms.WM_PROTOCOLS => {
                        let protocols =
                            fetch_protocols(&xwm.conn, xwm.atoms.WM_PROTOCOLS, msg.window)?;
                        trace!(?xwin, ?protocols, "protocols changed");
                    }
                    _ => (),
                }
            }
        }
        _ => (),
    }

    Ok(())
}

fn fetch_string_property(
    conn: &X11Connection,
    window: xproto::Window,
    atom: impl Into<xproto::Atom>,
) -> Result<Option<String>, ConnectionError> {
    let atom = atom.into();
    let reply = match conn
        .get_property(false, window, atom, xproto::AtomEnum::ANY, 0, 1024)?
        .reply_unchecked()
    {
        Ok(Some(reply)) => reply,
        Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(None),
        Err(err) => return Err(err),
    };

    let Some(bytes) = reply.value8() else {
        return Ok(None);
    };

    match String::from_utf8(bytes.collect()) {
        Ok(v) => Ok(Some(v)),
        Err(_) => {
            debug!(?atom, "invalid string property");
            Ok(None)
        }
    }
}

fn fetch_class(
    conn: &X11Connection,
    window: xproto::Window,
) -> Result<Option<String>, ConnectionError> {
    let reply = match x11rb::properties::WmClass::get(conn, window)?.reply_unchecked() {
        Ok(Some(reply)) => reply,
        Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(None),
        Err(err) => return Err(err),
    };

    match std::str::from_utf8(reply.class()) {
        Ok(v) => Ok(Some(v.to_owned())),
        Err(_) => {
            debug!("WM_CLASS property is invalid string");
            Ok(None)
        }
    }
}

fn fetch_hints(
    conn: &X11Connection,
    window: xproto::Window,
) -> Result<Option<x11rb::properties::WmHints>, ConnectionError> {
    match x11rb::properties::WmHints::get(conn, window)?.reply_unchecked() {
        Ok(Some(reply)) => Ok(Some(reply)),
        Ok(None) | Err(ConnectionError::ParseError(_)) => Ok(None),
        Err(err) => Err(err),
    }
}

fn fetch_protocols(
    conn: &X11Connection,
    atom: impl Into<xproto::Atom>,
    window: xproto::Window,
) -> Result<HashSet<xproto::Atom>, ConnectionError> {
    let reply = match conn
        .get_property(false, window, atom, xproto::AtomEnum::ATOM, 0, 1024)?
        .reply_unchecked()
    {
        Ok(Some(reply)) => reply,
        Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(HashSet::default()),
        Err(err) => return Err(err),
    };

    let Some(vals) = reply.value32() else {
        return Ok(HashSet::default());
    };

    Ok(vals.collect())
}

fn replace_window_list(
    conn: &X11Connection,
    win: xproto::Window,
    a: impl Into<xproto::Atom>,
    list: impl AsRef<[u32]>,
) -> Result<VoidCookie<X11Connection>, ConnectionError> {
    conn.change_property32(
        xproto::PropMode::REPLACE,
        win,
        a,
        xproto::AtomEnum::WINDOW,
        list.as_ref(),
    )
}

fn get_atom_name(
    conn: &X11Connection,
    atom: impl Into<xproto::Atom>,
) -> Result<String, ConnectionError> {
    if let Some(reply) = conn.get_atom_name(atom.into())?.reply_unchecked()? {
        Ok(String::from_utf8(reply.name).unwrap_or("<invalid string>".to_string()))
    } else {
        Ok("<unknown>".to_string())
    }
}
