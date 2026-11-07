//! In-process publisher for the OBS module. / OBS 插件内的 PERI 发布端。
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use peri_native::{
    media::{Catalog, Frame, FrameKind, Publisher},
    quinn,
};
use peri_types::{Hello, PERI_VERSION, Profile};
use serde::Deserialize;
use std::{
    ffi::{CStr, c_char},
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{runtime::Runtime, task::JoinSet};

type Error = Box<dyn std::error::Error + Send + Sync>;
const BUDGET: Duration = Duration::from_secs(5);
type Receipt = (u64, bool, Result<(), peri_native::Error>);
fn receipt_breaks_video((gop, video, result): Receipt, current_gop: u64) -> Result<bool, Error> {
    match result {
        Ok(()) => Ok(false),
        Err(peri_native::Error::Timeout | peri_native::Error::ObjectAborted) => {
            Ok(video && gop == current_gop)
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct Invitation {
    address: SocketAddr,
    name: String,
    resource: String,
    token: String,
    cert: String,
}

struct Live {
    session: Option<tokio::task::JoinHandle<Result<peri_native::Session, Error>>>,
    publisher: Option<Arc<Publisher>>,
    feedback: Arc<Mutex<Option<Arc<Publisher>>>>,
    pending: JoinSet<Receipt>,
    receipt_limit: usize,
    sequence: u64,
    previous_video: u64,
    needs_key: bool,
    current_gop: u64,
    _endpoint: quinn::Endpoint,
    runtime: Runtime,
}

impl Live {
    fn open(text: &str) -> Result<Self, Error> {
        let encoded = text
            .strip_prefix("peri://v1/")
            .ok_or("需要完整的发布 Token / Invalid invitation")?;
        if encoded.len() > 32768 {
            return Err("Token too large".into());
        }
        let invitation: Invitation = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
        if invitation.address.ip().is_unspecified() || invitation.address.port() == 0 {
            return Err("Token 中必须填写可访问的 Relay 地址 / Invalid relay address".into());
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (endpoint, session) = runtime.block_on(async {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(rustls::pki_types::CertificateDer::from(
                STANDARD.decode(invitation.cert)?,
            ))?;
            let mut tls = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            tls.alpn_protocols = vec![peri_native::ALPN.to_vec()];
            let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
            let mut config = quinn::ClientConfig::new(Arc::new(crypto));
            let mut transport = quinn::TransportConfig::default();
            transport.keep_alive_interval(Some(Duration::from_secs(2)));
            transport.max_idle_timeout(Some(Duration::from_secs(10).try_into()?));
            if std::env::var_os("PERI_OBS_UPLINK_EXPERIMENT").is_some() {
                match std::env::var("PERI_OBS_CC").as_deref() {
                    Ok("bbr") => {
                        transport.congestion_controller_factory(Arc::new(
                            quinn::congestion::BbrConfig::default(),
                        ));
                    }
                    Ok("cubic") | Err(_) => {}
                    Ok(_) => return Err("PERI_OBS_CC must be cubic or bbr".into()),
                }
            }
            config.transport_config(Arc::new(transport));
            let bind = if invitation.address.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            };
            let mut endpoint = quinn::Endpoint::client(bind.parse()?)?;
            endpoint.set_default_client_config(config);
            let connecting = endpoint.connect(invitation.address, &invitation.name)?;
            // Network waits run on Tokio; start/stop on the OBS UI can cancel via endpoint.
            // 网络握手在 Tokio 中进行；OBS 启停可通过 endpoint 取消。
            let session = tokio::spawn(async move {
                let connection = tokio::time::timeout(BUDGET, connecting).await??;
                let hello = Hello {
                    version: PERI_VERSION,
                    profiles: vec![Profile::Live],
                    codecs: vec!["video/av1".into(), "audio/opus".into()],
                    extensions: vec![],
                };
                let mut session = peri_native::publish_authenticated(
                    connection,
                    hello,
                    &invitation.resource,
                    &invitation.token,
                    BUDGET,
                )
                .await?;
                if std::env::var_os("PERI_OBS_UPLINK_EXPERIMENT").is_some() {
                    session.set_send_limits(32, 2 * 1024 * 1024)?;
                }
                Ok::<_, Error>(session)
            });
            Ok::<_, Error>((endpoint, session))
        })?;
        Ok(Self {
            session: Some(session),
            publisher: None,
            feedback: Arc::new(Mutex::new(None)),
            pending: JoinSet::new(),
            receipt_limit: if std::env::var_os("PERI_OBS_UPLINK_EXPERIMENT").is_some() {
                32
            } else {
                8
            },
            sequence: 0,
            previous_video: 0,
            needs_key: true,
            current_gop: 0,
            _endpoint: endpoint,
            runtime,
        })
    }

    fn send(&mut self, block: &[u8], budget: Duration) -> Result<bool, Error> {
        let deadline = std::time::Instant::now() + budget;
        // Blocks are owned by the native output queue for this call. / 队列持有本次调用的媒体内存。
        if let Some(session) = self.session.take() {
            if block.len() < 4
                || u32::from_be_bytes(block[..4].try_into()?) as usize != block.len() - 4
            {
                return Err("invalid catalog length".into());
            }
            let catalog: Catalog = serde_json::from_slice(&block[4..])?;
            let publisher = Arc::new(self.runtime.block_on(Publisher::new(
                self.runtime.block_on(session)??,
                catalog,
                BUDGET,
            ))?);
            *self.feedback.lock().expect("feedback") = Some(publisher.clone());
            self.publisher = Some(publisher);
            return Ok(true);
        }
        let publisher = self.publisher.as_ref().ok_or("missing catalog")?.clone();
        if block.len() < 17
            || u32::from_be_bytes(block[13..17].try_into()?) as usize != block.len() - 17
        {
            return Err("invalid frame length".into());
        }
        let (track, kind) = match block[0] {
            1 => (1, FrameKind::Key),
            2 => (1, FrameKind::Reference),
            3 => (2, FrameKind::Audio),
            _ => return Err("unknown frame type".into()),
        };
        // Retire failed dependencies before admitting another object. Canceling the
        // bounded receipt set resets unfinished streams; never continue a broken GOP.
        // 接纳新对象前清理失败依赖；取消有界确认集合会重置未完成流。
        let lost = self.runtime.block_on(async {
            let mut lost = false;
            while let Some(result) = self.pending.try_join_next() {
                lost |= receipt_breaks_video(result?, self.current_gop)?;
            }
            if self.pending.len() >= self.receipt_limit {
                let wait = deadline.saturating_duration_since(std::time::Instant::now());
                let Ok(receipt) = tokio::time::timeout(wait, self.pending.join_next()).await else {
                    return Ok(true);
                };
                lost |= receipt_breaks_video(receipt.ok_or("receipt missing")??, self.current_gop)?;
            }
            Ok::<_, Error>(lost)
        })?;
        if lost {
            self.runtime.block_on(self.pending.shutdown());
            self.needs_key = true;
            if kind != FrameKind::Key {
                return Ok(false);
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            if track == 1 {
                self.needs_key = true;
            }
            return Ok(false);
        }
        if self.needs_key && kind == FrameKind::Reference {
            return Ok(false);
        }
        self.sequence += 1;
        let dependencies = if kind == FrameKind::Reference {
            vec![self.previous_video]
        } else {
            vec![]
        };
        if track == 1 {
            self.previous_video = self.sequence;
        }
        let frame = Frame {
            track,
            kind,
            sequence: self.sequence,
            dependencies,
            timestamp_us: u64::from_be_bytes(block[1..9].try_into()?),
            duration_us: u32::from_be_bytes(block[9..13].try_into()?),
            payload: block[17..].to_vec(),
        };
        let result = self.runtime.block_on(async {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(peri_native::Error::Timeout);
            }
            if kind == FrameKind::Key {
                peri_native::trace("source_key_start", serde_json::json!({"sequence":frame.sequence,"pts_us":frame.timestamp_us,"bytes":frame.payload.len(),"budget_ms":remaining.as_millis()}));
            }
            let receipt = publisher.send_with_receipt(&frame, remaining).await?;
            let gop = if kind == FrameKind::Key {
                frame.sequence
            } else {
                self.current_gop
            };
            let (sequence, pts_us) = (frame.sequence, frame.timestamp_us);
            self.pending.spawn(async move {
                let result = receipt.await;
                if kind == FrameKind::Key {
                    peri_native::trace("source_key_receipt", serde_json::json!({"sequence":sequence,"pts_us":pts_us,"ok":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}));
                }
                (gop, track == 1, result)
            });
            Ok::<_, peri_native::Error>(())
        });
        match result {
            Ok(()) => {
                if kind == FrameKind::Key {
                    self.needs_key = false;
                    self.current_gop = frame.sequence;
                }
                Ok(true)
            }
            Err(peri_native::Error::Timeout | peri_native::Error::ObjectAborted) => {
                if track == 1 {
                    self.runtime.block_on(self.pending.shutdown());
                    self.needs_key = true;
                }
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
}

// A separate shared handle permits feedback polling during a blocked media send.
// 独立共享句柄允许反馈线程在媒体发送阻塞时继续轮询，不与 Live 的可变借用重叠。
type FeedbackHandle = Arc<Mutex<Option<Arc<Publisher>>>>;
#[repr(C)]
#[derive(Default)]
struct Recovery {
    track: u16,
    origin: u32,
    id: u64,
    viewer: u64,
    after_sequence: u64,
}
#[repr(C)]
struct Transport {
    cwnd: u64,
    rtt_us: u64,
    sent_packets: u64,
    lost_packets: u64,
}
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_feedback_transport(
    handle: *const FeedbackHandle,
    out: *mut Transport,
) -> i32 {
    let publisher = unsafe { &*handle }.lock().expect("feedback").clone();
    let Some(publisher) = publisher else { return 0 };
    let path = publisher.transport_stats().path;
    unsafe {
        *out = Transport {
            cwnd: path.cwnd,
            rtt_us: path.rtt.as_micros().min(u64::MAX as u128) as u64,
            sent_packets: path.sent_packets,
            lost_packets: path.lost_packets,
        };
    }
    1
}
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_feedback_open(live: *const Live) -> *mut FeedbackHandle {
    Box::into_raw(Box::new(unsafe { &*live }.feedback.clone()))
}
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_feedback_poll(
    handle: *const FeedbackHandle,
    out: *mut Recovery,
) -> i32 {
    let publisher = unsafe { &*handle }.lock().expect("feedback").clone();
    let Some(publisher) = publisher else { return 0 };
    // Reports remain telemetry only; this adapter does not change bitrate.
    // 接收报告仍仅为遥测，本适配器不调整码率。
    if publisher.poll_report().is_err() {
        return -1;
    }
    match publisher.poll_recovery() {
        Ok(Some(r)) => {
            unsafe {
                *out = Recovery {
                    track: r.track,
                    origin: r.origin,
                    id: r.id,
                    viewer: r.viewer,
                    after_sequence: r.after_sequence,
                };
            }
            1
        }
        Ok(None) => 0,
        Err(_) => -1,
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_feedback_close(handle: *mut FeedbackHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

// The C++ module owns one opaque handle, joins the writer before dropping it,
// and passes valid buffers for the duration of each call.
// C++ 模块独占句柄，停止写线程后释放；传入缓冲在调用期间始终有效。
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_open(
    key: *const c_char,
    cancel: *mut *mut quinn::Endpoint,
    error: *mut c_char,
    cap: usize,
) -> *mut Live {
    match Live::open(unsafe { CStr::from_ptr(key) }.to_str().unwrap_or("")) {
        Ok(state) => {
            unsafe {
                *cancel = Box::into_raw(Box::new(state._endpoint.clone()));
            }
            Box::into_raw(Box::new(state))
        }
        Err(e) => {
            set_error(
                error,
                cap,
                &format!("PERI 连接失败 / Connection failed: {e}"),
            );
            std::ptr::null_mut()
        }
    }
}
fn set_error(buffer: *mut c_char, cap: usize, message: &str) {
    if cap == 0 {
        return;
    }
    let n = message.len().min(cap - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(message.as_ptr(), buffer.cast(), n);
        *buffer.add(n) = 0;
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_send(
    live: *mut Live,
    data: *const u8,
    len: usize,
    error: *mut c_char,
    cap: usize,
) -> bool {
    match unsafe { &mut *live }.send(unsafe { std::slice::from_raw_parts(data, len) }, BUDGET) {
        Ok(true) => true,
        Ok(false) => {
            set_error(error, cap, "PERI media expired / 媒体发送过期");
            false
        }
        Err(e) => {
            set_error(error, cap, &format!("PERI 发送失败 / Publish failed: {e}"));
            false
        }
    }
}
// 1 = admitted, 0 = new recovery, 2 = waiting for a key, 3 = audio drop, -1 = fatal.
// 预算由原生队列扣除排队年龄；媒体过期不等同于发布连接失败。
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_send_budget(
    live: *mut Live,
    data: *const u8,
    len: usize,
    remaining_ms: u32,
    error: *mut c_char,
    cap: usize,
) -> i32 {
    let live = unsafe { &mut *live };
    let block = unsafe { std::slice::from_raw_parts(data, len) };
    let waiting = live.needs_key && block.first() == Some(&2);
    match live.send(
        block,
        Duration::from_millis(u64::from(remaining_ms.min(750))),
    ) {
        Ok(true) => 1,
        Ok(false) if waiting => 2,
        Ok(false) if block.first() == Some(&3) && !live.needs_key => 3,
        Ok(false) => 0,
        Err(e) => {
            set_error(error, cap, &format!("PERI 发送失败 / Publish failed: {e}"));
            -1
        }
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_close(live: *mut Live) {
    if !live.is_null() {
        drop(unsafe { Box::from_raw(live) });
    }
}

// OBS 32 builds its service picker from the rtmp-services JSON catalog.
// OBS 32 从服务清单生成下拉菜单；保留其他服务及未知字段，原子更新用户副本。
fn register_catalog(stock: &Path, user: &Path) -> Result<(), Error> {
    let data = match std::fs::read(user) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::read(stock)?,
        Err(e) => return Err(e.into()),
    };
    let mut root: serde_json::Value = serde_json::from_slice(&data)?;
    let services = root["services"]
        .as_array_mut()
        .ok_or("invalid OBS services catalog")?;
    if services
        .iter()
        .any(|s| s["name"] == "PERI" && s["protocol"] == "PERI")
    {
        return Ok(());
    }
    services.push(serde_json::json!({"name":"PERI","common":true,"protocol":"PERI","servers":[{"name":"Token 内的服务器 / From token","url":"peri://invitation"}],"supported video codecs":["av1"],"supported audio codecs":["opus"]}));
    std::fs::create_dir_all(user.parent().ok_or("missing catalog directory")?)?;
    let tmp = user.with_extension(format!("peri-{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(&root)?)?;
    if let Err(e) = std::fs::rename(&tmp, user) {
        let _ = std::fs::remove_file(tmp);
        return Err(e.into());
    }
    Ok(())
}
#[unsafe(no_mangle)]
unsafe extern "C" fn peri_register_catalog(stock: *const c_char, user: *const c_char) -> bool {
    let paths = (
        unsafe { CStr::from_ptr(stock) }.to_str(),
        unsafe { CStr::from_ptr(user) }.to_str(),
    );
    matches!(paths,(Ok(a),Ok(b)) if register_catalog(Path::new(a),Path::new(b)).is_ok())
}

#[unsafe(no_mangle)]
unsafe extern "C" fn peri_live_cancel(handle: *mut quinn::Endpoint) {
    if !handle.is_null() {
        let ep = unsafe { Box::from_raw(handle) };
        ep.close(0u32.into(), b"stopped");
    }
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../peri-native/examples/support/mod.rs"]
mod support;
#[cfg(test)]
mod tests {
    use super::*;
    use peri_native::media::{Subscriber, Track, TrackKind};

    #[test]
    fn audio_and_old_gop_receipts_do_not_cancel_a_new_recovery_key() -> Result<(), Error> {
        assert!(!receipt_breaks_video(
            (10, false, Err(peri_native::Error::Timeout)),
            10
        )?);
        assert!(!receipt_breaks_video(
            (9, true, Err(peri_native::Error::ObjectAborted)),
            10
        )?);
        assert!(receipt_breaks_video(
            (10, true, Err(peri_native::Error::Timeout)),
            10
        )?);
        assert!(
            receipt_breaks_video(
                (9, false, Err(peri_native::Error::Protocol("bad receipt"))),
                10
            )
            .is_err()
        );
        Ok(())
    }

    // Exercise the production Live state against real flow-controlled QUIC.
    // 使用真实受流控 QUIC 验证生产 Live 状态，过期不能断开发布连接。
    #[test]
    fn expired_media_resets_stream_and_requires_a_fresh_key() -> Result<(), Error> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let (pair, publisher, mut sub) = runtime.block_on(async {
            let pair =
                support::pair_with_stream_window(peri_native::ALPN, false, Some(1024)).await?;
            let hello = || Hello {
                version: PERI_VERSION,
                profiles: vec![Profile::Live],
                codecs: vec!["video/av1".into()],
                extensions: vec![],
            };
            let (source, sink) = tokio::try_join!(
                peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
                peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
            )?;
            let catalog = Catalog {
                tracks: vec![Track {
                    id: 1,
                    kind: TrackKind::Video,
                    codec: "video/av1".into(),
                    init: vec![],
                    width: Some(320),
                    height: Some(180),
                    sample_rate: None,
                    channels: None,
                }],
            };
            let (publisher, sub) = tokio::try_join!(
                Publisher::new(source, catalog, BUDGET),
                Subscriber::new(sink, BUDGET)
            )?;
            Ok::<_, Error>((pair, Arc::new(publisher), sub))
        })?;
        let mut live = Live {
            session: None,
            publisher: Some(publisher.clone()),
            feedback: Arc::new(Mutex::new(Some(publisher))),
            pending: JoinSet::new(),
            receipt_limit: 8,
            sequence: 0,
            previous_video: 0,
            needs_key: true,
            current_gop: 0,
            _endpoint: pair.server.clone(),
            runtime,
        };
        let block = |kind, size: usize| {
            let mut b = vec![kind];
            b.extend_from_slice(&0u64.to_be_bytes());
            b.extend_from_slice(&16667u32.to_be_bytes());
            b.extend_from_slice(&(size as u32).to_be_bytes());
            b.resize(17 + size, 42);
            b
        };
        let before = std::time::Instant::now();
        assert!(!live.send(&block(1, 65536), Duration::from_millis(60))?);
        assert!(before.elapsed() < Duration::from_millis(500));
        assert!(live.needs_key);
        assert!(pair.incoming.close_reason().is_none());
        assert!(!live.send(&block(2, 10), Duration::from_millis(250))?);
        assert_eq!(live.sequence, 1, "dependent frame must not open a stream");
        assert!(
            live.runtime
                .block_on(sub.recv_retained(BUDGET, Duration::from_millis(250), false))?
                .is_none()
        );
        assert!(live.send(&block(1, 10), Duration::from_millis(250))?);
        let recovered = live
            .runtime
            .block_on(sub.recv_retained(BUDGET, Duration::from_millis(250), false))?
            .ok_or("missing key")?;
        assert_eq!(recovered.kind, FrameKind::Key);
        assert!(recovered.dependencies.is_empty());
        assert!(!live.needs_key);
        assert!(!live.send(&block(2, 10), Duration::ZERO)?);
        assert!(
            live.needs_key,
            "queue-expired reference breaks dependency chain"
        );
        assert!(!live.send(&block(2, 10), Duration::from_millis(250))?);
        assert_eq!(live.sequence, 2);
        live.runtime.block_on(live.pending.shutdown());
        live.runtime.block_on(async {
            for _ in 0..8 {
                live.pending.spawn(std::future::pending());
            }
        });
        let before = std::time::Instant::now();
        assert!(!live.send(&block(1, 10), Duration::from_millis(10))?);
        assert!(before.elapsed() < Duration::from_millis(500));
        assert!(
            live.pending.is_empty(),
            "expired slot wait cancels pending work"
        );
        assert!(live.needs_key);
        assert_eq!(
            live.sequence, 2,
            "slot waiting must consume the same budget"
        );
        Ok(())
    }

    // The feedback handle must work while media has exhausted stream credit.
    // 媒体发送耗尽流窗口时，独立反馈句柄仍必须可用。
    #[tokio::test]
    async fn feedback_survives_blocked_media_send() -> Result<(), Error> {
        let pair = support::pair_with_stream_window(peri_native::ALPN, false, Some(1024)).await?;
        let hello = || Hello {
            version: PERI_VERSION,
            profiles: vec![Profile::Live],
            codecs: vec!["video/av1".into()],
            extensions: vec![],
        };
        let (source, sink) = tokio::try_join!(
            peri_native::accept(pair.incoming.clone(), hello(), "live/test", BUDGET),
            peri_native::subscribe(pair.outgoing.clone(), hello(), "live/test", BUDGET)
        )?;
        let catalog = Catalog {
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                codec: "video/av1".into(),
                init: vec![],
                width: Some(320),
                height: Some(180),
                sample_rate: None,
                channels: None,
            }],
        };
        let (publisher, sub) = tokio::try_join!(
            Publisher::new(source, catalog, BUDGET),
            Subscriber::new(sink, BUDGET)
        )?;
        let publisher = Arc::new(publisher);
        let handle = Box::into_raw(Box::new(Arc::new(Mutex::new(Some(publisher.clone())))));
        let frame = Frame {
            track: 1,
            kind: FrameKind::Key,
            sequence: 1,
            dependencies: vec![],
            timestamp_us: 0,
            duration_us: 16667,
            payload: vec![42; 65536],
        };
        let mut send = Box::pin(publisher.send(&frame, BUDGET));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut send)
                .await
                .is_err()
        );
        let request = peri_native::RecoveryRequest::after(1, 7, 11);
        sub.forward_recovery(request).await?;
        let mut out = Recovery::default();
        tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let result = unsafe { peri_live_feedback_poll(handle, &mut out) };
                assert!(result >= 0);
                if result == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        assert_eq!(
            (
                out.track,
                out.origin,
                out.id,
                out.viewer,
                out.after_sequence
            ),
            (1, request.origin, request.id, 7, 11)
        );
        assert_eq!(unsafe { peri_live_feedback_poll(handle, &mut out) }, 0);
        assert_eq!(std::mem::size_of::<Recovery>(), 32);
        unsafe {
            peri_live_feedback_close(handle);
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut send)
                .await
                .is_err()
        );
        drop(send);
        Ok(())
    }
}
