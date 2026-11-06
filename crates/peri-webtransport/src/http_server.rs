//! Bounded GET-only HTTP egress. / 有界的只读 HTTP 观看端。
use crate::{Error, LIMIT, hub, mobile::Mobile, webm};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
const PAGE: &str = include_str!("../web/watch.html");
const PLAYER: &[u8] = include_bytes!("../web/player.js");
const HLS: &[u8] = include_bytes!("../../../scripts/vendor/hls.js/hls.light.min.js");

async fn reply(sock: &mut TcpStream, status: &str, mime: &str, data: &[u8]) -> Result<(), Error> {
    let header = format!(
        "HTTP/1.0 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        data.len()
    );
    timeout(LIMIT, async {
        sock.write_all(header.as_bytes()).await?;
        sock.write_all(data).await
    })
    .await??;
    Ok(())
}
pub async fn serve(listener: TcpListener, hub: hub::Hub, mobile: Mobile) -> Result<(), Error> {
    let config = serde_json::json!({"url":std::env::var("PERI_WT_URL").unwrap_or_default(),"hash":std::env::var("PERI_WT_CERT_HASH").unwrap_or_default()});
    let page: Arc<str> = PAGE
        .replace(
            "__PERI_WT_CONFIG__",
            &config.to_string().replace('<', "\\u003c"),
        )
        .into();
    let slots = Arc::new(Semaphore::new(256));
    loop {
        let (sock, _) = listener.accept().await?;
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let (h, m, p) = (hub.clone(), mobile.clone(), page.clone());
        tokio::spawn(async move {
            let _slot = slot;
            let _ = serve_one(sock, h, m, &p).await;
        });
    }
}
async fn serve_one(
    mut sock: TcpStream,
    hub: hub::Hub,
    mobile: Mobile,
    page: &str,
) -> Result<(), Error> {
    let mut read = BufReader::new((&mut sock).take(16384));
    let mut line = String::new();
    timeout(LIMIT, async {
        read.read_line(&mut line).await?;
        loop {
            let mut header = String::new();
            if read.read_line(&mut header).await? == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            if header == "\r\n" {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    })
    .await??;
    drop(read);
    let mut words = line.split_whitespace();
    if words.next() != Some("GET") {
        return reply(
            &mut sock,
            "405 Method Not Allowed",
            "text/plain",
            b"GET required",
        )
        .await;
    }
    let uri = words.next().unwrap_or("");
    let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
    match path {
        "/" => {
            reply(
                &mut sock,
                "200 OK",
                "text/html; charset=utf-8",
                page.as_bytes(),
            )
            .await
        }
        "/peri-webtransport.js" => {
            reply(&mut sock, "200 OK", "application/javascript", PLAYER).await
        }
        "/hls.light.min.js" => reply(&mut sock, "200 OK", "application/javascript", HLS).await,
        "/live.webm" => {
            let Ok((catalog, mut rx, mut frame)) = hub::join(hub).await else {
                return reply(
                    &mut sock,
                    "503 Unavailable",
                    "text/plain",
                    b"stream offline",
                )
                .await;
            };
            sock.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: video/webm\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n").await?;
            sock.write_all(&webm::header(&catalog)).await?;
            let origin = frame.timestamp_us;
            loop {
                timeout(LIMIT, sock.write_all(&webm::cluster(&frame, origin))).await??;
                frame = timeout(LIMIT, rx.recv()).await??;
            }
        }
        path if path.starts_with("/mobile/") => {
            let name = &path[8..];
            let playlist = name == "live.m3u8";
            let Ok(parts) = mobile.access(playlist).await else {
                return reply(
                    &mut sock,
                    "503 Unavailable",
                    "text/plain",
                    b"encoder unavailable",
                )
                .await;
            };
            let data = if playlist {
                let mut msn = None;
                let mut part = None;
                for kv in query.split('&') {
                    if let Some((k, v)) = kv.split_once('=') {
                        match k {
                            "_HLS_msn" => msn = Some(v.parse()?),
                            "_HLS_part" => part = Some(v.parse()?),
                            _ => (),
                        }
                    }
                }
                parts.manifest(msn, part).await.map(|(m, _)| m.into_bytes())
            } else {
                parts.file(name).await
            };
            match data {
                Ok(data) => {
                    reply(
                        &mut sock,
                        "200 OK",
                        if playlist {
                            "application/vnd.apple.mpegurl"
                        } else {
                            "video/mp4"
                        },
                        &data,
                    )
                    .await
                }
                Err(_) => {
                    reply(
                        &mut sock,
                        if playlist {
                            "503 Unavailable"
                        } else {
                            "404 Not Found"
                        },
                        "text/plain",
                        b"unavailable",
                    )
                    .await
                }
            }
        }
        _ => reply(&mut sock, "404 Not Found", "text/plain", b"not found").await,
    }
}
