//! Shared NSP application types. No I/O and no MOQT wire redefinitions.
//! NSP 共用的应用层类型，不执行 I/O，也不重新定义 MOQT 线格式。

use serde::{Deserialize, Serialize};
use std::fmt;

pub const NSP_VERSION: u32 = 1;
pub const MAX_CONTROL_BYTES: usize = 16 * 1024;
pub const MAX_CAPABILITIES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Interactive,
    Live,
    Quality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferPolicy {
    #[serde(rename = "targetBufferMs")]
    pub target_ms: u32,
    #[serde(rename = "maxBufferMs")]
    pub maximum_ms: u32,
}

impl Profile {
    /// Default buffer policy, not a capture-to-display latency guarantee.
    /// 默认缓冲策略，不代表采集到显示的延迟保证。
    pub const fn buffer_policy(self) -> BufferPolicy {
        match self {
            Self::Interactive => BufferPolicy {
                target_ms: 35,
                maximum_ms: 100,
            },
            Self::Live => BufferPolicy {
                target_ms: 100,
                maximum_ms: 300,
            },
            Self::Quality => BufferPolicy {
                target_ms: 300,
                maximum_ms: 1000,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extension {
    pub id: String,
    pub version: u32,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// Ordered by the local endpoint's preference.
    /// 按本地端点的偏好顺序排列。
    pub profiles: Vec<Profile>,
    pub codecs: Vec<String>,
    pub extensions: Vec<Extension>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedExtension {
    pub id: String,
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agreement {
    pub version: u32,
    pub profile: Profile,
    pub codecs: Vec<String>,
    pub extensions: Vec<AcceptedExtension>,
    #[serde(flatten)]
    pub buffer: BufferPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NegotiationError {
    InvalidHello(&'static str),
    UnsupportedVersion(u32),
    NoCommonProfile,
    NoCommonCodec,
    RequiredExtension { id: String, version: u32 },
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHello(reason) => write!(f, "invalid NSP hello: {reason}"),
            Self::UnsupportedVersion(version) => write!(f, "unsupported NSP version: {version}"),
            Self::NoCommonProfile => f.write_str("no common media profile"),
            Self::NoCommonCodec => f.write_str("no common codec"),
            Self::RequiredExtension { id, version } => {
                write!(f, "unsupported required extension: {id}@{version}")
            }
        }
    }
}

impl std::error::Error for NegotiationError {}
