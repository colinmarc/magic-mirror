use std::sync::Arc;

use mm_protocol as protocol;

uniffi::setup_scaffolding!();

#[derive(uniffi::Error)]
#[derive(Debug, Clone, thiserror::Error)]
pub enum ClientError {
    #[error("foobar")]
    Foo,
}

#[derive(uniffi::Object)]
pub struct Client {
    // todo
}

#[derive(uniffi::Record)]
pub struct Session {
    // todo
}

#[derive(uniffi::Record)]
pub struct VirtualDisplayParameters {
    // todo
}


#[uniffi::export]
impl Client {
    #[uniffi::constructor]
    pub fn connect() -> Result<Self, ClientError> {
        todo!()
    }

    async fn fetch_sessions(
        &self,
    ) -> Result<Vec<Session>, ClientError> {
        todo!()
    }

    async fn launch_session(
        &self,
        name: &str,
        display_params: VirtualDisplayParameters,
    ) -> Result<u64, ClientError> {
        todo!()
    }

    async fn update_session_display_params(
        &self,
        id: u64,
        display_params: VirtualDisplayParameters,
    ) -> Result<(), ClientError> {
        todo!()
    }

    async fn kill_session(&self, id: u64) -> Result<(), ClientError> {
        todo!()
    }

    async fn attach_session(
        &self,
        id: u64,
        handler: Arc<dyn AttachmentHandler>,
    ) -> Result<Attachment, ClientError> {
        todo!()
    }
}

#[derive(uniffi::Object)]
pub struct Attachment {
    // todo
}

impl Attachment {
    /// Sets the desired resolution and scale. The client will automatically
    /// debounce and then update the session to match.
    fn set_desired_resolution(width: u32, height: u32, scale: f32) -> Result<(), ClientError> {
        todo!()
    }

    fn set_desired_codec(codec: protocol::VideoCodec) -> Result<(), ClientError> {
        todo!()
    }
}

#[derive(uniffi::Record)]
pub struct Packet {
    pub sid: u64,
    pub seq: u64,
    pub data: Vec<u8>,
}

/// The main delegate trait. Client applications should implement this trait to
/// receive callbacks for interactions with the server.
#[uniffi::export(with_foreign)]
pub trait AttachmentHandler: Send + Sync {
    /// Called when the attachment ends (but not when transiently reattaching).
    fn on_disconnect(&self);

    /// Called when the client begins a reattachment operation, usually because the desired streaming parameters changed.
    fn on_reattach_started(&self);

    /// Called when a reattachment operation finishes.
    fn on_reattach_finished(&self);

    /// Called when a video packet is available.
    fn on_video_packet(&self, packet: Packet);

    /// Called when an audio packet is available.
    fn on_audio_packet(&self, packet: Packet);
}
