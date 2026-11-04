//! Experimental PERI framing, independent of third-party object protocols.
//! 实验 PERI 封装，不依赖第三方对象协议；所有长度在分配前校验。

use crate::{Error, Object};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PREFACE: &[u8; 8] = b"PERI\x00\x01\x00\x00";
const MAX_CONTROL: usize = 16 * 1024;
pub const MAX_PAYLOAD: usize = 1024 * 1024;
const OBJECT_HEADER: usize = 25;

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
    if object.payload.is_empty() || object.payload.len() > MAX_PAYLOAD {
        return Err(Error::Protocol("object length"));
    }
    let mut bytes = Vec::with_capacity(OBJECT_HEADER + object.payload.len());
    bytes.extend_from_slice(&object.sequence.to_be_bytes());
    bytes.extend_from_slice(&object.timestamp_us.to_be_bytes());
    bytes.extend_from_slice(&object.duration_us.to_be_bytes());
    bytes.push(u8::from(object.independent));
    bytes.extend_from_slice(&(object.payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&object.payload);
    Ok(bytes)
}

pub async fn read_object<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Object, Error> {
    read_object_selected(reader, false)
        .await?
        .ok_or(Error::Protocol("object selection"))
}

pub async fn read_object_selected<R: AsyncRead + Unpin>(
    reader: &mut R,
    independent_only: bool,
) -> Result<Option<Object>, Error> {
    let sequence = reader.read_u64().await?;
    let timestamp_us = reader.read_u64().await?;
    let duration_us = reader.read_u32().await?;
    let flags = reader.read_u8().await?;
    let size = reader.read_u32().await? as usize;
    if flags > 1 || size == 0 || size > MAX_PAYLOAD {
        return Err(Error::Protocol("object header"));
    }
    // 先校验长度和标志再跳过，防止恢复模式绕过头部边界检查；不分配旧参考负载。
    if independent_only && flags == 0 {
        return Ok(None);
    }
    let mut payload = vec![0; size];
    reader.read_exact(&mut payload).await?;
    // 每个单向流只允许一个对象，禁止拼接对象或忽略尾部数据。
    if reader.read(&mut [0]).await? != 0 {
        return Err(Error::Protocol("object trailing data"));
    }
    Ok(Some(Object {
        sequence,
        timestamp_us,
        duration_us,
        independent: flags == 1,
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
