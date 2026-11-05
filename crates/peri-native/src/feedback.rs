//! Bounded authenticated recovery and receiver telemetry.
use crate::{Control, Error, wire};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
static SERIAL: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequest {
    pub track: u16,
    pub origin: u32,
    pub id: u64,
    /// 转发节点分配的订阅者作用域，必须与接收报告对应。
    pub viewer: u64,
    /// 最后正常输出或已知故障的位置，0为未知，不允许据此推断已恢复。
    pub after_sequence: u64,
}
impl RecoveryRequest {
    pub fn new(track: u16) -> Self {
        Self::after(track, 0, 0)
    }
    /// 创建带作用域及位置的恢复请求；origin/id仅用于追踪，不作为认证。
    pub fn after(track: u16, viewer: u64, after_sequence: u64) -> Self {
        Self {
            viewer,
            after_sequence,
            track,
            origin: std::process::id(),
            id: SERIAL.fetch_add(1, Ordering::Relaxed),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverReport {
    pub track: u16,
    pub viewer: u64,
    pub sample: u64,
    pub sequence: u64,
    pub media_us: u64,
    pub buffered_ms: u32,
    pub queued_bytes: u32,
    pub lateness_ms: u32,
    pub dropped: u64,
    pub recoveries: u32,
}
impl ReceiverReport {
    pub fn valid(&self) -> bool {
        self.track > 0
            && self.sample > 0
            && self.buffered_ms <= 10000
            && self.lateness_ms <= 60000
            && self.queued_bytes <= 8 * 1024 * 1024
    }
}
pub fn trace(event: &str, fields: impl Serialize) {
    println!(
        "PERI_TRACE {}",
        serde_json::json!({"event":event,"wall_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),"process":std::process::id(),"data":fields})
    );
}
struct SendState {
    stream: quinn::SendStream,
    key: Option<Instant>,
    report: Option<Instant>,
}
pub(crate) struct Feedback {
    send: AsyncMutex<SendState>,
    inbox: Mutex<mpsc::Receiver<RecoveryRequest>>,
    reports: std::sync::Arc<Mutex<Option<(ReceiverReport, Instant)>>>,
    reader: tokio::task::JoinHandle<()>,
    connection: quinn::Connection,
}
impl Feedback {
    pub fn new(
        send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        connection: quinn::Connection,
        publisher: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel(8);
        let reports = std::sync::Arc::new(Mutex::new(None));
        let output = reports.clone();
        let peer = connection.clone();
        let reader = tokio::spawn(async move {
            let mut window = Instant::now();
            let mut count = 0;
            loop {
                let result = wire::read_control::<_, Control>(&mut recv).await;
                if window.elapsed() >= Duration::from_secs(1) {
                    window = Instant::now();
                    count = 0;
                }
                count += 1;
                if !publisher || count > 10 {
                    peer.close(2u32.into(), b"invalid feedback");
                    break;
                }
                match result {
                    Ok(Control::Keyframe {
                        track,
                        origin,
                        id,
                        viewer,
                        after_sequence,
                    }) if track > 0 && id > 0 => {
                        let _ = tx.try_send(RecoveryRequest {
                            track,
                            origin,
                            id,
                            viewer,
                            after_sequence,
                        });
                    }
                    Ok(Control::ReceiverReport { report }) if report.valid() => {
                        *output.lock().expect("reports") = Some((report, Instant::now()));
                    }
                    _ => {
                        peer.close(2u32.into(), b"invalid feedback");
                        break;
                    }
                }
            }
        });
        Self {
            send: AsyncMutex::new(SendState {
                stream: send,
                key: None,
                report: None,
            }),
            inbox: Mutex::new(rx),
            reports,
            reader,
            connection,
        }
    }
    pub fn poll(&self) -> Option<RecoveryRequest> {
        self.inbox.lock().expect("inbox").try_recv().ok()
    }
    pub fn report(&self) -> Option<ReceiverReport> {
        self.reports
            .lock()
            .expect("reports")
            .take()
            .filter(|(_, at)| at.elapsed() <= Duration::from_secs(2))
            .map(|(report, _)| report)
    }
    pub async fn request(&self, request: RecoveryRequest) -> Result<(), Error> {
        if request.track == 0 || request.id == 0 {
            return Err(Error::Protocol("invalid recovery request"));
        }
        self.write(
            Control::Keyframe {
                track: request.track,
                origin: request.origin,
                id: request.id,
                viewer: request.viewer,
                after_sequence: request.after_sequence,
            },
            false,
        )
        .await
    }
    pub async fn send_report(&self, mut report: ReceiverReport) -> Result<(), Error> {
        report.sample = SERIAL.fetch_add(1, Ordering::Relaxed);
        if !report.valid() {
            return Err(Error::Protocol("invalid receiver report"));
        }
        self.write(Control::ReceiverReport { report }, true).await
    }
    async fn write(&self, message: Control, report: bool) -> Result<(), Error> {
        let mut state = self.send.lock().await;
        let last = if report { state.report } else { state.key };
        if last.is_some_and(|last| {
            last.elapsed() < Duration::from_millis(if report { 500 } else { 200 })
        }) {
            return Ok(());
        }
        struct Guard(Option<quinn::Connection>);
        impl Drop for Guard {
            fn drop(&mut self) {
                if let Some(c) = self.0.take() {
                    c.close(2u32.into(), b"feedback write cancelled");
                }
            }
        }
        let mut guard = Guard(Some(self.connection.clone()));
        tokio::time::timeout(
            Duration::from_millis(200),
            wire::write_control(&mut state.stream, &message),
        )
        .await
        .map_err(|_| Error::Timeout)??;
        guard.0 = None;
        if report {
            state.report = Some(Instant::now());
        } else {
            state.key = Some(Instant::now());
        }
        match message {
            Control::Keyframe {
                track,
                origin,
                id,
                viewer,
                after_sequence,
            } => trace(
                "request_sent",
                RecoveryRequest {
                    track,
                    origin,
                    id,
                    viewer,
                    after_sequence,
                },
            ),
            Control::ReceiverReport { report } => trace("report_sent", report),
            _ => {}
        }
        Ok(())
    }
}
impl Drop for Feedback {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
