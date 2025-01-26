// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{fs::File, path::Path};

use anyhow::bail;
use bytes::Bytes;
use crossbeam_channel::Receiver;
use mm_protocol as protocol;
use protocol::error::ErrorCode;
use tracing::{debug, debug_span, error, trace};

use crate::{
    session::{control::DisplayParams, Session},
    state::SharedState,
    waking_sender::{WakingOneshot, WakingSender},
};

mod attachment;
mod validation;

use validation::*;

#[derive(Debug, Clone)]
struct ServerError(protocol::error::ErrorCode, Option<String>);

struct Context {
    state: SharedState,
    incoming: Receiver<protocol::MessageType>,
    outgoing: WakingSender<protocol::MessageType>,
    outgoing_dgrams: WakingSender<protocol::MessageType>,
    max_dgram_len: usize,
}

impl Context {
    fn send_err(&self, err: ServerError) {
        let ServerError(code, text) = err;

        if let Some(text) = text.as_ref() {
            debug!("handler ended with error: {:?}: {}", code, text);
        } else {
            debug!("handler ended with error: {:?}", code);
        }

        let err = protocol::Error {
            err_code: code.into(),
            error_text: text.unwrap_or_default(),
        };

        self.outgoing.send(err.into()).ok();
    }
}

type Result<M> = std::result::Result<M, ServerError>;

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

    let ctx = Context {
        state,
        incoming,
        outgoing,
        outgoing_dgrams,
        max_dgram_len,
    };

    match initial {
        protocol::MessageType::ListApplications(msg) => roundtrip(list_applications, &ctx, msg),
        protocol::MessageType::FetchApplicationImage(msg) => roundtrip(fetch_img, &ctx, msg),
        protocol::MessageType::LaunchSession(msg) => roundtrip(launch_session, &ctx, msg),
        protocol::MessageType::ListSessions(msg) => roundtrip(list_sessions, &ctx, msg),
        protocol::MessageType::UpdateSession(msg) => roundtrip(update_session, &ctx, msg),
        protocol::MessageType::EndSession(msg) => roundtrip(end_session, &ctx, msg),
        protocol::MessageType::Attach(msg) => {
            if let Err(err) = attachment::attach(&ctx, msg) {
                ctx.send_err(err);
            } else {
                // Clean exit, no final message.
            }
        }
        _ => {
            error!("unexpected message type: {}", initial);
            ctx.send_err(ServerError(ErrorCode::ErrorProtocolUnexpectedMessage, None));
        }
    };

    // Explicitly hang up.
    drop(ctx);
    let _ = done.send(());

    debug!(dur = ?instant.elapsed(),"worker finished");
}

fn roundtrip<F, Req, Resp>(f: F, ctx: &Context, req: Req)
where
    Resp: Into<protocol::MessageType>,
    F: Fn(&Context, Req) -> Result<Resp>,
{
    match f(ctx, req) {
        Ok(resp) => {
            if ctx.outgoing.send(resp.into()).is_err() {
                debug!("client hung up before response could be sent");
            }
        }
        Err(err) => {
            error!(?err, "handler returned error");
            ctx.send_err(err);
        }
    }
}

fn list_applications(
    ctx: &Context,
    _msg: protocol::ListApplications,
) -> Result<protocol::ApplicationList> {
    let apps = ctx
        .state
        .lock()
        .cfg
        .apps
        .iter()
        .map(|(id, app)| protocol::application_list::Application {
            id: id.clone(),
            description: app.description.clone().unwrap_or_default(),
            folder: app.path.clone(),
            images_available: if app.header_image.is_some() {
                vec![protocol::ApplicationImageFormat::Header.into()]
            } else {
                vec![]
            },
        })
        .collect();

    Ok(protocol::ApplicationList { list: apps })
}

fn fetch_img(
    ctx: &Context,
    msg: protocol::FetchApplicationImage,
) -> Result<protocol::ApplicationImage> {
    match msg.format.try_into() {
        Ok(protocol::ApplicationImageFormat::Header) => (),
        _ => {
            return Err(ServerError(
                ErrorCode::ErrorProtocol,
                Some("unknown application image type".to_string()),
            ));
        }
    }

    let Some(config) = ctx.state.lock().cfg.apps.get(&msg.application_id).cloned() else {
        return Err(ServerError(
            ErrorCode::ErrorApplicationNotFound,
            Some("application not found".to_string()),
        ));
    };

    let Some(path) = &config.header_image else {
        return Err(ServerError(
            ErrorCode::ErrorApplicationNotFound,
            Some("image not found".to_string()),
        ));
    };

    match read_file(path, crate::config::MAX_IMAGE_SIZE) {
        Ok(image_data) => Ok(protocol::ApplicationImage { image_data }),
        Err(err) => {
            error!(path = ?path, ?err, "failed to load image data");

            Err(ServerError(
                ErrorCode::ErrorServer,
                Some("failed to load image".into()),
            ))
        }
    }
}

fn launch_session(
    ctx: &Context,
    msg: protocol::LaunchSession,
) -> Result<protocol::SessionLaunched> {
    let display_params = validate_display_params(msg.display_params).map_err(|err| match err {
        ValidationError::Unsupported(text) => {
            ServerError(ErrorCode::ErrorSessionParamsNotSupported, Some(text))
        }
        ValidationError::Invalid(text) => ServerError(ErrorCode::ErrorProtocol, Some(text)),
    })?;

    // Tracy gets confused if we have multiple sessions going.
    let mut guard = ctx.state.lock();
    if cfg!(feature = "tracy") && !guard.sessions.is_empty() {
        return Err(ServerError(
            ErrorCode::ErrorServer,
            Some("only one session allowed if actively debugging".into()),
        ));
    }

    // Don't keep the state cloned while we launch the session.
    let vk_clone = guard.vk.clone();
    let Some(application_config) = guard.cfg.apps.get(&msg.application_id).cloned() else {
        return Err(ServerError(
            ErrorCode::ErrorSessionLaunchFailed,
            Some("application not found".to_string()),
        ));
    };

    for gamepad in msg.permanent_gamepads.clone() {
        validate_gamepad(Some(gamepad)).map_err(|err| match err {
            ValidationError::Unsupported(text) => {
                ServerError(ErrorCode::ErrorSessionParamsNotSupported, Some(text))
            }
            ValidationError::Invalid(text) => ServerError(ErrorCode::ErrorProtocol, Some(text)),
        })?;
    }

    let bug_report_dir = guard.cfg.bug_report_dir.clone();
    let (session_seq, session_id) = guard.generate_session_id();
    drop(guard);

    // Create a folder in the bug report directory just for this session.
    let mut bug_report_dir = bug_report_dir;
    if let Some(ref mut dir) = bug_report_dir {
        dir.push(format!("session-{:02}-{}", session_seq, session_id));
        std::fs::create_dir_all(dir).unwrap();
    }

    let session = match Session::launch(
        vk_clone,
        session_id,
        &msg.application_id,
        &application_config,
        display_params,
        msg.permanent_gamepads,
        bug_report_dir,
    ) {
        Ok(session) => session,
        Err(err) => {
            error!(?err, "failed to launch session");
            return Err(ServerError(ErrorCode::ErrorSessionLaunchFailed, None));
        }
    };

    let id = session.id;
    ctx.state.lock().sessions.insert(id, session);

    // XXX: The protocol allows us to support superresolution here, but we don't
    // know how to downscale before encoding (yet).
    Ok(protocol::SessionLaunched {
        id,
        supported_streaming_resolutions: generate_streaming_res(&display_params),
    })
}

fn list_sessions(ctx: &Context, _msg: protocol::ListSessions) -> Result<protocol::SessionList> {
    let sessions = ctx
        .state
        .lock()
        .sessions
        .values()
        .map(|s| protocol::session_list::Session {
            application_id: s.application_id.clone(),
            session_id: s.id,
            session_start: Some(s.started.into()),
            display_params: Some(s.display_params.into()),
            supported_streaming_resolutions: generate_streaming_res(&s.display_params),
            permanent_gamepads: s.permanent_gamepads.clone(),
        })
        .collect();

    Ok(protocol::SessionList { list: sessions })
}

fn update_session(ctx: &Context, msg: protocol::UpdateSession) -> Result<protocol::SessionUpdated> {
    let display_params = validate_display_params(msg.display_params).map_err(|err| match err {
        ValidationError::Unsupported(text) => {
            ServerError(ErrorCode::ErrorSessionParamsNotSupported, Some(text))
        }
        ValidationError::Invalid(text) => ServerError(ErrorCode::ErrorProtocol, Some(text)),
    })?;

    let mut state = ctx.state.lock();
    let Some(session) = state.sessions.get_mut(&msg.session_id) else {
        return Err(ServerError(ErrorCode::ErrorSessionNotFound, None));
    };

    trace!(?session.display_params, ?display_params, "update_session");
    if session.display_params != display_params {
        if let Err(err) = session.update_display_params(display_params) {
            error!(?err, "failed to update display params");
            return Err(ServerError(
                ErrorCode::ErrorServer,
                Some("failed to update display params".to_string()),
            ));
        }
    } else {
        debug!("display params unchanged; ignoring update");
    }

    Ok(protocol::SessionUpdated {})
}

fn end_session(ctx: &Context, msg: protocol::EndSession) -> Result<protocol::SessionEnded> {
    let Some(session) = ctx.state.lock().sessions.remove(&msg.session_id) else {
        return Err(ServerError(ErrorCode::ErrorSessionNotFound, None));
    };

    if let Err(e) = session.stop() {
        error!("failed to gracefully stop session: {}", e)
    };

    Ok(protocol::SessionEnded {})
}

fn generate_streaming_res(display_params: &DisplayParams) -> Vec<protocol::Size> {
    // XXX: The protocol allows us to support superresolution here, but we don't
    // know how to downscale before encoding (yet).
    vec![protocol::Size {
        width: display_params.width,
        height: display_params.height,
    }]
}

fn read_file(p: impl AsRef<Path>, max_size: u64) -> anyhow::Result<Bytes> {
    use std::io::Read as _;

    use bytes::buf::BufMut;

    let mut r = File::open(p.as_ref())?.take(max_size + 1);
    let mut w = bytes::BytesMut::new().writer();

    match std::io::copy(&mut r, &mut w) {
        Ok(len) if len > max_size => bail!("file is bigger than maximum size"),
        Ok(0) => bail!("file is empty"),
        Ok(len) => {
            let mut buf = w.into_inner();
            Ok(buf.split_to(len as usize).freeze())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_file() -> anyhow::Result<()> {
        let zero_file = mktemp::Temp::new_file()?;
        assert_eq!("".to_string(), std::fs::read_to_string(&zero_file)?);
        assert!(read_file(&zero_file, 1024).is_err());
        drop(zero_file);

        let s = "foobar".repeat(64);
        let len = s.len() as u64;
        let big_file = mktemp::Temp::new_file()?;
        std::fs::write(&big_file, &s)?;
        assert_eq!(s, std::fs::read_to_string(&big_file)?);
        assert_eq!(s.as_bytes().to_vec(), read_file(&big_file, len)?);
        assert_eq!(s.as_bytes().to_vec(), read_file(&big_file, len + 1)?);
        assert!(read_file(&big_file, len - 1).is_err());
        drop(big_file);

        Ok(())
    }
}
