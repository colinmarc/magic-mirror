// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use prost::Message as _;

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

include!(concat!(env!("OUT_DIR"), "/_include.rs"));
pub use messages::*;

mod timestamp;

#[derive(Debug, thiserror::Error)]
enum ProtobufError {
    #[error(transparent)]
    ProtobufDecode(#[from] prost::DecodeError),
    #[error(transparent)]
    ProtobufEncode(#[from] prost::EncodeError),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProtocolError {
    #[error("protobuf encode error: {0}")]
    ProtobufEncode(#[from] prost::EncodeError),
    #[error("protobuf decode error: {0}")]
    ProtobufDecode(#[from] prost::DecodeError),
    #[error("short buffer, need {0} bytes")]
    ShortBuffer(usize),
    #[error("invalid message")]
    InvalidMessage,
    #[error("invalid message type: {0} (len={1})")]
    InvalidMessageType(u32, usize),
}

pub const MAX_MESSAGE_SIZE: usize = 65535;

// This is a very simplified version of the enum_dispatch macro.
macro_rules! message_types {
    ($($num:expr => $variant:ident),*,) => {
        /// A protocol message.
        #[repr(u32)]
        #[derive(Clone, Debug, PartialEq)]
        pub enum MessageType {
            $($variant($variant) = $num),*
        }

        impl std::fmt::Display for MessageType {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(MessageType::$variant(_) => write!(f, "{}:{}", $num, stringify!($variant))),*
                }
            }
        }

        impl MessageType {
            fn message_type(&self) -> u32 {
                match self {
                    $(MessageType::$variant(_) => $num),*
                }
            }

            fn encoded_len(&self) -> usize {
                match self {
                    $(MessageType::$variant(v) => v.encoded_len()),*
                }
            }

            fn encode<B>(&self, buf: &mut B) -> Result<(), ProtocolError>
            where
                B: bytes::BufMut,
            {
                let res = match self {
                    $(MessageType::$variant(v) => v.encode(buf)),*
                };

                res.map_err(|e| e.into())
            }

            fn decode<B: bytes::Buf>(msg_type: u32, total_len: usize, buf: B) -> Result<Self, ProtocolError> {
                match msg_type {
                    $($num => Ok($variant::decode(buf)?.into())),*,
                    _ => Err(ProtocolError::InvalidMessageType(msg_type, total_len)),
                }
            }
        }

        $(impl From<$variant> for MessageType {
            fn from(v: $variant) -> Self {
                MessageType::$variant(v)
            }
        })*
    };
}

message_types! {
    1 => Error,
    11 => ListApplications,
    12 => ApplicationList,
    13 => LaunchSession,
    14 => SessionLaunched,
    15 => UpdateSession,
    16 => SessionUpdated,
    17 => ListSessions,
    18 => SessionList,
    19 => EndSession,
    20 => SessionEnded,
    30 => Attach,
    31 => Attached,
    32 => KeepAlive,
    33 => SessionParametersChanged,
    35 => Detach,
    51 => VideoChunk,
    56 => AudioChunk,
    60 => KeyboardInput,
    61 => PointerEntered,
    62 => PointerLeft,
    63 => PointerMotion,
    64 => PointerInput,
    65 => PointerScroll,
    66 => UpdateCursor,
    67 => LockPointer,
    68 => ReleasePointer,
    69 => RelativePointerMotion,
    70 => GamepadAvailable,
    71 => GamepadUnavailable,
    72 => GamepadMotion,
    73 => GamepadInput,
}

/// Reads a header-prefixed message from a byte slice, and returns the number
/// of bytes consumed. Returns ProtocolError::ShortBuffer if the buffer
/// contains a partial message.
pub fn decode_message(buf: &[u8]) -> Result<(MessageType, usize), ProtocolError> {
    if buf.len() < 10 {
        return Err(ProtocolError::ShortBuffer(10));
    }

    let (msg_type, data_off, total_len) = {
        let mut hdr = octets::Octets::with_slice(&buf[..10]);

        let remaining = get_varint32(&mut hdr)? as usize;
        let prefix_off = hdr.off();

        let msg_type = get_varint32(&mut hdr)?;
        let off = hdr.off();

        (msg_type, off, prefix_off + remaining)
    };

    if msg_type == 0 || total_len == 0 || total_len > MAX_MESSAGE_SIZE || data_off > total_len {
        return Err(ProtocolError::InvalidMessage);
    } else if data_off > buf.len() || total_len > buf.len() {
        return Err(ProtocolError::ShortBuffer(total_len));
    }

    let padded_len = total_len.max(10);
    let msg = MessageType::decode(msg_type, padded_len, &buf[data_off..total_len])?;
    Ok((msg, padded_len))
}

/// Writes a header-prefixed message to a byte slice, and returns the number
/// of bytes used. Returns ProtocolError::ShortBuffer if the slice doesn't have
/// enough capacity.
pub fn encode_message(msg: &MessageType, buf: &mut [u8]) -> Result<usize, ProtocolError> {
    let msg_type = msg.message_type();
    let msg_len =
        u32::try_from(msg.encoded_len()).map_err(|_| ProtocolError::InvalidMessage)? as usize;

    let header_len = encode_header(msg_type, msg_len, buf)?;
    let total_len = header_len + msg_len;

    let mut msg_buf = &mut buf[header_len..];
    msg.encode(&mut msg_buf)?;

    if total_len < 10 {
        buf[total_len..].fill(0);
        Ok(10)
    } else {
        Ok(total_len)
    }
}

fn encode_header(msg_type: u32, msg_len: usize, buf: &mut [u8]) -> Result<usize, ProtocolError> {
    let msg_type_len = octets::varint_len(msg_type as u64);
    let prefix_len = octets::varint_len((msg_type_len + msg_len) as u64);
    let total_len = prefix_len + msg_type_len + msg_len;

    if total_len > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::InvalidMessage);
    } else if total_len > buf.len() || buf.len() < 10 {
        return Err(ProtocolError::ShortBuffer(std::cmp::max(total_len, 10)));
    }

    let off = {
        let mut hdr = octets::OctetsMut::with_slice(buf);
        hdr.put_varint((msg_type_len + msg_len) as u64).unwrap();
        hdr.put_varint(msg_type as u64).unwrap();
        hdr.off()
    };

    Ok(off)
}

// get_varint correctly handles u64 varints, but the protocol specifies u32.
fn get_varint32(buf: &mut octets::Octets) -> Result<u32, ProtocolError> {
    let x = match buf.get_varint() {
        Ok(x) => x,
        Err(_) => return Err(ProtocolError::InvalidMessage),
    };

    u32::try_from(x).map_err(|_| ProtocolError::InvalidMessage)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! test_roundtrip {
        ($name:ident: $value:expr) => {
            #[test]
            fn $name() {
                let msg = $value.into();
                let mut buf = [0; MAX_MESSAGE_SIZE];
                let len = encode_message(&msg, &mut buf).unwrap();
                let (decoded_msg, decoded_len) = decode_message(&buf).unwrap();
                assert_eq!(msg, decoded_msg);
                assert_eq!(len, decoded_len);
            }
        };
    }

    test_roundtrip!(test_roundtrip_detach: Detach {});

    test_roundtrip!(test_roundtrip_error: Error {
        err_code: 1,
        error_text: "test".to_string(),
    });

    test_roundtrip!(test_roundtrip_smallframe: VideoChunk {
        attachment_id: 0,
        session_id: 1,
        stream_seq: 1,
        seq: 2,
        chunk: 3,
        num_chunks: 4,
        data: bytes::Bytes::from(vec![9; 52]),
        timestamp: 1234,
    });

    test_roundtrip!(test_roundtrip_frame: VideoChunk {
        attachment_id: 0,
        session_id: 1,
        stream_seq: 1,
        seq: 2,
        chunk: 3,
        num_chunks: 4,
        data: bytes::Bytes::from(vec![9; 1200]),
        timestamp: 1234,
    });

    #[test]
    fn invalid_message_type() {
        let msg_type = 999;

        let msg_buf = [100_u8; 322];
        let msg_len = msg_buf.len();

        // Create a fake message with a msg_type of 999.
        let mut buf = [0; MAX_MESSAGE_SIZE];
        let header_len =
            encode_header(msg_type, msg_len, &mut buf).expect("failed to encode fake message");
        let total_len = header_len + msg_len;
        buf[header_len..total_len].copy_from_slice(&msg_buf);

        match decode_message(&buf) {
            Err(ProtocolError::InvalidMessageType(t, len)) => {
                assert_eq!(t, 999);
                assert_eq!(len, total_len);
            }
            v => panic!("expected InvalidMessageType, got {:?}", v),
        }
    }
}
