//! PERI negotiation over ordinary MOQT tracks on native QUIC.
//! 在原生 QUIC 的普通 MOQT 控制轨道上完成 PERI 双向协商。

use crate::{Session, moq};
use peri_session::{
    handshake::{Handshake, Role, State},
    wire,
};
use peri_types::{Agreement, Hello, MAX_CONTROL_BYTES};
use std::time::Duration;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Keep negotiated controls alive and expose the agreement only on a live connection.
/// 保持控制轨道存活，仅在底层连接正常时提供协商结果。
pub struct ActivePeer {
    session: Session,
    agreement: Agreement,
    _broadcast: moq::broadcast::Producer,
    _track: moq::track::Producer,
}
impl ActivePeer {
    pub fn agreement(&self) -> Result<&Agreement, Error> {
        if self.session.connection.close_reason().is_some() {
            return Err("session closed".into());
        }
        Ok(&self.agreement)
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
}

/// Complete offer/accept/ack within one overall deadline, consuming the session.
/// 在统一超时内完成协商；失败或取消会释放持有的会话与控制轨道。
pub async fn negotiate(
    session: Session,
    origin: &moq::origin::Producer,
    role: Role,
    session_id: String,
    hello: Hello,
    timeout: Duration,
) -> Result<ActivePeer, Error> {
    tokio::time::timeout(timeout, async move {
        let mut handshake = Handshake::new(role, session_id.clone(), hello)?;
        // 外部协议版本检查只属于内部适配器，正式协商状态机只接收已校验的就绪通知。
        if session.inner.version().as_str() != "moq-transport-22" {
            return Err("unsupported comparison transport version".into());
        }
        handshake.transport_ready()?;
        let (local, remote) = match role {
            Role::Initiator => ("initiator", "responder"),
            Role::Responder => ("responder", "initiator"),
        };
        let broadcast = origin.create_broadcast(format!("peri/{session_id}/{local}"))?;
        let track = broadcast.create_track("peri.control", None)?;
        broadcast.announce(Default::default())?;
        let send = |message: &peri_session::handshake::Message| -> Result<(), Error> {
            let bytes = wire::encode(message)?;
            let mut group = track.append_group()?;
            group.write_frame(moq::Timestamp::ZERO, bytes)?;
            group.finish()?;
            Ok(())
        };
        if let Some(offer) = handshake.start()? {
            send(&offer)?;
        }
        let consumer = origin.consume();
        let path = format!("peri/{session_id}/{remote}");
        consumer.routed(&path).await.ok_or("peer route closed")?;
        let remote = consumer.request_broadcast(&path).await?;
        let subscription =
            moq::track::Subscription::default().with_start(moq::track::Position::group(0));
        let mut input = remote
            .track("peri.control")?
            .subscribe(subscription)
            .await?;
        while handshake.state() != State::Active {
            let mut group = input.recv_group().await?.ok_or("control track ended")?;
            let mut frame = group.next_frame().await?.ok_or("empty control group")?;
            if frame.size > MAX_CONTROL_BYTES as u64 {
                return Err("control object too large".into());
            }
            let message = wire::decode(&frame.read_all().await?)?;
            let reply = handshake.receive(message)?;
            if let Some(reply) = reply {
                send(&reply)?;
                if matches!(reply, peri_session::handshake::Message::Ack { .. }) {
                    handshake.ack_sent()?;
                }
            }
        }
        let agreement = handshake.agreement().ok_or("session not active")?.clone();
        Ok(ActivePeer {
            session,
            agreement,
            _broadcast: broadcast,
            _track: track,
        })
    })
    .await
    .map_err(|_| -> Error { "PERI negotiation timed out".into() })?
}
