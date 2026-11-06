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
    sync::Arc,
    time::Duration,
};
use tokio::{runtime::Runtime, task::JoinSet};

type Error = Box<dyn std::error::Error + Send + Sync>;
const BUDGET: Duration = Duration::from_secs(5);

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
    publisher: Option<Publisher>,
    pending: JoinSet<Result<(), peri_native::Error>>,
    sequence: u64,
    previous_video: u64,
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
                let session = peri_native::publish_authenticated(
                    connection,
                    hello,
                    &invitation.resource,
                    &invitation.token,
                    BUDGET,
                )
                .await?;
                Ok::<_, Error>(session)
            });
            Ok::<_, Error>((endpoint, session))
        })?;
        Ok(Self {
            session: Some(session),
            publisher: None,
            pending: JoinSet::new(),
            sequence: 0,
            previous_video: 0,
            _endpoint: endpoint,
            runtime,
        })
    }

    fn send(&mut self, block: &[u8]) -> Result<(), Error> {
        // Blocks are owned by the native output queue for this call. / 队列持有本次调用的媒体内存。
        if let Some(session) = self.session.take() {
            if block.len() < 4
                || u32::from_be_bytes(block[..4].try_into()?) as usize != block.len() - 4
            {
                return Err("invalid catalog length".into());
            }
            let catalog: Catalog = serde_json::from_slice(&block[4..])?;
            self.publisher = Some(self.runtime.block_on(Publisher::new(
                self.runtime.block_on(session)??,
                catalog,
                BUDGET,
            ))?);
            return Ok(());
        }
        let publisher = self.publisher.as_ref().ok_or("missing catalog")?;
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
        self.runtime.block_on(async {
            if self.pending.len() >= 8 {
                self.pending
                    .join_next()
                    .await
                    .ok_or("receipt missing")???;
            }
            self.pending
                .spawn(publisher.send_with_receipt(&frame, BUDGET).await?);
            let _ = publisher.poll_recovery()?;
            let _ = publisher.poll_report()?;
            Ok(())
        })
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
    match unsafe { &mut *live }.send(unsafe { std::slice::from_raw_parts(data, len) }) {
        Ok(()) => true,
        Err(e) => {
            set_error(error, cap, &format!("PERI 发送失败 / Publish failed: {e}"));
            false
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
