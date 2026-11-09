//! Experimental PERI framing, independent of third-party object protocols.
//! 实验 PERI 封装，不依赖第三方对象协议；所有长度在分配前校验。

use crate::{Error, Object};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PREFACE: &[u8; 8] = b"PERI\x00\x02\x00\x00";
const MAX_CONTROL: usize = 16 * 1024;
pub const MAX_PAYLOAD: usize = 1024 * 1024;
const OBJECT_HEADER: usize = 25;

// 流读取与修复后重组共用头部校验，避免两套长度/标志规则逐渐分叉。
fn parse_header(bytes: &[u8; OBJECT_HEADER]) -> Result<(Object, usize), Error> {
    let size = u32::from_be_bytes(bytes[21..25].try_into().expect("fixed header")) as usize;
    if bytes[20] > 1 || size == 0 || size > MAX_PAYLOAD {
        return Err(Error::Protocol("object header"));
    }
    Ok((
        Object {
            sequence: u64::from_be_bytes(bytes[..8].try_into().expect("fixed header")),
            timestamp_us: u64::from_be_bytes(bytes[8..16].try_into().expect("fixed header")),
            duration_us: u32::from_be_bytes(bytes[16..20].try_into().expect("fixed header")),
            independent: bytes[20] == 1,
            payload: Vec::new(),
        },
        size,
    ))
}

pub(crate) fn decode_object_packet(bytes: &[u8]) -> Result<Object, Error> {
    if bytes.len() > MAX_PAYLOAD + OBJECT_HEADER {
        return Err(Error::Protocol("object packet limit"));
    }
    let header = bytes
        .get(..OBJECT_HEADER)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
    let (mut object, size) = parse_header(header.try_into().expect("fixed header"))?;
    if bytes.len() < OBJECT_HEADER + size {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    if bytes.len() > OBJECT_HEADER + size {
        return Err(Error::Protocol("object trailing data"));
    }
    object.payload = bytes[OBJECT_HEADER..].to_vec();
    Ok(object)
}

pub async fn write_control<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec(message)?;
    if bytes.is_empty() || bytes.len() > MAX_CONTROL {
        return Err(Error::Protocol("control length"));
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    Ok(())
}

pub async fn read_control<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<T, Error> {
    let size = reader.read_u32().await? as usize;
    if size == 0 || size > MAX_CONTROL {
        return Err(Error::Protocol("control length"));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn encode_object(object: &Object) -> Result<Vec<u8>, Error> {
    let size = encoded_object_len(object)?;
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(&object.sequence.to_be_bytes());
    bytes.extend_from_slice(&object.timestamp_us.to_be_bytes());
    bytes.extend_from_slice(&object.duration_us.to_be_bytes());
    bytes.push(u8::from(object.independent));
    bytes.extend_from_slice(&(object.payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&object.payload);
    Ok(bytes)
}

pub(crate) fn encoded_object_len(object: &Object) -> Result<usize, Error> {
    if object.payload.is_empty() || object.payload.len() > MAX_PAYLOAD {
        return Err(Error::Protocol("object length"));
    }
    Ok(OBJECT_HEADER + object.payload.len())
}

#[cfg(test)]
pub async fn read_object<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Object, Error> {
    read_object_selected(reader, false)
        .await?
        .ok_or(Error::Protocol("object selection"))
}

#[cfg(test)]
pub async fn read_object_selected<R: AsyncRead + Unpin>(
    reader: &mut R,
    independent_only: bool,
) -> Result<Option<Object>, Error> {
    read_object_observed(reader, independent_only, &mut ReadProgress::default()).await
}

/// 本地诊断进度；完整头部通过验证后才关联序号，部分头部不能冒充已知对象。
#[derive(Serialize)]
pub(crate) struct ReadProgress {
    pub sequence: Option<u64>,
    pub phase: &'static str,
}
impl Default for ReadProgress {
    fn default() -> Self {
        Self {
            sequence: None,
            phase: "header",
        }
    }
}
pub(crate) async fn read_object_observed<R: AsyncRead + Unpin>(
    reader: &mut R,
    independent_only: bool,
    progress: &mut ReadProgress,
) -> Result<Option<Object>, Error> {
    let mut header = [0; OBJECT_HEADER];
    reader.read_exact(&mut header).await?;
    let (mut object, size) = parse_header(&header)?;
    progress.sequence = Some(object.sequence);
    // 先校验长度和标志再跳过，防止恢复模式绕过头部边界检查；不分配旧参考负载。
    if independent_only && !object.independent {
        progress.phase = "selected_out";
        return Ok(None);
    }
    progress.phase = "payload";
    let mut payload = vec![0; size];
    reader.read_exact(&mut payload).await?;
    progress.phase = "fin";
    // 每个单向流只允许一个对象，禁止拼接对象或忽略尾部数据。
    if reader.read(&mut [0]).await? != 0 {
        return Err(Error::Protocol("object trailing data"));
    }
    progress.phase = "complete";
    object.payload = payload;
    Ok(Some(object))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn observation_never_assigns_unvalidated_sequence_and_distinguishes_fin() {
        let object = Object {
            sequence: 42,
            timestamp_us: 0,
            duration_us: 66667,
            independent: true,
            payload: vec![1, 2, 3],
        };
        let bytes = encode_object(&object).unwrap();
        for scenario in 0..4 {
            let mut input = bytes.clone();
            match scenario {
                0 => input[20] = 2,
                1 => {
                    input.pop();
                }
                2 => input.push(0),
                _ => {}
            }
            let mut progress = ReadProgress::default();
            let result = read_object_observed(&mut input.as_slice(), false, &mut progress).await;
            if scenario == 0 {
                assert_eq!(progress.sequence, None);
                assert_eq!(progress.phase, "header");
            } else {
                assert_eq!(progress.sequence, Some(42));
                assert_eq!(
                    progress.phase,
                    match scenario {
                        1 => "payload",
                        2 => "fin",
                        _ => "complete",
                    }
                );
            }
            assert_eq!(result.is_ok(), scenario == 3);
        }
    }

    #[tokio::test]
    async fn rejects_lengths_before_reading_payload() {
        let mut control = &u32::MAX.to_be_bytes()[..];
        assert!(matches!(
            read_control::<_, serde_json::Value>(&mut control).await,
            Err(Error::Protocol(_))
        ));
        let mut bytes = vec![0; OBJECT_HEADER];
        bytes[21..25].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            read_object(&mut bytes.as_slice()).await,
            Err(Error::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn object_has_exact_binary_layout_and_requires_fin() {
        let object = Object {
            sequence: 42,
            timestamp_us: 1000,
            duration_us: 20000,
            independent: true,
            payload: vec![1, 2, 3],
        };
        let bytes = encode_object(&object).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/peri-1.0.json")).unwrap();
        let hex = |data: &[u8]| data.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(&bytes), fixture["object"]["hex"].as_str().unwrap());
        assert_eq!(hex(PREFACE), fixture["preface"].as_str().unwrap());
        assert_eq!(crate::ALPN, fixture["alpn"].as_str().unwrap().as_bytes());
        let mut control = Vec::new();
        write_control(&mut control, &fixture["control"]["json"])
            .await
            .unwrap();
        assert_eq!(hex(&control), fixture["control"]["hex"].as_str().unwrap());
        assert_eq!(&bytes[..8], &42_u64.to_be_bytes());
        assert_eq!(bytes[20], 1);
        assert_eq!(&bytes[21..25], &[0, 0, 0, 3]);
        assert_eq!(read_object(&mut bytes.as_slice()).await.unwrap(), object);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(read_object(&mut trailing.as_slice()).await.is_err());
        assert!(read_object(&mut &bytes[..bytes.len() - 1]).await.is_err());
        let mut invalid = bytes;
        invalid[20] = 2;
        assert!(read_object(&mut invalid.as_slice()).await.is_err());
    }
}
