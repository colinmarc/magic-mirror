// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time,
};

use async_mutex::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use futures::{channel::oneshot, executor::block_on};
use mm_protocol as protocol;
use tracing::{debug, error};

mod attachment;
mod conn;
mod logging;
mod packet;
mod session;
mod stats;
mod validation;

pub mod codec;
pub mod display_params;
pub mod input;
pub mod pixel_scale;

pub use attachment::*;
pub use logging::*;
pub use packet::*;
pub use session::*;

uniffi::setup_scaffolding!();

pub use protocol::error::ErrorCode;

#[derive(Debug, Clone, thiserror::Error, uniffi::Error)]
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
    outgoing: flume::Sender<conn::OutgoingMessage>,
    roundtrips: flume::Sender<(u64, Roundtrip)>,
    attachments: flume::Sender<(u64, AttachmentState)>,
    shutdown: oneshot::Sender<()>,
}

impl ConnHandle {
    /// Signals the connection thread that it should close.
    fn close(self) -> Result<(), Option<conn::ConnError>> {
        let _ = self.shutdown.send(());
        self.waker.wake().map_err(conn::ConnError::from)?;

        if !self.thread_handle.is_finished() {
            return Ok(());
        }

        match self.thread_handle.join() {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(Some(e)),
            // The connection thread panicked.
            Err(_) => {
                error!("connection thread panicked");
                Err(None)
            }
        }
    }
}

/// Stores the current connection state.
enum ClientState {
    Connected(ConnHandle),
    Defunct(ClientError),
}

struct Roundtrip {
    tx: oneshot::Sender<Result<protocol::MessageType, ClientError>>,
    deadline: Option<time::Instant>,
}

/// Client state inside the mutex.
struct InnerClient {
    next_stream_id: u64,
    state: ClientState,
}

impl InnerClient {
    fn next_stream_id(&mut self) -> u64 {
        let sid = self.next_stream_id;
        self.next_stream_id += 4;

        sid
    }

    fn close(&mut self) -> Result<(), ClientError> {
        if let ClientState::Defunct(err) = &self.state {
            return Err(err.clone());
        }

        let ClientState::Connected(conn) =
            std::mem::replace(&mut self.state, ClientState::Defunct(ClientError::Defunct))
        else {
            unreachable!();
        };

        //Shut down the connection thread.
        let close_err = conn.close();
        if let Err(Some(e)) = &close_err {
            error!("connection error: {e:?}");
            self.state = ClientState::Defunct(e.clone().into());
        }

        match close_err {
            Ok(_) => Ok(()),
            Err(Some(e)) => Err(e.into()),
            Err(None) => Err(ClientError::Defunct),
        }
    }
}

#[derive(uniffi::Object)]
pub struct Client {
    name: String,
    addr: String,
    inner: Arc<AsyncMutex<InnerClient>>,
    stats: Arc<stats::StatsCollector>,
}

impl Client {
    async fn reconnect(&self) -> Result<AsyncMutexGuard<InnerClient>, ClientError> {
        let inner_clone = self.inner.clone();
        let mut guard = self.inner.lock().await;

        match &guard.state {
            ClientState::Connected(_) => (),
            ClientState::Defunct(ClientError::ConnectionError(conn::ConnError::Idle)) => {
                // Reconnect after an idle timeout.
                let conn = match spawn_conn(&self.addr, inner_clone, self.stats.clone()).await {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("connection failed: {e:#}");
                        return Err(e);
                    }
                };

                guard.state = ClientState::Connected(conn);

                debug!("reconnected after idle timeout");
            }
            ClientState::Defunct(e) => {
                return Err(e.clone());
            }
        }

        Ok(guard)
    }

    async fn initiate_stream(
        &self,
        msg: impl Into<protocol::MessageType>,
        fin: bool,
        timeout: Option<time::Duration>,
    ) -> Result<(u64, protocol::MessageType), ClientError> {
        let mut guard = self.reconnect().await?;

        let sid = guard.next_stream_id();
        let (oneshot_tx, oneshot_rx) = oneshot::channel();

        let ConnHandle {
            waker,
            outgoing,
            roundtrips,
            ..
        } = match &guard.state {
            ClientState::Connected(conn) => conn,
            ClientState::Defunct(err) => return Err(err.clone()),
        };

        if outgoing
            .send(conn::OutgoingMessage {
                sid,
                msg: msg.into(),
                fin,
            })
            .is_err()
        {
            match guard.close() {
                Ok(_) => return Err(ClientError::Defunct),
                Err(e) => return Err(e),
            }
        }

        let deadline = timeout.map(|d| time::Instant::now() + d);
        if roundtrips
            .send_async((
                sid,
                Roundtrip {
                    tx: oneshot_tx,
                    deadline,
                },
            ))
            .await
            .is_err()
        {
            match guard.close() {
                Ok(_) => return Err(ClientError::Defunct),
                Err(e) => return Err(e),
            }
        };

        waker.wake().map_err(conn::ConnError::from)?;

        // We don't want to hold the mutex while waiting for a response.
        drop(guard);

        let res = oneshot_rx.await??;
        Ok((sid, res))
    }

    async fn roundtrip(
        &self,
        msg: impl Into<protocol::MessageType>,
        timeout: time::Duration,
    ) -> Result<protocol::MessageType, ClientError> {
        let (_, msg) = self.initiate_stream(msg, false, Some(timeout)).await?;
        Ok(msg)
    }
}

#[uniffi::export]
impl Client {
    #[uniffi::constructor]
    pub async fn new(addr: &str, client_name: &str) -> Result<Self, ClientError> {
        let inner = Arc::new(AsyncMutex::new(InnerClient {
            next_stream_id: 0,
            state: ClientState::Defunct(ClientError::Defunct),
        }));

        let stats = Arc::new(stats::StatsCollector::default());
        let conn = spawn_conn(addr, inner.clone(), stats.clone()).await?;
        inner.lock().await.state = ClientState::Connected(conn);

        Ok(Self {
            name: client_name.to_owned(),
            addr: addr.to_owned(),
            inner,
            stats,
        })
    }

    pub fn stats(&self) -> stats::ClientStats {
        self.stats.snapshot()
    }

    pub async fn list_applications(
        &self,
        timeout: time::Duration,
    ) -> Result<Vec<Application>, ClientError> {
        let res = match self
            .roundtrip(protocol::ListApplications {}, timeout)
            .await?
        {
            protocol::MessageType::ApplicationList(res) => res,
            protocol::MessageType::Error(e) => return Err(ClientError::ServerError(e)),
            msg => return Err(ClientError::UnexpectedMessage(msg)),
        };

        Ok(res
            .list
            .into_iter()
            .map(Application::try_from)
            .collect::<Result<Vec<_>, validation::ValidationError>>()?)
    }

    pub async fn fetch_application_image(
        &self,
        application_id: String,
        format: session::ApplicationImageFormat,
        timeout: time::Duration,
    ) -> Result<Vec<u8>, ClientError> {
        let fetch = protocol::FetchApplicationImage {
            format: format.into(),
            application_id,
        };

        match self.roundtrip(fetch, timeout).await? {
            protocol::MessageType::ApplicationImage(res) => Ok(res.image_data.into()),
            protocol::MessageType::Error(e) => Err(ClientError::ServerError(e)),
            msg => Err(ClientError::UnexpectedMessage(msg)),
        }
    }

    pub async fn list_sessions(
        &self,
        timeout: time::Duration,
    ) -> Result<Vec<Session>, ClientError> {
        let res = match self.roundtrip(protocol::ListSessions {}, timeout).await? {
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
        application_id: String,
        display_params: display_params::DisplayParams,
        permanent_gamepads: Vec<input::Gamepad>,
        timeout: time::Duration,
    ) -> Result<Session, ClientError> {
        let msg = protocol::LaunchSession {
            application_id: application_id.clone(),
            display_params: Some(display_params.clone().into()),
            permanent_gamepads: permanent_gamepads.iter().map(|pad| (*pad).into()).collect(),
        };

        let res = match self.roundtrip(msg, timeout).await? {
            protocol::MessageType::SessionLaunched(msg) => msg,
            protocol::MessageType::Error(e) => return Err(ClientError::ServerError(e)),
            msg => return Err(ClientError::UnexpectedMessage(msg)),
        };

        Ok(Session {
            id: res.id,
            start: time::SystemTime::now(),
            application_id,
            display_params,
        })
    }

    pub async fn end_session(&self, id: u64, timeout: time::Duration) -> Result<(), ClientError> {
        let msg = protocol::EndSession { session_id: id };
        match self.roundtrip(msg, timeout).await? {
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
        let msg = protocol::UpdateSession {
            session_id: id,
            display_params: Some(params.into()),
        };

        match self.roundtrip(msg, timeout).await? {
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
        let channel_conf = if config.channels.is_empty() {
            None
        } else {
            Some(protocol::AudioChannels {
                channels: config.channels.iter().copied().map(Into::into).collect(),
            })
        };

        let attach = protocol::Attach {
            session_id,
            client_name: self.name.clone(),
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

        let (sid, res) = self.initiate_stream(attach, false, Some(timeout)).await?;

        let attached = match res {
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
        .await
    }
}

async fn spawn_conn(
    addr: &str,
    client: Arc<AsyncMutex<InnerClient>>,
    stats: Arc<stats::StatsCollector>,
) -> Result<ConnHandle, ClientError> {
    let (incoming_tx, incoming_rx) = flume::unbounded();
    let (outgoing_tx, outgoing_rx) = flume::unbounded();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    // Rendezvous channels for synchronized state.
    let (roundtrips_tx, roundtrips_rx) = flume::bounded(0);
    let (attachments_tx, attachments_rx) = flume::bounded(0);

    let mut conn = conn::Conn::new(addr, incoming_tx, outgoing_rx, ready_tx, shutdown_rx, stats)?;
    let waker = conn.waker();

    // Spawn a polling loop for the quic connection.
    let thread_handle = std::thread::Builder::new()
        .name("QUIC conn".to_string())
        .spawn(move || conn.run())
        .unwrap();

    // Spawn a second thread to fulfill request/response futures and drive
    // the attachment delegates.

    let outgoing_clone = outgoing_tx.clone();
    let waker_clone = waker.clone();
    let _ = std::thread::Builder::new()
        .name("mmclient reactor".to_string())
        .spawn(move || {
            conn_reactor(
                incoming_rx,
                outgoing_clone,
                waker_clone,
                roundtrips_rx,
                attachments_rx,
                client,
            )
        })
        .unwrap();

    if ready_rx.await.is_err() {
        // An error occured while spinning up.
        match thread_handle.join() {
            Ok(Ok(_)) | Err(_) => return Err(ClientError::Defunct),
            Ok(Err(e)) => return Err(e.into()),
        }
    }

    Ok(ConnHandle {
        thread_handle,
        waker,
        outgoing: outgoing_tx,
        shutdown: shutdown_tx,
        roundtrips: roundtrips_tx,
        attachments: attachments_tx,
    })
}

#[derive(Default)]
struct InFlight {
    roundtrips: HashMap<u64, Roundtrip>,
    attachments: HashMap<u64, AttachmentState>,
    prev_attachments: HashSet<u64>, // By attachment ID.
}

fn conn_reactor(
    incoming: flume::Receiver<conn::ConnEvent>,
    outgoing: flume::Sender<conn::OutgoingMessage>,
    conn_waker: Arc<mio::Waker>,
    roundtrips: flume::Receiver<(u64, Roundtrip)>,
    attachments: flume::Receiver<(u64, AttachmentState)>,
    client: Arc<AsyncMutex<InnerClient>>,
) {
    let mut in_flight = InFlight::default();
    let mut deadline = time::Instant::now() + time::Duration::from_secs(1);

    loop {
        let now = time::Instant::now();
        if deadline < now {
            deadline = now + time::Duration::from_secs(1);

            // Check roundtrip deadlines.
            let mut timed_out = Vec::new();
            for (sid, Roundtrip { deadline, .. }) in in_flight.roundtrips.iter() {
                if deadline.is_some_and(|dl| now >= dl) {
                    timed_out.push(*sid);
                }
            }

            // Fulfill the futures with an error.
            for id in &timed_out {
                let Roundtrip { tx, .. } = in_flight.roundtrips.remove(id).unwrap();
                let _ = tx.send(Err(ClientError::RequestTimeout));
            }

            // Send keepalives.
            if !in_flight.attachments.is_empty() {
                for (sid, _) in in_flight.attachments.iter() {
                    let _ = outgoing.send(conn::OutgoingMessage {
                        sid: *sid,
                        msg: protocol::KeepAlive {}.into(),
                        fin: false,
                    });
                }

                let _ = conn_waker.wake();
            }
        }

        enum SelectResult {
            RecvError,
            InsertRoundtrip(u64, Roundtrip),
            InsertAttachment(u64, AttachmentState),
            Incoming(conn::ConnEvent),
        }

        let res = flume::select::Selector::new()
            .recv(&roundtrips, |ev| {
                if let Ok((sid, rt)) = ev {
                    SelectResult::InsertRoundtrip(sid, rt)
                } else {
                    SelectResult::RecvError
                }
            })
            .recv(&attachments, |ev| {
                if let Ok((sid, att)) = ev {
                    SelectResult::InsertAttachment(sid, att)
                } else {
                    SelectResult::RecvError
                }
            })
            .recv(&incoming, |ev| {
                if let Ok(ev) = ev {
                    SelectResult::Incoming(ev)
                } else {
                    SelectResult::RecvError
                }
            })
            .wait_deadline(deadline);

        match res {
            Err(flume::select::SelectError::Timeout) => continue,
            Ok(SelectResult::RecvError) => break,
            Ok(SelectResult::InsertRoundtrip(sid, rt)) => {
                in_flight.roundtrips.insert(sid, rt);
            }
            Ok(SelectResult::InsertAttachment(sid, att)) => {
                in_flight.attachments.insert(sid, att);
            }
            Ok(SelectResult::Incoming(ev)) => conn_reactor_handle_incoming(&mut in_flight, ev),
        };
    }

    // The client is probably already closed, but we should make sure, since
    // this thread is the only one notified if the connection thread died.
    let mut guard = block_on(client.lock());
    let stream_err = match guard.close() {
        Err(e) => Some(e.clone()),
        Ok(_) => None,
    };

    for (_, att) in in_flight.attachments.drain() {
        att.handle_close(stream_err.clone());
    }

    in_flight.roundtrips.clear(); // Cancels the futures.
}

fn conn_reactor_handle_incoming(in_flight: &mut InFlight, ev: conn::ConnEvent) {
    match ev {
        conn::ConnEvent::StreamMessage(sid, msg) => {
            if let Some(attachment) = in_flight.attachments.get_mut(&sid) {
                attachment.handle_message(msg);
                return;
            }

            if let Some(Roundtrip { tx, .. }) = in_flight.roundtrips.remove(&sid) {
                let _ = tx.send(Ok(msg));
            }
        }
        conn::ConnEvent::Datagram(msg) => {
            let (session_id, attachment_id) = match &msg {
                protocol::MessageType::VideoChunk(chunk) => (chunk.session_id, chunk.attachment_id),
                protocol::MessageType::AudioChunk(chunk) => (chunk.session_id, chunk.attachment_id),
                msg => {
                    error!("unexpected {} as datagram", msg);
                    return;
                }
            };

            // Find the relevant attachment. The session ID and attachment
            // may be omitted if there's only one attachment.
            let attachment = match (session_id, attachment_id) {
                (0, 0) if in_flight.attachments.len() == 1 => {
                    in_flight.attachments.iter_mut().next()
                }
                (0, _) | (_, 0) => None, // This is invalid.
                (s, a) => in_flight
                    .attachments
                    .iter_mut()
                    .find(|(_, att)| att.session_id == s && att.attachment_id == a),
            };

            if let Some((_, attachment)) = attachment {
                attachment.handle_message(msg);
            } else if !in_flight.prev_attachments.contains(&attachment_id) {
                error!(
                    session_id,
                    attachment_id, "failed to match datagram to attachment"
                );
            }
        }
        conn::ConnEvent::StreamClosed(sid) => {
            in_flight.roundtrips.remove(&sid);
            if let Some(attachment) = in_flight.attachments.remove(&sid) {
                in_flight.prev_attachments.insert(attachment.attachment_id);
                attachment.handle_close(None);
            }
        }
    }
}
