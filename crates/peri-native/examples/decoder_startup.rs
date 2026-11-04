//! 内部诊断：逐次创建解码进程并测量首帧，失败也记录，不重试掩盖样本。
use peri_native::{av1::read_gop_ivf, playback::Decoder};
use std::{
    io::Write,
    time::{Duration, Instant},
};
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: decoder_startup input.ivf probe.json report.json".into());
    }
    let (catalog, frames) = read_gop_ivf(&std::fs::read(&args[1])?, &std::fs::read(&args[2])?)?;
    let mut samples = Vec::new();
    let mut expected: Option<Vec<u8>> = None;
    for index in 0..12 {
        let start = Instant::now();
        let mut decoder = Decoder::new(&catalog.tracks[0]).await?;
        let spawned = start.elapsed().as_micros();
        let decode = Instant::now();
        let success = matches!(
            tokio::time::timeout(Duration::from_millis(240), decoder.decode(&frames[0])).await,
            Ok(Ok(()))
        );
        let decode_us = decode.elapsed().as_micros();
        let total_us = start.elapsed().as_micros();
        if success {
            if let Some(expected) = &expected {
                if decoder.pixels() != expected {
                    return Err("pixel mismatch across restarts".into());
                }
            } else {
                expected = Some(decoder.pixels().to_vec());
            }
        }
        decoder.stop().await;
        let row = serde_json::json!({"iteration":index,"spawn_us":spawned,"decode_us":decode_us,"total_us":total_us,"success":success});
        println!("{row}");
        samples.push(row);
    }
    // 手动仅推进一次真实解码Future，再取消；后续调用必须立即拒绝复用。
    let mut decoder = Decoder::new(&catalog.tracks[0]).await?;
    let mut operation = Box::pin(decoder.decode(&frames[0]));
    std::future::poll_fn(
        |cx| match std::future::Future::poll(operation.as_mut(), cx) {
            std::task::Poll::Pending => std::task::Poll::Ready(Ok::<_, &str>(())),
            std::task::Poll::Ready(_) => {
                std::task::Poll::Ready(Err("decode unexpectedly completed in one poll"))
            }
        },
    )
    .await?;
    drop(operation);
    let error = match decoder.decode(&frames[0]).await {
        Err(error) => error,
        Ok(()) => return Err("cancelled decoder was reused".into()),
    };
    if error.to_string() != "decoder requires restart after interrupted frame" {
        return Err("cancelled decoder did not reject reuse explicitly".into());
    }
    decoder.stop().await;
    println!("CANCELLED_DECODER_REUSE_REJECTED");
    std::fs::File::create(&args[3])?
        .write_all(serde_json::to_string_pretty(&serde_json::json!({"scope":"decoder-startup-only","decodeBudgetMs":240,"samples":samples,"cancelledReuseRejected":true,"successfulPixelsEqual":true}))?.as_bytes())?;
    if samples.iter().any(|r| r["success"] != true) {
        return Err("startup samples exceeded unchanged decode budget".into());
    }
    Ok(())
}
