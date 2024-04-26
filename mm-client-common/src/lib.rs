// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time,
};

use crossbeam_channel as crossbeam;
use futures::channel::oneshot;
use mm_protocol as protocol;
use tracing::error;

mod attachment;
mod conn;
mod packet;
mod session;
mod validation;

pub mod codec;
pub mod display_params;
pub mod input;
pub mod pixel_scale;

pub use attachment::*;
pub use packet::Packet;
pub use session::*;

uniffi::setup_scaffolding!();

pub use protocol::error::ErrorCode;

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ClientError {
    #[error("protocol error")]
    ProtocolError(#[from] protocol::ProtocolError),
    #[error("{}: {}", .0.err_code().as_str_name(), .0.error_text)]
    ServerError(protocol::Error),
    #[error("request timed out")]
    RequestTimeout,
    #[error("connection error")]
    ConnectionError(#[from] conn::ConnError),
    #[error("stream closed before request could be received")]
    Canceled(#[from] oneshot::Canceled),
    #[error("received unexpected message: {0}")]
    UnexpectedMessage(protocol::MessageType),
    #[error("message validation failed")]
    ValidationFailed(#[from] validation::ValidationError),
    #[error("client defunct")]
    Defunct,
    #[error("attachment ended")]
    Detached,
}

/// A handle for the QUIC connection thread, used to push outgoing messages.
struct ConnHandle {
    thread_handle: std::thread::JoinHandle<Result<(), conn::ConnError>>,
    waker: Arc<mio::Waker>,
    outgoing_tx: crossbeam::Sender<conn::OutgoingMessage>,
}

/// The result of a simple request/response interaction with the server.
type RoundtripResult = Result<protocol::MessageType, ClientError>;

struct Roundtrip {
    tx: oneshot::Sender<RoundtripResult>,
    deadline: Option<time::Instant>,
}

/// Client state inside the mutex.
struct InnerClient {
    client_name: String,
    next_stream_id: u64,
    conn_handle: Option<ConnHandle>,
    futures: HashMap<u64, Roundtrip>,
    attachments: HashMap<u64, AttachmentState>,
}

impl InnerClient {
    fn next_stream_id(&mut self) -> u64 {
        let sid = self.next_stream_id;
        self.next_stream_id += 4;

        sid
    }

    fn future(
        &mut self,
        sid: u64,
        msg: impl Into<protocol::MessageType>,
        fin: bool,
        deadline: Option<time::Instant>,
    ) -> Result<oneshot::Receiver<RoundtripResult>, ClientError> {
        let (oneshot_tx, oneshot_rx) = oneshot::channel();

        let Some(ConnHandle {
            waker, outgoing_tx, ..
        }) = &self.conn_handle
        else {
            return Err(ClientError::Defunct);
        };

        if outgoing_tx
            .send(conn::OutgoingMessage {
                sid,
                msg: msg.into(),
                fin,
            })
            .is_err()
        {
            match self.close() {
                Ok(_) => return Err(ClientError::Defunct),
                Err(e) => return Err(e),
            }
        }

        self.futures.insert(
            sid,
            Roundtrip {
                tx: oneshot_tx,
                deadline,
            },
        );

        waker.wake().map_err(conn::ConnError::from)?;
        Ok(oneshot_rx)
    }

    fn roundtrip(
        &mut self,
        msg: impl Into<protocol::MessageType>,
        deadline: Option<time::Instant>,
    ) -> Result<oneshot::Receiver<RoundtripResult>, ClientError> {
        let sid = self.next_stream_id();
        self.future(sid, msg, true, deadline)
    }

    fn close(&mut self) -> Result<(), ClientError> {
        let Some(ConnHandle {
            thread_handle,
            waker,
            outgoing_tx,
        }) = self.conn_handle.take()
        else {
            return Err(ClientError::Defunct);
        };

        drop(outgoing_tx);

        waker.wake().map_err(conn::ConnError::from)?;

        match thread_handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err(ClientError::Defunct),
        }
    }
}

#[derive(uniffi::Object)]
pub struct Client {
    inner: Arc<Mutex<InnerClient>>,
}

#[uniffi::export]
impl Client {
    #[uniffi::constructor]
    pub async fn new(
        addr: &str,
        client_name: &str,
        connect_timeout: time::Duration,
    ) -> Result<Self, ClientError> {
        let (incoming_tx, incoming_rx) = crossbeam::unbounded();
        let (outgoing_tx, outgoing_rx) = crossbeam::unbounded();
        let (ready_tx, ready_rx) = oneshot::channel();

        let mut conn = conn::Conn::new(addr, incoming_tx, outgoing_rx, ready_tx, connect_timeout)?;
        let waker = conn.waker();

        // Spawn a polling loop for the quic connection.
        let thread_handle = std::thread::Builder::new()
            .name("QUIC conn".to_string())
            .spawn(move || conn.run())
            .unwrap();

        let inner = Arc::new(Mutex::new(InnerClient {
            client_name: client_name.to_string(),
            next_stream_id: 0,
            conn_handle: Some(ConnHandle {
                thread_handle,
                waker,
                outgoing_tx,
            }),
            futures: HashMap::new(),
            attachments: HashMap::new(),
        }));

        // Spawn a second thread to fulfill request/response futures and drive
        // the attachment delegates.
        let inner_clone = inner.clone();
        let _ = std::thread::Builder::new()
            .name("mmclient reactor".to_string())
            .spawn(move || conn_reactor(incoming_rx, inner_clone))
            .unwrap();

        match ready_rx.await {
            Ok(Ok(())) => Ok(Self { inner }),
            Ok(Err(e)) => Err(e.into()),
            Err(oneshot::Canceled) => {
                // Should only happen if the connection thread errored while
                // spinning up.
                match inner.lock().unwrap().close() {
                    Ok(_) => Err(ClientError::Defunct),
                    Err(e) => Err(e),
                }
            }
        }
    }

    pub async fn list_sessions(
        &self,
        timeout: time::Duration,
    ) -> Result<Vec<Session>, ClientError> {
        let fut = self.inner.lock().unwrap().roundtrip(
            protocol::ListSessions {},
            Some(time::Instant::now() + timeout),
        )?;

        let res = match fut.await?? {
            protocol::MessageType::SessionList(res) => res,
            protocol::MessageType::Error(e) => return Err(ClientError::ServerError(e)),
            msg => return Err(ClientError::UnexpectedMessage(msg)),
        };

        Ok(res
            .list
            .into_iter()
            .map(Session::try_from)
            .collect::<Result<Vec<_>, validation::ValidationError>>()?)
    }

    pub async fn launch_session(
        &self,
        application_name: String,
        display_params: display_params::DisplayParams,
        timeout: time::Duration,
    ) -> Result<Session, ClientError> {
        let fut = self.inner.lock().unwrap().roundtrip(
            protocol::LaunchSession {
                application_name: application_name.clone(),
                display_params: Some(display_params.clone().into()),
            },
            Some(time::Instant::now() + timeout),
        )?;

        let mut session = Session {
            id: 0,
            start: time::SystemTime::UNIX_EPOCH,
            application_name,
            display_params,
        };

        let res = match fut.await?? {
            protocol::MessageType::SessionLaunched(msg) => msg,
            protocol::MessageType::Error(e) => return Err(ClientError::ServerError(e)),
            msg => return Err(ClientError::UnexpectedMessage(msg)),
        };

        session.id = res.id;
        session.start = time::SystemTime::now();
        Ok(session)
    }

    pub async fn end_session(&self, id: u64, timeout: time::Duration) -> Result<(), ClientError> {
        let fut = self.inner.lock().unwrap().roundtrip(
            protocol::EndSession { session_id: id },
            Some(time::Instant::now() + timeout),
        )?;

        match fut.await?? {
            protocol::MessageType::SessionEnded(_) => Ok(()),
            protocol::MessageType::Error(e) => Err(ClientError::ServerError(e)),
            msg => Err(ClientError::UnexpectedMessage(msg)),
        }
    }

    pub async fn update_session_display_params(
        &self,
        id: u64,
        params: display_params::DisplayParams,
        timeout: time::Duration,
    ) -> Result<(), ClientError> {
        let fut = self.inner.lock().unwrap().roundtrip(
            protocol::UpdateSession {
                session_id: id,
                display_params: Some(params.into()),
            },
            Some(time::Instant::now() + timeout),
        )?;

        match fut.await?? {
            protocol::MessageType::SessionUpdated(_) => Ok(()),
            protocol::MessageType::Error(e) => Err(ClientError::ServerError(e)),
            msg => Err(ClientError::UnexpectedMessage(msg)),
        }
    }

    /// Attach to a session. The timeout parameter is used for the duration of
    /// the initial request, i.e. until an Attached message is returned by the
    /// server.
    pub async fn attach_session(
        &self,
        session_id: u64,
        config: AttachmentConfig,
        delegate: Arc<dyn AttachmentDelegate>,
        timeout: time::Duration,
    ) -> Result<Attachment, ClientError> {
        // Send an attach message using the roundtrip mechanism, but the leave
        // the stream open.
        let (sid, fut) = {
            let mut guard = self.inner.lock().unwrap();
            let sid = guard.next_stream_id();

            let channel_conf = if config.channels.is_empty() {
                None
            } else {
                Some(protocol::AudioChannels {
                    channels: config.channels.iter().copied().map(Into::into).collect(),
                })
            };

            let attach = protocol::Attach {
                session_id,
                client_name: guard.client_name.clone(),
                attachment_type: protocol::AttachmentType::Operator.into(),
                video_codec: config.video_codec.unwrap_or_default().into(),
                streaming_resolution: Some(protocol::Size {
                    width: config.width,
                    height: config.height,
                }),
                video_profile: config.video_profile.unwrap_or_default().into(),
                quality_preset: config.quality_preset.unwrap_or_default(),

                audio_codec: config.audio_codec.unwrap_or_default().into(),
                sample_rate_hz: config.sample_rate.unwrap_or_default(),
                channels: channel_conf,
            };

            (
                sid,
                guard.future(sid, attach, false, Some(time::Instant::now() + timeout))?,
            )
        };

        let attached = match fut.await?? {
            protocol::MessageType::Attached(att) => att,
            protocol::MessageType::Error(e) => return Err(ClientError::ServerError(e)),
            msg => return Err(ClientError::UnexpectedMessage(msg)),
        };

        Attachment::new(
            sid,
            self.inner.clone(),
            attached,
            delegate,
            config.video_stream_seq_offset,
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let mut state = self.inner.lock().unwrap();
        if let Err(e) = state.close() {
            error!("error closing connection: {:#}", e);
        }
    }
}

fn conn_reactor(incoming: crossbeam::Receiver<conn::ConnEvent>, client: Arc<Mutex<InnerClient>>) {
    loop {
        let ev = match incoming.recv_timeout(time::Duration::from_secs(1)) {
            Ok(v) => v,
            Err(crossbeam::RecvTimeoutError::Disconnected) => {
                // The connection thread died. Cancel all pending futures.
                let mut guard = client.lock().unwrap();

                guard.futures.clear();
                guard.attachments.clear();

                match guard.close() {
                    Err(ClientError::Defunct) => (), // The error has been reported elsewhere.
                    Err(e) => error!("connection thread died: {:?}", e),
                    Ok(_) => error!("connection thread died"),
                }

                return;
            }
            Err(crossbeam::RecvTimeoutError::Timeout) => {
                // Check deadlines.
                let now = time::Instant::now();
                let mut guard = client.lock().unwrap();

                let mut timed_out = Vec::new();
                for (sid, Roundtrip { deadline, .. }) in guard.futures.iter() {
                    if deadline.is_some_and(|dl| now >= dl) {
                        timed_out.push(*sid);
                    }
                }

                // Fulfill the futures with an error.
                for id in &timed_out {
                    let Roundtrip { tx, .. } = guard.futures.remove(id).unwrap();
                    let _ = tx.send(Err(ClientError::RequestTimeout));
                }

                // Send keepalives.
                if !guard.attachments.is_empty() {
                    let Some(ConnHandle {
                        waker, outgoing_tx, ..
                    }) = &guard.conn_handle
                    else {
                        return;
                    };

                    for (sid, _) in guard.attachments.iter() {
                        let _ = outgoing_tx.send(conn::OutgoingMessage {
                            sid: *sid,
                            msg: protocol::KeepAlive {}.into(),
                            fin: false,
                        });
                    }

                    let _ = waker.wake();
                }

                continue;
            }
        };

        let mut guard = client.lock().unwrap();

        match ev {
            conn::ConnEvent::StreamMessage(sid, msg) => {
                if let Some(attachment) = guard.attachments.get_mut(&sid) {
                    attachment.handle_message(msg);
                    continue;
                }

                if let Some(Roundtrip { tx, .. }) = guard.futures.remove(&sid) {
                    let _ = tx.send(Ok(msg));
                }
            }
            conn::ConnEvent::Datagram(msg) => {
                let (session_id, attachment_id) = match &msg {
                    protocol::MessageType::VideoChunk(chunk) => {
                        (chunk.session_id, chunk.attachment_id)
                    }
                    protocol::MessageType::AudioChunk(chunk) => {
                        (chunk.session_id, chunk.attachment_id)
                    }
                    msg => {
                        error!("unexpected {} as datagram", msg);
                        continue;
                    }
                };

                // Find the relevant attachment. The session ID and attachment
                // may be omitted if there's only one attachment.
                let attachment = match (session_id, attachment_id) {
                    (0, 0) if guard.attachments.len() == 1 => guard.attachments.iter_mut().next(),
                    (0, _) | (_, 0) => None,
                    (s, a) => guard
                        .attachments
                        .iter_mut()
                        .find(|(_, att)| att.session_id == s && att.attachment_id == a),
                };

                if let Some((_, attachment)) = attachment {
                    attachment.handle_message(msg);
                } else {
                    error!(
                        session_id,
                        attachment_id, "failed to match datagram to attachment"
                    );
                }
            }
            conn::ConnEvent::StreamClosed(sid) => {
                guard.futures.remove(&sid);
                if let Some(attachment) = guard.attachments.remove(&sid) {
                    attachment.handle_fin();
                }
            }
        }
    }
}
