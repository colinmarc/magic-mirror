// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use crate::{
    compositor::{self, CompositorEvent, ControlMessage, DisplayParams},
    session::Session,
    state::SharedState,
    waking_sender::{WakingOneshot, WakingSender},
};
use crossbeam_channel::{select, Receiver};
use hashbrown::HashMap;
use mm_protocol as protocol;
use protocol::error::ErrorCode;
use std::time;
use tracing::{debug, debug_span, error, trace};

mod validation;
use validation::*;

impl From<DisplayParams> for protocol::VirtualDisplayParameters {
    fn from(params: DisplayParams) -> Self {
        protocol::VirtualDisplayParameters {
            resolution: Some(protocol::Size {
                width: params.width,
                height: params.height,
            }),
            framerate_hz: params.framerate,
            ui_scale: Some(params.ui_scale.into()),
        }
    }
}

pub fn dispatch(
    state: SharedState,
    incoming: Receiver<protocol::MessageType>,
    outgoing: WakingSender<protocol::MessageType>,
    outgoing_dgrams: WakingSender<protocol::MessageType>,
    max_dgram_len: usize,
    done: WakingOneshot<()>,
) {
    let instant = std::time::Instant::now();

    let initial = match incoming.recv() {
        Ok(msg) => msg,
        Err(_) => {
            error!("empty worker pipe");
            return;
        }
    };

    let span = debug_span!("dispatch", initial = %initial);
    let _guard = span.enter();

    match initial {
        protocol::MessageType::LaunchSession(msg) => launch_session(state, msg, &outgoing),
        protocol::MessageType::ListSessions(_) => list_sessions(state, &outgoing),
        protocol::MessageType::UpdateSession(msg) => update_session(state, msg, &outgoing),
        protocol::MessageType::EndSession(msg) => end_session(state, msg.session_id, &outgoing),
        protocol::MessageType::Attach(msg) => attach(
            state,
            msg,
            &incoming,
            &outgoing,
            &outgoing_dgrams,
            max_dgram_len,
        ),
        _ => {
            error!("unexpected message type: {}", initial);
            send_err(&outgoing, ErrorCode::ErrorProtocolUnexpectedMessage, None);
        }
    }

    // Explicitly hang up.
    drop(incoming);
    drop(outgoing);
    drop(outgoing_dgrams);
    let _ = done.send(());

    debug!(dur = ?instant.elapsed(),"worker finished");
}

fn launch_session(
    state: SharedState,
    msg: protocol::LaunchSession,
    response: &WakingSender<protocol::MessageType>,
) {
    let display_params = match validate_display_params(msg.display_params) {
        Ok(p) => p,
        Err(ve) => {
            send_validation_error(response, ve, false);
            return;
        }
    };

    // Tracy gets confused if we have multiple sessions going.
    let guard = state.lock().unwrap();
    if cfg!(feature = "tracy") && !guard.sessions.is_empty() {
        send_err(
            response,
            ErrorCode::ErrorServer,
            Some("only one session allowed if actively debugging".into()),
        );

        return;
    }

    // Don't keep the state cloned while we launch the session.
    let vk_clone = guard.vk.clone();
    let application_config = match guard.cfg.apps.get(&msg.application_name).cloned() {
        Some(c) => c,
        None => {
            send_err(
                response,
                ErrorCode::ErrorSessionLaunchFailed,
                Some("application not found".to_string()),
            );

            return;
        }
    };

    let bug_report_dir = guard.cfg.bug_report_dir.clone();
    drop(guard);

    let session = match Session::launch(
        vk_clone,
        &msg.application_name,
        &application_config,
        display_params,
        bug_report_dir,
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to launch session: {:#}", e);
            send_err(response, ErrorCode::ErrorSessionLaunchFailed, None);
            return;
        }
    };

    let id = session.id;
    state.lock().unwrap().sessions.insert(id, session);

    // XXX: The protocol allows us to support superresolution here, but we don't
    // know how to downscale before encoding (yet).
    let msg = protocol::SessionLaunched {
        id,
        supported_streaming_resolutions: generate_streaming_res(&display_params),
    };

    response.send(msg.into()).ok();
}

fn list_sessions(state: SharedState, response: &WakingSender<protocol::MessageType>) {
    let sessions = state
        .lock()
        .unwrap()
        .sessions
        .values()
        .map(|s| protocol::session_list::Session {
            application_name: s.application_name.clone(),
            session_id: s.id,
            session_start: Some(s.started.into()),
            display_params: Some(s.display_params.into()),
            supported_streaming_resolutions: generate_streaming_res(&s.display_params),
        })
        .collect();

    let msg = protocol::SessionList { list: sessions };
    response.send(msg.into()).ok();
}

fn update_session(
    state: SharedState,
    msg: protocol::UpdateSession,
    response: &WakingSender<protocol::MessageType>,
) {
    let display_params = match validate_display_params(msg.display_params) {
        Ok(p) => p,
        Err(ve) => {
            send_validation_error(response, ve, false);
            return;
        }
    };

    let mut state = state.lock().unwrap();

    let session = match state.sessions.get_mut(&msg.session_id) {
        Some(s) => s,
        None => {
            send_err(response, ErrorCode::ErrorSessionNotFound, None);
            return;
        }
    };

    trace!(?session.display_params, ?display_params, "update_session");
    if session.display_params != display_params {
        match session.update_display_params(display_params) {
            Ok(()) => (),
            Err(e) => {
                error!("failed to update display params: {}", e);
                send_err(
                    response,
                    ErrorCode::ErrorServer,
                    Some("failed to update display params".to_string()),
                );
            }
        }
    } else {
        debug!("display params unchanged; ignoring update");
    }

    let msg = protocol::SessionUpdated {};
    response.send(msg.into()).ok();
}

fn end_session(
    state: SharedState,
    session_id: u64,
    response: &WakingSender<protocol::MessageType>,
) {
    let session = match state.lock().unwrap().sessions.remove(&session_id) {
        Some(s) => s,
        None => {
            send_err(response, ErrorCode::ErrorSessionNotFound, None);
            return;
        }
    };

    if let Err(e) = session.stop() {
        error!("failed to gracefully stop session: {}", e)
    };

    let msg = protocol::SessionEnded {};
    response.send(msg.into()).ok();
}

fn attach(
    state: SharedState,
    initial: protocol::Attach,
    incoming: &Receiver<protocol::MessageType>,
    outgoing: &WakingSender<protocol::MessageType>,
    outgoing_dgrams: &WakingSender<protocol::MessageType>,
    max_dgram_len: usize,
) {
    if initial.attachment_type() != protocol::AttachmentType::Operator {
        send_err(
            outgoing,
            ErrorCode::ErrorProtocol,
            Some("unsupported attachment type".to_string()),
        );

        return;
    }

    let session_id = initial.session_id;
    let (video_params, audio_params) = match validate_attachment(initial) {
        Ok(p) => p,
        Err(ve) => {
            send_validation_error(outgoing, ve, true);
            return;
        }
    };

    let (handle, display_params, bug_report_dir) = {
        let mut state = state.lock().unwrap();

        let session = match state.sessions.get_mut(&session_id) {
            Some(s) => s,
            None => {
                send_err(outgoing, ErrorCode::ErrorSessionNotFound, None);
                return;
            }
        };

        if !session.supports_stream(video_params) {
            send_err(
                outgoing,
                ErrorCode::ErrorAttachmentParamsNotSupported,
                Some("unsupported streaming resolution or codec".to_string()),
            );
            return;
        }

        match session.attach(true, video_params, audio_params) {
            Ok(handle) => (
                handle,
                session.display_params,
                session.bug_report_dir.clone(),
            ),
            Err(e) => {
                error!("failed to attach to session: {}", e);
                send_err(
                    outgoing,
                    ErrorCode::ErrorServer,
                    Some("failed to attach to session".to_string()),
                );
                return;
            }
        }
    };

    let span = debug_span!(
        "attachment",
        session_id,
        attachment_id = handle.attachment_id
    );

    debug!(?video_params, ?audio_params, "attaching with params");

    let _guard = span.enter();

    let handle = scopeguard::guard(handle, |h| {
        debug!("detaching from session");
        if let Some(s) = state.lock().unwrap().sessions.get_mut(&session_id) {
            s.detach(h).ok();
        };
    });

    let video_codec: protocol::VideoCodec = video_params.codec.into();
    let video_profile: protocol::VideoProfile = video_params.profile.into();
    let audio_codec: protocol::AudioCodec = audio_params.codec.into();
    let msg = protocol::Attached {
        session_id,
        attachment_id: handle.attachment_id,

        video_codec: video_codec.into(),
        streaming_resolution: Some(protocol::Size {
            width: video_params.width,
            height: video_params.height,
        }),
        video_profile: video_profile.into(),

        quality_preset: video_params.preset,

        audio_codec: audio_codec.into(),
        sample_rate_hz: audio_params.sample_rate,
        channels: Some(protocol::AudioChannels {
            channels: vec![
                protocol::audio_channels::Channel::Mono.into();
                audio_params.channels as usize
            ],
        }),
    };

    if outgoing.send(msg.into()).is_err() {
        return;
    }

    let mut last_video_frame_recv = time::Instant::now();
    let mut last_audio_frame_recv = time::Instant::now();

    // For tracing.
    #[cfg(feature = "tracy")]
    let mut video_bitrate = simple_moving_average::SingleSumSMA::<_, f64, 300>::new();

    #[cfg(feature = "tracy")]
    let worst_case_bitrate =
        (video_params.width as f64 * video_params.height as f64 * 3.0 / 2.0) * 8.0 / 1000.0;

    let mut pointer_lock = None;

    let mut debug_outputs = if bug_report_dir.is_some() {
        Some(HashMap::<u64, std::fs::File>::new())
    } else {
        None
    };

    const KEEPALIVE_TIMEOUT: time::Duration = time::Duration::from_secs(30);
    let mut keepalive_timer = crossbeam_channel::after(KEEPALIVE_TIMEOUT);

    // Five varints at max 5 bytes, plus a header, works out to around 32
    // bytes. Double for safety.
    let dgram_chunk_size = max_dgram_len - 64;

    loop {
        select! {
            recv(incoming) -> msg => {
                match msg {
                    Ok(m) => {
                        // Reset timer.
                        keepalive_timer = crossbeam_channel::after(KEEPALIVE_TIMEOUT);

                        match m {
                            protocol::MessageType::KeepAlive(_) => {},
                            protocol::MessageType::Detach(_) => return,
                            protocol::MessageType::KeyboardInput(ev) => {
                                use protocol::keyboard_input::KeyState;

                                trace!(ev.key, ev.state, "received keyboard event: {:?}", ev);


                                let state = match ev.state.try_into() {
                                    Ok(KeyState::Unknown) | Err(_) => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid key state".to_string()));
                                        return;
                                    }
                                    Ok(KeyState::Pressed) => compositor::KeyState::Pressed,
                                    Ok(KeyState::Released) => compositor::KeyState::Released,
                                    Ok(KeyState::Repeat) => compositor::KeyState::Repeat,
                                };

                                let key_code = match protocol::keyboard_input::Key::try_from(ev.key).map(key_to_evdev) {
                                    Ok(Some(scancode)) => scancode,
                                    _ => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid key".to_string()));
                                        return;
                                    }
                                };

                                let ch = match ev.char {
                                    0 => None,
                                    c => match char::from_u32(c) {
                                        Some(c) => Some(c),
                                        None => {
                                            send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid keychar".to_string()));
                                            return;
                                        }
                                    }
                                };

                                trace!(key_code, ?state, ?ch, "translated keyboard event");

                                handle.control.send(ControlMessage::KeyboardInput{
                                    key_code,
                                    state,
                                    char: ch,
                                }).ok();
                            }
                            protocol::MessageType::PointerMotion(ev) => {
                                handle.control.send(ControlMessage::PointerMotion(ev.x, ev.y)).ok();
                            }
                            protocol::MessageType::RelativePointerMotion(ev) => {
                                handle.control.send(ControlMessage::RelativePointerMotion(ev.x, ev.y)).ok();
                            }
                            protocol::MessageType::PointerEntered(_) => {
                                handle.control.send(ControlMessage::PointerEntered).ok();
                            }
                            protocol::MessageType::PointerLeft(_) => {
                                handle.control.send(ControlMessage::PointerLeft).ok();
                            }
                            protocol::MessageType::PointerInput(ev) => {
                                use protocol::pointer_input::*;

                                let state = match ev.state.try_into() {
                                    Ok(ButtonState::Unknown) | Err(_) => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid button state".to_string()));
                                        return;
                                    }
                                    Ok(ButtonState::Pressed) => compositor::ButtonState::Pressed,
                                    Ok(ButtonState::Released) => compositor::ButtonState::Released,
                                };

                                // https://gitlab.freedesktop.org/libinput/libinput/-/blob/main/include/linux/linux/input-event-codes.h#L354
                                let button_code = match ev.button.try_into() {
                                    Ok(Button::Left) => 0x110,
                                    Ok(Button::Right) => 0x111,
                                    Ok(Button::Middle) => 0x112,
                                    Ok(Button::Forward) => 0x115,
                                    Ok(Button::Back) => 0x116,
                                    _ => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid button".to_string()));
                                        return;
                                    }
                                };

                                trace!(
                                    button = ev.button,
                                    pressed = (state == compositor::ButtonState::Pressed),
                                    "sending cursor input event",
                                );

                                handle.control.send(ControlMessage::PointerInput{
                                    x: ev.x,
                                    y: ev.y,
                                    button_code,
                                    state,
                                }).ok();
                            }
                            protocol::MessageType::PointerScroll(ev) => {
                                match ev.scroll_type.try_into() {
                                    Ok(protocol::pointer_scroll::ScrollType::Continuous) => {
                                        handle.control.send(ControlMessage::PointerAxis(ev.x, ev.y)).ok();
                                    }
                                    Ok(protocol::pointer_scroll::ScrollType::Discrete) => {
                                        handle.control.send(ControlMessage::PointerAxisDiscrete(ev.x, ev.y)).ok();
                                    },
                                    _ => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid scroll type".to_string()));
                                        return;
                                    }
                                }
                            }
                            protocol::MessageType::GamepadAvailable(_) => {
                                // handle.control.send(ControlMessage::GamepadAvailable(ev.id)).ok();
                            }
                            protocol::MessageType::GamepadUnavailable(_) => {
                                // handle.control.send(ControlMessage::GamepadUnavailable(ev.id)).ok();
                            }
                            protocol::MessageType::GamepadMotion(ev) => {
                                let (scancode, is_trigger) = match protocol::gamepad_motion::GamepadAxis::try_from(ev.axis).ok().and_then(axis_to_evdev) {
                                    Some(v) => v,
                                    _ => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid gamepad axis".to_string()));
                                        return;
                                    }
                                };

                                let cm = if is_trigger {
                                    ControlMessage::GamepadTrigger {
                                        _id: ev.gamepad_id,
                                        trigger_code: scancode,
                                        value: ev.value,
                                    }
                                } else {
                                    ControlMessage::GamepadAxis {
                                        _id: ev.gamepad_id,
                                        axis_code: scancode,
                                        value: ev.value,
                                    }
                                };

                                handle.control.send(cm).ok();
                            },
                            protocol::MessageType::GamepadInput(ev) => {
                                use protocol::gamepad_input::{GamepadButton, GamepadButtonState};
                                let state = match ev.state.try_into() {
                                    Ok(GamepadButtonState::Unknown) | Err(_) => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid gamepad button state".to_string()));
                                        return;
                                    }
                                    Ok(GamepadButtonState::Pressed) => compositor::ButtonState::Pressed,
                                    Ok(GamepadButtonState::Released) => compositor::ButtonState::Released,
                                };

                                let scancode = match GamepadButton::try_from(ev.button).ok().and_then(gamepad_button_to_evdev) {
                                    Some(v) => v,
                                    _ => {
                                        send_err(outgoing, ErrorCode::ErrorProtocol, Some("invalid gamepad button".to_string()));
                                        return;
                                    }
                                };

                                handle.control.send(ControlMessage::GamepadInput {
                                    _id: ev.gamepad_id,
                                    button_code: scancode,
                                    state,
                                }).ok();
                            }
                            protocol::MessageType::Error(ev) => {
                                error!("received error from client: {}: {}", ev.err_code().as_str_name(), ev.error_text);
                            }
                            msg => {
                                debug!("received {} from client on attachment stream", msg);
                                send_err(outgoing, ErrorCode::ErrorProtocolUnexpectedMessage, None);
                                return;
                            }
                        }
                    }
                    Err(_) => return,
                }
            },
            recv(&handle.events) -> event => {
                match event {
                    Ok(CompositorEvent::Shutdown) => {
                        // The session ended, probably because the app exited.
                        state.lock().unwrap().sessions.remove(&session_id);

                        outgoing.send(protocol::SessionEnded {}.into()).ok();
                        return;
                    }
                    Ok(CompositorEvent::DisplayParamsChanged { params, reattach }) => {
                        let msg = protocol::SessionParametersChanged {
                            display_params: Some(params.into()),
                            supported_streaming_resolutions: generate_streaming_res(&params),
                            reattach_required: reattach,
                        };

                        outgoing.send(msg.into()).ok();
                        if reattach {
                            return;
                        }
                    }
                    Ok(CompositorEvent::VideoFrame { stream_seq, seq, ts, frame, .. }) => {
                        let duration = last_video_frame_recv.elapsed();
                        if duration > time::Duration::from_millis(2 * 1000 / display_params.framerate as u64) {
                            debug!(dur = ?duration, "slow video frame");
                        }

                        #[cfg(feature = "tracy")]
                        {
                            use simple_moving_average::SMA;

                            video_bitrate.add_sample(frame.len() as f64 * (8.0 / 1000.0) * (1000.0 / duration.as_millis() as f64));
                            if seq % 10 == 0 {
                                let avg = video_bitrate.get_average();
                                tracy_client::plot!("video bitrate (KB/s)", avg);
                                tracy_client::plot!("compression ratio", avg / worst_case_bitrate);
                            }
                        }

                        if let Some(ref mut debug_outputs) = debug_outputs {
                            let file = debug_outputs.entry(stream_seq).or_insert_with(|| {
                                let dir = bug_report_dir.clone().unwrap();
                                let ext = format!("{video_codec:?}").to_lowercase();
                                let path = dir.join(format!("attachment-{}-{}.{}", handle.attachment_id, stream_seq, ext));
                                std::fs::File::create(path).unwrap()
                            });

                            std::io::Write::write_all(file, &frame).unwrap();
                            std::io::Write::flush(file).unwrap();
                        }

                        last_video_frame_recv = time::Instant::now();

                        // TODO FEC

                        for (chunk, num_chunks, data) in iter_chunks(frame, dgram_chunk_size) {
                            let msg = protocol::VideoChunk {
                                session_id,
                                attachment_id: handle.attachment_id,

                                stream_seq,
                                seq,
                                data,
                                chunk,
                                num_chunks,
                                timestamp: ts,
                            };

                            outgoing_dgrams.send(msg.into()).ok();
                        }
                    }
                    Ok(CompositorEvent::AudioFrame{ stream_seq, seq, ts, frame }) => {
                            let duration = last_audio_frame_recv.elapsed();
                            if duration > time::Duration::from_millis(20) {
                                debug!(dur = ?duration, "slow audio frame");
                            }

                            last_audio_frame_recv = time::Instant::now();


                            for (chunk, num_chunks, data) in iter_chunks(frame, dgram_chunk_size) {
                                let msg = protocol::AudioChunk {
                                    session_id,
                                    attachment_id: handle.attachment_id,

                                    stream_seq,
                                    seq,
                                    data,
                                    chunk,
                                    num_chunks,
                                    timestamp: ts,
                                };

                                outgoing_dgrams.send(msg.into()).ok();
                            }
                    }
                    Ok(CompositorEvent::CursorUpdate{ image, icon, hotspot_x, hotspot_y }) => {
                        use protocol::update_cursor::CursorIcon;
                        let icon: CursorIcon = icon.map(cursor_icon_to_proto).unwrap_or(CursorIcon::None);

                        let msg = protocol::UpdateCursor {
                            image: image.unwrap_or_default(),
                            icon: icon.into(),
                            hotspot_x,
                            hotspot_y,
                        };

                        outgoing.send(msg.into()).ok();
                    }
                    Ok(CompositorEvent::PointerLocked(x, y)) => {
                        if pointer_lock.replace((x, y)).is_none() {
                            let msg = protocol::LockPointer {
                                x,
                                y,
                            };

                            outgoing.send(msg.into()).ok();
                        }
                    }
                    Ok(CompositorEvent::PointerReleased) => {
                        if pointer_lock.take().is_some() {
                            let msg = protocol::ReleasePointer {};
                            outgoing.send(msg.into()).ok();
                        }
                    }
                    Err(e) => {
                        // Mark the session defunct. It'll get GC'd.
                        error!("error in attach handler: {:#}", e);
                        if let Some(s) = state.lock().unwrap().sessions.get_mut(&session_id) {
                            s.defunct = true;
                        };

                        send_err(outgoing, ErrorCode::ErrorServer, Some("internal server error".to_string()));
                        return;
                    },
                }
            },
            recv(keepalive_timer) -> _ => {
                debug!("client hung; ending attachment");
                return;
            }
        }
    }
}

fn generate_streaming_res(display_params: &DisplayParams) -> Vec<protocol::Size> {
    // XXX: The protocol allows us to support superresolution here, but we don't
    // know how to downscale before encoding (yet).
    vec![protocol::Size {
        width: display_params.width,
        height: display_params.height,
    }]
}

fn iter_chunks(
    mut buf: bytes::Bytes,
    chunk_size: usize,
) -> impl Iterator<Item = (u32, u32, bytes::Bytes)> {
    let num_chunks = ((buf.len() + chunk_size - 1) / chunk_size) as u32;
    let mut next_chunk: u32 = 0;

    std::iter::from_fn(move || {
        if buf.is_empty() {
            return None;
        }

        let data = if buf.len() < chunk_size {
            buf.split_to(buf.len())
        } else {
            buf.split_to(chunk_size)
        };

        let chunk = next_chunk;
        next_chunk += 1;
        Some((chunk, num_chunks, data))
    })
}

fn key_to_evdev(key: protocol::keyboard_input::Key) -> Option<u32> {
    use protocol::keyboard_input::Key;

    match key {
        Key::Escape => Some(1),
        Key::Digit1 => Some(2),
        Key::Digit2 => Some(3),
        Key::Digit3 => Some(4),
        Key::Digit4 => Some(5),
        Key::Digit5 => Some(6),
        Key::Digit6 => Some(7),
        Key::Digit7 => Some(8),
        Key::Digit8 => Some(9),
        Key::Digit9 => Some(10),
        Key::Digit0 => Some(11),
        Key::Minus => Some(12),
        Key::Equal => Some(13),
        Key::Backspace => Some(14),
        Key::Tab => Some(15),
        Key::Q => Some(16),
        Key::W => Some(17),
        Key::E => Some(18),
        Key::R => Some(19),
        Key::T => Some(20),
        Key::Y => Some(21),
        Key::U => Some(22),
        Key::I => Some(23),
        Key::O => Some(24),
        Key::P => Some(25),
        Key::BracketLeft => Some(26),
        Key::BracketRight => Some(27),
        Key::Enter => Some(28),
        Key::ControlLeft => Some(29),
        Key::A => Some(30),
        Key::S => Some(31),
        Key::D => Some(32),
        Key::F => Some(33),
        Key::G => Some(34),
        Key::H => Some(35),
        Key::J => Some(36),
        Key::K => Some(37),
        Key::L => Some(38),
        Key::Semicolon => Some(39),
        Key::Quote => Some(40),
        Key::Backquote => Some(41),
        Key::ShiftLeft => Some(42),
        Key::Backslash => Some(43),
        Key::Z => Some(44),
        Key::X => Some(45),
        Key::C => Some(46),
        Key::V => Some(47),
        Key::B => Some(48),
        Key::N => Some(49),
        Key::M => Some(50),
        Key::Comma => Some(51),
        Key::Period => Some(52),
        Key::Slash => Some(53),
        Key::ShiftRight => Some(54),
        Key::NumpadMultiply => Some(55),
        Key::AltLeft => Some(56),
        Key::Space => Some(57),
        Key::CapsLock => Some(58),
        Key::F1 => Some(59),
        Key::F2 => Some(60),
        Key::F3 => Some(61),
        Key::F4 => Some(62),
        Key::F5 => Some(63),
        Key::F6 => Some(64),
        Key::F7 => Some(65),
        Key::F8 => Some(66),
        Key::F9 => Some(67),
        Key::F10 => Some(68),
        Key::NumLock => Some(69),
        Key::ScrollLock => Some(70),
        Key::Numpad7 => Some(71),
        Key::Numpad8 => Some(72),
        Key::Numpad9 => Some(73),
        Key::NumpadSubtract => Some(74),
        Key::Numpad4 => Some(75),
        Key::Numpad5 => Some(76),
        Key::Numpad6 => Some(77),
        Key::NumpadAdd => Some(78),
        Key::Numpad1 => Some(79),
        Key::Numpad2 => Some(80),
        Key::Numpad3 => Some(81),
        Key::Numpad0 => Some(82),
        Key::NumpadDecimal => Some(83),
        Key::Lang5 => Some(85),
        Key::IntlBackslash => Some(86),
        Key::F11 => Some(87),
        Key::F12 => Some(88),
        Key::IntlRo => Some(89),
        Key::Katakana => Some(90),
        Key::Hiragana => Some(91),
        Key::Convert => Some(92),
        Key::KanaMode => Some(93),
        Key::NonConvert => Some(94),
        Key::NumpadEnter => Some(96),
        Key::ControlRight => Some(97),
        Key::NumpadDivide => Some(98),
        Key::PrintScreen => Some(99),
        Key::AltRight => Some(100),
        Key::Home => Some(102),
        Key::ArrowUp => Some(103),
        Key::PageUp => Some(104),
        Key::ArrowLeft => Some(105),
        Key::ArrowRight => Some(106),
        Key::End => Some(107),
        Key::ArrowDown => Some(108),
        Key::PageDown => Some(109),
        Key::Insert => Some(110),
        Key::Delete => Some(111),
        Key::NumpadEqual => Some(117),
        Key::Pause => Some(119),
        Key::NumpadComma => Some(121),
        Key::IntlYen => Some(124),
        Key::MetaLeft => Some(125),
        Key::MetaRight => Some(126),
        Key::ContextMenu => Some(127),
        Key::Help => Some(138),
        Key::NumpadParenLeft => Some(179),
        Key::NumpadParenRight => Some(180),
        // Linux doesn't have this, so we'll map it to the regular backspace.
        Key::NumpadBackspace => Some(14),
        // TODO: Can't find these at all.
        Key::Fn | Key::FnLock => None,
        Key::Lang1 | Key::Lang2 | Key::Lang3 | Key::Lang4 => None,
        Key::NumpadClear
        | Key::NumpadClearEntry
        | Key::NumpadHash
        | Key::NumpadMemoryAdd
        | Key::NumpadMemoryClear
        | Key::NumpadMemoryRecall
        | Key::NumpadMemoryStore
        | Key::NumpadMemorySubtract => None,
        Key::Unknown => None,
    }
}

fn axis_to_evdev(axis: protocol::gamepad_motion::GamepadAxis) -> Option<(u32, bool)> {
    use protocol::gamepad_motion::GamepadAxis;
    match axis {
        GamepadAxis::LeftX => Some((0x00, false)),       // ABS_X
        GamepadAxis::LeftY => Some((0x01, false)),       // ABS_Y
        GamepadAxis::RightX => Some((0x03, false)),      // ABS_RX
        GamepadAxis::RightY => Some((0x04, false)),      // ABS_RY,
        GamepadAxis::LeftTrigger => Some((0x02, true)),  // ABS_Z
        GamepadAxis::RightTrigger => Some((0x05, true)), // ABS_RZ
        GamepadAxis::Unknown => None,
    }
}

fn gamepad_button_to_evdev(button: protocol::gamepad_input::GamepadButton) -> Option<u32> {
    use protocol::gamepad_input::GamepadButton;

    // TODO: My Dualsense actually reports Dpad events as an axis (ABS_HAT0X).
    // Otherwise, this simulates a Sony controller.

    match button {
        GamepadButton::DpadLeft => Some(0x222),      // BTN_DPAD_LEFT
        GamepadButton::DpadRight => Some(0x223),     // BTN_DPAD_RIGHT
        GamepadButton::DpadUp => Some(0x220),        // BTN_DPAD_UP
        GamepadButton::DpadDown => Some(0x221),      // BTN_DPAD_DOWN
        GamepadButton::South => Some(0x130),         // BTN_SOUTH
        GamepadButton::East => Some(0x131),          // BTN_EAST
        GamepadButton::North => Some(0x133),         // BTN_NORTH
        GamepadButton::West => Some(0x134),          // BTN_WEST
        GamepadButton::C => Some(0x132),             // BTN_C
        GamepadButton::Z => Some(0x135),             // BTN_Z
        GamepadButton::ShoulderLeft => Some(0x136),  // BTN_TL
        GamepadButton::ShoulderRight => Some(0x137), // BTN_TR
        GamepadButton::JoystickLeft => Some(0x13d),  // BTN_THUMBL
        GamepadButton::JoystickRight => Some(0x13e), // BTN_THUMBR
        GamepadButton::Start => Some(0x13b),         // BTN_START
        GamepadButton::Select => Some(0x13a),        // BTN_SELECT
        GamepadButton::Logo => Some(0x13c),          // BTN_MODE
        GamepadButton::Share => None,                // TODO I'm not sure what code to use.
        GamepadButton::TriggerLeft => Some(0x138),   // BTN_TL2
        GamepadButton::TriggerRight => Some(0x139),  // BTN_TL3
        GamepadButton::Unknown => None,
    }
}

fn cursor_icon_to_proto(icon: cursor_icon::CursorIcon) -> protocol::update_cursor::CursorIcon {
    use protocol::update_cursor::CursorIcon;

    match icon {
        cursor_icon::CursorIcon::ContextMenu => CursorIcon::ContextMenu,
        cursor_icon::CursorIcon::Help => CursorIcon::Help,
        cursor_icon::CursorIcon::Pointer => CursorIcon::Pointer,
        cursor_icon::CursorIcon::Progress => CursorIcon::Progress,
        cursor_icon::CursorIcon::Wait => CursorIcon::Wait,
        cursor_icon::CursorIcon::Cell => CursorIcon::Cell,
        cursor_icon::CursorIcon::Crosshair => CursorIcon::Crosshair,
        cursor_icon::CursorIcon::Text => CursorIcon::Text,
        cursor_icon::CursorIcon::VerticalText => CursorIcon::VerticalText,
        cursor_icon::CursorIcon::Alias => CursorIcon::Alias,
        cursor_icon::CursorIcon::Copy => CursorIcon::Copy,
        cursor_icon::CursorIcon::Move => CursorIcon::Move,
        cursor_icon::CursorIcon::NoDrop => CursorIcon::NoDrop,
        cursor_icon::CursorIcon::NotAllowed => CursorIcon::NotAllowed,
        cursor_icon::CursorIcon::Grab => CursorIcon::Grab,
        cursor_icon::CursorIcon::Grabbing => CursorIcon::Grabbing,
        cursor_icon::CursorIcon::EResize => CursorIcon::EResize,
        cursor_icon::CursorIcon::NResize => CursorIcon::NResize,
        cursor_icon::CursorIcon::NeResize => CursorIcon::NeResize,
        cursor_icon::CursorIcon::NwResize => CursorIcon::NwResize,
        cursor_icon::CursorIcon::SResize => CursorIcon::SResize,
        cursor_icon::CursorIcon::SeResize => CursorIcon::SeResize,
        cursor_icon::CursorIcon::SwResize => CursorIcon::SwResize,
        cursor_icon::CursorIcon::WResize => CursorIcon::WResize,
        cursor_icon::CursorIcon::EwResize => CursorIcon::EwResize,
        cursor_icon::CursorIcon::NsResize => CursorIcon::NsResize,
        cursor_icon::CursorIcon::NeswResize => CursorIcon::NeswResize,
        cursor_icon::CursorIcon::NwseResize => CursorIcon::NwseResize,
        cursor_icon::CursorIcon::ColResize => CursorIcon::ColResize,
        cursor_icon::CursorIcon::RowResize => CursorIcon::RowResize,
        cursor_icon::CursorIcon::AllScroll => CursorIcon::AllScroll,
        cursor_icon::CursorIcon::ZoomIn => CursorIcon::ZoomIn,
        cursor_icon::CursorIcon::ZoomOut => CursorIcon::ZoomOut,
        _ => CursorIcon::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk() {
        let frame = bytes::Bytes::from(vec![9; 3536]);
        let mut chunks = iter_chunks(frame, 1200);
        let (chunk, num_chunks, data) = chunks.next().unwrap();
        assert_eq!(chunk, 0);
        assert_eq!(num_chunks, 3);
        assert_eq!(data.len(), 1200);

        let (chunk, num_chunks, data) = chunks.next().unwrap();
        assert_eq!(chunk, 1);
        assert_eq!(num_chunks, 3);
        assert_eq!(data.len(), 1200);

        let (chunk, num_chunks, data) = chunks.next().unwrap();
        assert_eq!(chunk, 2);
        assert_eq!(num_chunks, 3);
        assert_eq!(data.len(), 1136);

        assert!(chunks.next().is_none());
    }
}
