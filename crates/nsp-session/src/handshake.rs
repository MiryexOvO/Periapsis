//! Transport-independent offer/accept/ack state machine.
//! 与传输实现无关的双向协商状态机；身份认证由连接层负责。

use crate::{negotiate, validate};
use nsp_types::{Agreement, Hello, NegotiationError};
use std::fmt;

pub const MOQT_VERSION: &str = "moq-transport-22";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    TransportPending,
    MoqReady,
    AwaitingOffer,
    AwaitingAccept,
    AwaitingAck,
    AckPending,
    Active,
    Rejected,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Offer {
        session_id: String,
        hello: Hello,
    },
    Accept {
        session_id: String,
        hello: Hello,
        agreement: Agreement,
    },
    Ack {
        session_id: String,
    },
    Reject {
        session_id: String,
    },
}

impl Message {
    fn session_id(&self) -> &str {
        match self {
            Self::Offer { session_id, .. }
            | Self::Accept { session_id, .. }
            | Self::Ack { session_id }
            | Self::Reject { session_id } => session_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandshakeError {
    InvalidSessionId,
    ProtocolVersion,
    UnexpectedMessage,
    SessionMismatch,
    AgreementMismatch,
    PeerRejected,
    Negotiation(NegotiationError),
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NSP handshake: {self:?}")
    }
}
impl std::error::Error for HandshakeError {}

/// Enforce handshake ordering before exposing an active agreement.
/// 强制执行协商顺序，仅在会话进入 ACTIVE 后提供可用的协商结果。
pub struct Handshake {
    role: Role,
    state: State,
    session_id: String,
    local: Hello,
    agreement: Option<Agreement>,
}

impl Handshake {
    pub fn new(role: Role, session_id: String, local: Hello) -> Result<Self, HandshakeError> {
        if session_id.len() != 32
            || !session_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(HandshakeError::InvalidSessionId);
        }
        validate(&local).map_err(HandshakeError::Negotiation)?;
        Ok(Self {
            role,
            state: State::TransportPending,
            session_id,
            local,
            agreement: None,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn agreement(&self) -> Option<&Agreement> {
        (self.state == State::Active)
            .then_some(self.agreement.as_ref())
            .flatten()
    }

    pub fn transport_ready(&mut self, version: &str) -> Result<(), HandshakeError> {
        if self.state != State::TransportPending {
            return self.fail(HandshakeError::UnexpectedMessage);
        }
        if version != MOQT_VERSION {
            return self.fail(HandshakeError::ProtocolVersion);
        }
        self.state = State::MoqReady;
        Ok(())
    }

    pub fn start(&mut self) -> Result<Option<Message>, HandshakeError> {
        if self.state != State::MoqReady {
            return self.fail(HandshakeError::UnexpectedMessage);
        }
        match self.role {
            Role::Initiator => {
                self.state = State::AwaitingAccept;
                Ok(Some(Message::Offer {
                    session_id: self.session_id.clone(),
                    hello: self.local.clone(),
                }))
            }
            Role::Responder => {
                self.state = State::AwaitingOffer;
                Ok(None)
            }
        }
    }

    pub fn receive(&mut self, message: Message) -> Result<Option<Message>, HandshakeError> {
        if matches!(
            self.state,
            State::TransportPending
                | State::MoqReady
                | State::Active
                | State::AckPending
                | State::Rejected
                | State::Closed
        ) {
            return self.fail(HandshakeError::UnexpectedMessage);
        }
        if message.session_id() != self.session_id {
            return self.fail(HandshakeError::SessionMismatch);
        }
        let result = match (self.state, message) {
            (_, Message::Reject { .. }) => Err(HandshakeError::PeerRejected),
            (State::AwaitingOffer, Message::Offer { hello, .. }) => {
                match negotiate(&self.local, &hello) {
                    Ok(agreement) => {
                        self.agreement = Some(agreement.clone());
                        self.state = State::AwaitingAck;
                        Ok(Some(Message::Accept {
                            session_id: self.session_id.clone(),
                            hello: self.local.clone(),
                            agreement,
                        }))
                    }
                    Err(error) => Err(HandshakeError::Negotiation(error)),
                }
            }
            (
                State::AwaitingAccept,
                Message::Accept {
                    hello, agreement, ..
                },
            ) => match negotiate(&hello, &self.local) {
                Ok(expected) if expected == agreement => {
                    self.agreement = Some(agreement);
                    self.state = State::AckPending;
                    Ok(Some(Message::Ack {
                        session_id: self.session_id.clone(),
                    }))
                }
                Ok(_) => Err(HandshakeError::AgreementMismatch),
                Err(error) => Err(HandshakeError::Negotiation(error)),
            },
            (State::AwaitingAck, Message::Ack { .. }) => {
                self.state = State::Active;
                Ok(None)
            }
            _ => Err(HandshakeError::UnexpectedMessage),
        };
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error),
        }
    }

    /// Commit activation once the ACK is handed to the transport.
    /// ACK 成功交给传输层后，发起端才能启用协商结果。
    pub fn ack_sent(&mut self) -> Result<(), HandshakeError> {
        if self.state != State::AckPending {
            return self.fail(HandshakeError::UnexpectedMessage);
        }
        self.state = State::Active;
        Ok(())
    }

    /// Invalidate negotiation when the transport closes or a local deadline expires.
    /// 传输关闭或本地超时后使协商结果失效，不能继续用旧状态发送媒体。
    pub fn close(&mut self) {
        self.state = State::Closed;
        self.agreement = None;
    }

    fn fail<T>(&mut self, error: HandshakeError) -> Result<T, HandshakeError> {
        if self.state != State::Closed {
            self.state = State::Rejected;
        }
        self.agreement = None;
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsp_types::{NSP_VERSION, Profile};
    fn hello() -> Hello {
        Hello {
            version: NSP_VERSION,
            profiles: vec![Profile::Live],
            codecs: vec!["application/octet-stream".into()],
            extensions: vec![],
        }
    }
    fn endpoint(role: Role) -> Handshake {
        let mut endpoint =
            Handshake::new(role, "0123456789abcdef0123456789abcdef".into(), hello()).unwrap();
        endpoint.transport_ready(MOQT_VERSION).unwrap();
        endpoint
    }

    #[test]
    fn responder_requires_ack_before_activation() {
        let mut initiator = endpoint(Role::Initiator);
        let mut responder = endpoint(Role::Responder);
        responder.start().unwrap();
        let offer = initiator.start().unwrap().unwrap();
        let accept = responder.receive(offer).unwrap().unwrap();
        assert_eq!(responder.state(), State::AwaitingAck);
        assert!(responder.agreement().is_none());
        let ack = initiator.receive(accept).unwrap().unwrap();
        assert_eq!(initiator.state(), State::AckPending);
        assert!(initiator.agreement().is_none());
        initiator.ack_sent().unwrap();
        responder.receive(ack).unwrap();
        assert_eq!(initiator.state(), State::Active);
        assert_eq!(responder.state(), State::Active);
        assert_eq!(initiator.agreement(), responder.agreement());
    }

    #[test]
    fn fabricated_agreement_is_rejected() {
        let mut initiator = endpoint(Role::Initiator);
        let mut responder = endpoint(Role::Responder);
        responder.start().unwrap();
        let offer = initiator.start().unwrap().unwrap();
        let mut accept = responder.receive(offer).unwrap().unwrap();
        if let Message::Accept { agreement, .. } = &mut accept {
            agreement.buffer.maximum_ms += 1;
        }
        assert_eq!(
            initiator.receive(accept),
            Err(HandshakeError::AgreementMismatch)
        );
        assert_eq!(initiator.state(), State::Rejected);
        assert!(initiator.agreement().is_none());
    }

    #[test]
    fn wrong_session_and_replayed_ack_fail_closed() {
        let mut responder = endpoint(Role::Responder);
        responder.start().unwrap();
        assert_eq!(
            responder.receive(Message::Ack {
                session_id: "ffffffffffffffffffffffffffffffff".into()
            }),
            Err(HandshakeError::SessionMismatch)
        );
        assert_eq!(responder.state(), State::Rejected);
        let mut responder = endpoint(Role::Responder);
        responder.start().unwrap();
        assert_eq!(
            responder.receive(Message::Ack {
                session_id: "0123456789abcdef0123456789abcdef".into()
            }),
            Err(HandshakeError::UnexpectedMessage)
        );
    }

    #[test]
    fn lite_transport_cannot_start_nsp_negotiation() {
        let mut endpoint = Handshake::new(
            Role::Initiator,
            "0123456789abcdef0123456789abcdef".into(),
            hello(),
        )
        .unwrap();
        assert_eq!(
            endpoint.transport_ready("moq-lite-02"),
            Err(HandshakeError::ProtocolVersion)
        );
        assert_eq!(endpoint.state(), State::Rejected);
    }

    #[test]
    fn closing_invalidates_active_media_permission() {
        let mut endpoint = endpoint(Role::Responder);
        endpoint.close();
        assert_eq!(endpoint.state(), State::Closed);
        assert!(endpoint.agreement().is_none());
        assert!(endpoint.start().is_err());
    }
}
