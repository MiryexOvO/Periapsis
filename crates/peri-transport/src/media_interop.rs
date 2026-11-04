//! 原生MoQ对照适配器，复用PERI校验，不定义官方MoQ媒体格式。
//! LiveReceiver限制本层组并发和持有配额；调用方仍负责目录协商、上游缓存与会话生命周期。
use peri_native::{
    Error, Object,
    media::{Catalog, Frame},
};
/// MoQ帧负载承载相同媒体对象，不重新编码或修改AV1字节。
pub fn encode_frame(frame: &Frame, catalog: &Catalog) -> Result<Vec<u8>, Error> {
    frame.to_object(catalog)?.encode_packet()
}
/// 恢复公共Frame供相同解码路径使用；不确认参考帧已解码。
/// 完整包接收前的资源边界仍须由MoQ订阅方设置。
pub async fn decode_frame(bytes: &[u8], catalog: &Catalog) -> Result<Frame, Error> {
    Frame::from_object(Object::decode_packet(bytes).await?, catalog)
}

mod live;
pub use live::{LiveEvent, LiveFrame, LiveLimits, LiveReceiver, ReceiveError};
