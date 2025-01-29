// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{collections::BTreeMap, fs, path::PathBuf, time};

use chunk::iter_chunks;
use mm_protocol::{self as protocol, error::ErrorCode};
use tracing::{debug, debug_span, error, trace, trace_span};

mod chunk;
mod stats;

use super::{validate_attachment, validate_gamepad, ServerError, ValidationError};
use crate::{
    config,
    session::{
        compositor,
        control::{ControlMessage, DisplayParams, SessionEvent},
        Attachment,
    },
};

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

struct AttachmentHandler<'a> {
    ctx: &'a super::Context,
    handle: Attachment,

    server_config: config::ServerConfig,
    session_display_params: DisplayParams,
    attached: protocol::Attached,
    superscale: f64,

    // Keep track of the pointer lock, and debounce session events for it.
    pointer_lock: Option<(f64, f64)>,

    chunk_size: usize,

    last_video_frame_recvd: time::Instant,
    last_audio_frame_recvd: time::Instant,
    keepalive_timer: crossbeam_channel::Receiver<time::Instant>,

    // For saving the bitstream to disk in bug reports.
    bug_report_dir: Option<PathBuf>,
    bug_report_files: BTreeMap<u64, fs::File>,

    stats: stats::AttachmentStats,
}

#[derive(Debug, Clone)]
enum AttachmentError {
    Finished,
    ServerError(ErrorCode, Option<String>),
}

/// How long to wait before kicking the client for inactivity.
const KEEPALIVE_TIMEOUT: time::Duration = time::Duration::from_secs(30);

pub fn attach(ctx: &super::Context, msg: protocol::Attach) -> Result<(), ServerError> {
    let session_id = msg.session_id;
    let handler = AttachmentHandler::new(ctx, msg)?;

    // Make sure we detach, even if we panic.
    let mut handler = scopeguard::guard(handler, |h| {
        debug!("detaching from session");
        if let Some(s) = ctx.state.lock().sessions.get_mut(&session_id) {
            s.detach(h.handle).ok();
        };
    });

    handler.run()
}

impl<'a> AttachmentHandler<'a> {
    fn new(ctx: &'a super::Context, msg: protocol::Attach) -> Result<Self, ServerError> {
        if msg.attachment_type() != protocol::AttachmentType::Operator {
            return Err(ServerError(
                ErrorCode::ErrorAttachmentParamsNotSupported,
                Some("unsupported attachment type".to_string()),
            ));
        }

        let session_id = msg.session_id;
        let (video_params, audio_params) = validate_attachment(msg).map_err(|err| match err {
            ValidationError::Unsupported(text) => {
                ServerError(ErrorCode::ErrorAttachmentParamsNotSupported, Some(text))
            }
            ValidationError::Invalid(text) => ServerError(ErrorCode::ErrorProtocol, Some(text)),
        })?;

        let mut guard = ctx.state.lock();
        let server_config = guard.cfg.server.clone();

        let attachment_id = guard.id_generator.next_int();
        let Some(session) = guard.sessions.get_mut(&session_id) else {
            return Err(ServerError(ErrorCode::ErrorSessionNotFound, None));
        };

        if !session.supports_stream(video_params) {
            return Err(ServerError(
                ErrorCode::ErrorAttachmentParamsNotSupported,
                Some("unsupported streaming resolution or codec".to_string()),
            ));
        }

        let handle = match session.attach(attachment_id, true, video_params, audio_params) {
            Ok(v) => v,
            Err(err) => {
                error!(?err, "failed to attach to session");
                return Err(ServerError(
                    ErrorCode::ErrorServer,
                    Some("failed to attach to session".to_string()),
                ));
            }
        };

        let app_id = session.application_id.clone();
        let display_params = session.display_params;
        let bug_report_dir = session.bug_report_dir.clone();
        drop(guard);

        let superscale = display_params.height as f64 / video_params.height as f64;
        assert_eq!(display_params.height % video_params.height, 0);
        assert_eq!(
            display_params.width as f64 / video_params.width as f64,
            superscale
        );

        debug!(
            ?video_params,
            ?audio_params,
            ?superscale,
            "attaching with params"
        );

        let video_codec: protocol::VideoCodec = video_params.codec.into();
        let video_profile: protocol::VideoProfile = video_params.profile.into();
        let audio_codec: protocol::AudioCodec = audio_params.codec.into();
        let attached = protocol::Attached {
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

        let pointer_lock = None;

        let keepalive_timer = crossbeam_channel::after(KEEPALIVE_TIMEOUT);

        // max_dgram_len is our overall MTU. The MM protocol header is 2-10 bytes,
        // and then we include seven varints (maximum 5 bytes each) and a bool of
        // metadata, plus an optional 12-ish bytes of FEC information. 64 bytes of
        // headroom should cover the worst case. However, a little extra will
        // increase the chance that the packet is coalesced into an existing QUIC
        // packet.
        let chunk_size = ctx.max_dgram_len - 128;

        let now = time::Instant::now();

        Ok(Self {
            ctx,
            handle,

            server_config,
            session_display_params: display_params,
            attached,
            superscale,

            pointer_lock,

            chunk_size,

            last_video_frame_recvd: now,
            last_audio_frame_recvd: now,
            keepalive_timer,

            bug_report_dir,
            bug_report_files: BTreeMap::default(),

            stats: stats::AttachmentStats::new(app_id),
        })
    }

    fn run(&mut self) -> Result<(), ServerError> {
        let span = debug_span!(
            "attachment",
            self.handle.session_id,
            self.handle.attachment_id,
        );
        let _guard = span.enter();

        if self
            .ctx
            .outgoing
            .send(self.attached.clone().into())
            .is_err()
        {
            // Client already hung up.
            return Ok(());
        }

        loop {
            crossbeam_channel::select! {
                recv(self.ctx.incoming) -> msg => {
                    match msg {
                        Ok(m) => {
                            // Reset timer.
                            self.keepalive_timer = crossbeam_channel::after(KEEPALIVE_TIMEOUT);

                            match self.handle_attachment_message(m) {
                                Ok(_) => (),
                                Err(AttachmentError::Finished) => return Ok(()),
                                Err(AttachmentError::ServerError(code, text)) => {
                                    return Err(ServerError(code, text));
                                }
                            }
                        }
                        Err(_) => return Ok(()), // Client fin.
                    }
                },
                recv(&self.handle.events) -> event => {
                    match event {
                        Ok(ev) => match self.handle_session_event(ev) {
                            Ok(_) => (),
                            Err(AttachmentError::Finished) => return Ok(()),
                            Err(AttachmentError::ServerError(code, text)) => {
                                return Err(ServerError(code, text));
                            }
                        }
                        Err(e) => {
                            // Mark the session defunct. It'll get GC'd.
                            error!("error in attach handler: {:#}", e);

                            if let Some(s) = self.ctx.state.lock().sessions.get_mut(&self.handle.session_id) {
                                s.defunct = true;
                            };

                            return Err(ServerError(
                                ErrorCode::ErrorServer,
                                Some("internal server error".to_string()),
                            ));
                        }
                    }
                },
                recv(self.keepalive_timer) -> _ => {
                    debug!("client hung; ending attachment");
                    return Ok(());
                }
            }
        }
    }

    fn handle_attachment_message(
        &mut self,
        msg: protocol::MessageType,
    ) -> Result<(), AttachmentError> {
        match msg {
            protocol::MessageType::KeepAlive(_) => {}
            protocol::MessageType::Detach(_) => return Err(AttachmentError::Finished),
            protocol::MessageType::RequestVideoRefresh(ev) => {
                self.handle
                    .control
                    .send(ControlMessage::RequestVideoRefresh(ev.stream_seq))
                    .ok();
            }
            protocol::MessageType::KeyboardInput(ev) => {
                use protocol::keyboard_input::KeyState;

                trace!(ev.key, ev.state, "received keyboard event: {:?}", ev);

                let state = match ev.state.try_into() {
                    Ok(KeyState::Unknown) | Err(_) => {
                        return Err(AttachmentError::ServerError(
                            ErrorCode::ErrorProtocol,
                            Some("invalid key state".to_string()),
                        ));
                    }
                    Ok(KeyState::Pressed) => compositor::KeyState::Pressed,
                    Ok(KeyState::Released) => compositor::KeyState::Released,
                    Ok(KeyState::Repeat) => compositor::KeyState::Repeat,
                };

                let key_code =
                    match protocol::keyboard_input::Key::try_from(ev.key).map(key_to_evdev) {
                        Ok(Some(scancode)) => scancode,
                        _ => {
                            return Err(AttachmentError::ServerError(
                                ErrorCode::ErrorProtocol,
                                Some("invalid key".to_string()),
                            ));
                        }
                    };

                let ch = match ev.char {
                    0 => None,
                    c => match char::from_u32(c) {
                        Some(c) => Some(c),
                        None => {
                            return Err(AttachmentError::ServerError(
                                ErrorCode::ErrorProtocol,
                                Some("invalid keychar".to_string()),
                            ));
                        }
                    },
                };

                trace!(key_code, ?state, ?ch, "translated keyboard event");

                self.handle
                    .control
                    .send(ControlMessage::KeyboardInput {
                        key_code,
                        state,
                        char: ch,
                    })
                    .ok();
            }
            protocol::MessageType::PointerMotion(ev) => {
                let x = ev.x * self.superscale;
                let y = ev.y * self.superscale;
                self.handle
                    .control
                    .send(ControlMessage::PointerMotion(x, y))
                    .ok();
            }
            protocol::MessageType::RelativePointerMotion(ev) => {
                let x = ev.x * self.superscale;
                let y = ev.y * self.superscale;
                self.handle
                    .control
                    .send(ControlMessage::RelativePointerMotion(x, y))
                    .ok();
            }
            protocol::MessageType::PointerEntered(_) => {
                self.handle
                    .control
                    .send(ControlMessage::PointerEntered)
                    .ok();
            }
            protocol::MessageType::PointerLeft(_) => {
                self.handle.control.send(ControlMessage::PointerLeft).ok();
            }
            protocol::MessageType::PointerInput(ev) => {
                use protocol::pointer_input::*;

                let state = match ev.state.try_into() {
                    Ok(ButtonState::Unknown) | Err(_) => {
                        return Err(AttachmentError::ServerError(
                            ErrorCode::ErrorProtocol,
                            Some("invalid button state".to_string()),
                        ));
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
                        return Err(AttachmentError::ServerError(
                            ErrorCode::ErrorProtocol,
                            Some("invalid button".to_string()),
                        ));
                    }
                };

                trace!(
                    button = ev.button,
                    pressed = (state == compositor::ButtonState::Pressed),
                    "sending cursor input event",
                );

                self.handle
                    .control
                    .send(ControlMessage::PointerInput {
                        x: ev.x,
                        y: ev.y,
                        button_code,
                        state,
                    })
                    .ok();
            }
            protocol::MessageType::PointerScroll(ev) => match ev.scroll_type.try_into() {
                Ok(protocol::pointer_scroll::ScrollType::Continuous) => {
                    let x = ev.x * self.superscale;
                    let y = ev.y * self.superscale;
                    self.handle
                        .control
                        .send(ControlMessage::PointerAxis(x, y))
                        .ok();
                }
                Ok(protocol::pointer_scroll::ScrollType::Discrete) => {
                    self.handle
                        .control
                        .send(ControlMessage::PointerAxisDiscrete(ev.x, ev.y))
                        .ok();
                }
                _ => {
                    return Err(AttachmentError::ServerError(
                        ErrorCode::ErrorProtocol,
                        Some("invalid scroll type".to_string()),
                    ));
                }
            },
            protocol::MessageType::GamepadAvailable(ev) => {
                let (id, _layout) = match validate_gamepad(ev.gamepad) {
                    Ok(v) => v,
                    Err(ValidationError::Invalid(text)) => {
                        return Err(AttachmentError::ServerError(
                            ErrorCode::ErrorProtocol,
                            Some(text),
                        ))
                    }
                    Err(_) => unreachable!(),
                };

                self.handle
                    .control
                    .send(ControlMessage::GamepadAvailable(id))
                    .ok();
            }
            protocol::MessageType::GamepadUnavailable(ev) => {
                self.handle
                    .control
                    .send(ControlMessage::GamepadUnavailable(ev.id))
                    .ok();
            }
            protocol::MessageType::GamepadMotion(ev) => {
                let (scancode, is_trigger) =
                    match protocol::gamepad_motion::GamepadAxis::try_from(ev.axis)
                        .ok()
                        .and_then(axis_to_evdev)
                    {
                        Some(v) => v,
                        _ => {
                            return Err(AttachmentError::ServerError(
                                ErrorCode::ErrorProtocol,
                                Some("invalid gamepad axis".to_string()),
                            ));
                        }
                    };

                let cm = if is_trigger {
                    ControlMessage::GamepadTrigger {
                        id: ev.gamepad_id,
                        trigger_code: scancode,
                        value: ev.value,
                    }
                } else {
                    ControlMessage::GamepadAxis {
                        id: ev.gamepad_id,
                        axis_code: scancode,
                        value: ev.value,
                    }
                };

                self.handle.control.send(cm).ok();
            }
            protocol::MessageType::GamepadInput(ev) => {
                use protocol::gamepad_input::{GamepadButton, GamepadButtonState};
                let state = match ev.state.try_into() {
                    Ok(GamepadButtonState::Unknown) | Err(_) => {
                        return Err(AttachmentError::ServerError(
                            ErrorCode::ErrorProtocol,
                            Some("invalid gamepad button state".to_string()),
                        ));
                    }
                    Ok(GamepadButtonState::Pressed) => compositor::ButtonState::Pressed,
                    Ok(GamepadButtonState::Released) => compositor::ButtonState::Released,
                };

                let scancode = match GamepadButton::try_from(ev.button)
                    .ok()
                    .and_then(gamepad_button_to_evdev)
                {
                    Some(v) => v,
                    _ => {
                        return Err(AttachmentError::ServerError(
                            ErrorCode::ErrorProtocol,
                            Some("invalid gamepad button".to_string()),
                        ));
                    }
                };

                self.handle
                    .control
                    .send(ControlMessage::GamepadInput {
                        id: ev.gamepad_id,
                        button_code: scancode,
                        state,
                    })
                    .ok();
            }
            protocol::MessageType::Error(ev) => {
                error!(
                    "received error from client: {}: {}",
                    ev.err_code().as_str_name(),
                    ev.error_text
                );
            }
            msg => {
                debug!("received {} from client on attachment stream", msg);
                return Err(AttachmentError::ServerError(
                    ErrorCode::ErrorProtocolUnexpectedMessage,
                    None,
                ));
            }
        }

        Ok(())
    }

    fn handle_session_event(&mut self, event: SessionEvent) -> Result<(), AttachmentError> {
        match event {
            SessionEvent::Shutdown => {
                // The session ended, probably because the app exited.
                self.ctx
                    .state
                    .lock()
                    .sessions
                    .remove(&self.handle.session_id);

                self.send(protocol::SessionEnded {});
                return Err(AttachmentError::Finished);
            }
            SessionEvent::DisplayParamsChanged { params, reattach } => {
                self.session_display_params = params;
                let msg = protocol::SessionParametersChanged {
                    display_params: Some(params.into()),
                    supported_streaming_resolutions: super::generate_streaming_res(&params),
                    reattach_required: reattach,
                };

                self.send(msg);
                if reattach {
                    return Err(AttachmentError::Finished);
                }
            }
            SessionEvent::VideoFrame {
                stream_seq,
                seq,
                ts,
                frame,
                hierarchical_layer,
                ..
            } => {
                let duration = self.last_video_frame_recvd.elapsed();
                if duration
                    > time::Duration::from_secs_f32(
                        1.5 / self.session_display_params.framerate as f32,
                    )
                {
                    debug!(dur = ?duration, "slow video frame");
                }

                self.last_video_frame_recvd = time::Instant::now();
                self.stats.record_frame(seq, frame.len(), duration);

                if let Some(dir) = &self.bug_report_dir {
                    let file = self.bug_report_files.entry(stream_seq).or_insert_with(|| {
                        let ext = format!("{:?}", self.attached.video_codec()).to_lowercase();
                        let path = dir.join(format!(
                            "attachment-{:02}-{}.{}",
                            stream_seq, self.handle.attachment_id, ext
                        ));
                        std::fs::File::create(path).unwrap()
                    });

                    std::io::Write::write_all(file, &frame).unwrap();
                    std::io::Write::flush(file).unwrap();
                }

                let optional = hierarchical_layer != 0;
                let fec_ratio = self
                    .server_config
                    .video_fec_ratios
                    .get(hierarchical_layer as usize)
                    .copied()
                    .unwrap_or_default();

                for chunk in iter_chunks(frame, self.chunk_size, fec_ratio) {
                    let msg = protocol::VideoChunk {
                        session_id: self.handle.session_id,
                        attachment_id: self.handle.attachment_id,

                        stream_seq,
                        seq,
                        data: chunk.data,
                        chunk: chunk.index,
                        num_chunks: chunk.num_chunks,
                        frame_optional: optional,
                        timestamp: ts,

                        fec_metadata: chunk.fec_metadata,
                    };

                    let buf = trace_span!("encode_message").in_scope(|| {
                        let mut buf = vec![0; self.ctx.max_dgram_len];
                        let len = match protocol::encode_message(&msg.into(), &mut buf) {
                            Ok(v) => v,
                            Err(err) => {
                                error!(?err, "failed to encode video chunk");
                                return Err(AttachmentError::ServerError(
                                    ErrorCode::ErrorServer,
                                    None,
                                ));
                            }
                        };

                        buf.truncate(len);
                        Ok(buf)
                    })?;

                    let _ = self.ctx.outgoing_dgrams.send(buf);
                }
            }
            SessionEvent::AudioFrame {
                stream_seq,
                seq,
                ts,
                frame,
            } => {
                let duration = self.last_audio_frame_recvd.elapsed();
                if duration
                    > time::Duration::from_secs_f32(
                        1.5 / self.session_display_params.framerate as f32,
                    )
                {
                    debug!(dur = ?duration, "slow audio frame");
                }

                self.last_audio_frame_recvd = time::Instant::now();
                self.stats.record_frame(seq, frame.len(), duration);

                for chunk in iter_chunks(frame, self.chunk_size, 0.0) {
                    let msg = protocol::AudioChunk {
                        session_id: self.handle.session_id,
                        attachment_id: self.handle.attachment_id,

                        stream_seq,
                        seq,
                        data: chunk.data,
                        chunk: chunk.index,
                        num_chunks: chunk.num_chunks,
                        timestamp: ts,

                        fec_metadata: chunk.fec_metadata,
                    };

                    let buf = trace_span!("encode_message").in_scope(|| {
                        let mut buf = vec![0; self.ctx.max_dgram_len];
                        let len = match protocol::encode_message(&msg.into(), &mut buf) {
                            Ok(v) => v,
                            Err(err) => {
                                error!(?err, "failed to encode video chunk");
                                return Err(AttachmentError::ServerError(
                                    ErrorCode::ErrorServer,
                                    None,
                                ));
                            }
                        };

                        buf.truncate(len);
                        Ok(buf)
                    })?;

                    let _ = self.ctx.outgoing_dgrams.send(buf);
                }
            }
            SessionEvent::CursorUpdate {
                image,
                icon,
                hotspot_x,
                hotspot_y,
            } => {
                use protocol::update_cursor::CursorIcon;
                let icon: CursorIcon = icon.map(cursor_icon_to_proto).unwrap_or(CursorIcon::None);

                let msg = protocol::UpdateCursor {
                    image: image.unwrap_or_default(),
                    icon: icon.into(),
                    hotspot_x,
                    hotspot_y,
                };

                self.send(msg);
            }
            SessionEvent::PointerLocked(x, y) => {
                let x = x / self.superscale;
                let y = y / self.superscale;

                if self.pointer_lock.replace((x, y)).is_none() {
                    self.send(protocol::LockPointer { x, y });
                }
            }
            SessionEvent::PointerReleased => {
                if self.pointer_lock.take().is_some() {
                    self.send(protocol::ReleasePointer {});
                }
            }
        }

        Ok(())
    }

    fn send(&self, msg: impl Into<protocol::MessageType>) {
        let _ = self.ctx.outgoing.send(msg.into());
    }
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
