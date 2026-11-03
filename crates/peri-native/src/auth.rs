//! Role-scoped bearer credentials carried only inside the TLS control stream.
//! 凭据仅在 TLS 加密控制流内传输，区分发布和订阅，不进入日志。

use crate::Error;
use subtle::ConstantTimeEq;

pub(crate) fn validate_token(token: &str) -> Result<(), Error> {
    if !(32..=128).contains(&token.len()) || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(Error::Protocol("invalid credential format"));
    }
    Ok(())
}

/// Distinct publish and subscribe credentials for one configured resource.
/// 单个配置资源的独立发布和订阅凭据；不提供 Debug 输出，调用方负责安全存储。
pub struct Credentials {
    publish: String,
    subscribe: String,
}
impl Credentials {
    /// Validate independent credentials without exposing their contents in errors.
    /// 校验独立凭据，错误信息不包含凭据内容。
    pub fn new(publish: String, subscribe: String) -> Result<Self, Error> {
        validate_token(&publish)?;
        validate_token(&subscribe)?;
        if publish == subscribe {
            return Err(Error::Protocol("credentials must differ"));
        }
        Ok(Self { publish, subscribe })
    }
    pub(crate) fn authorize(&self, publishing: bool, provided: Option<&str>) -> Result<(), Error> {
        let provided = provided.ok_or(Error::Protocol("access denied"))?;
        if validate_token(provided).is_err() {
            return Err(Error::Protocol("access denied"));
        }
        let expected = if publishing {
            &self.publish
        } else {
            &self.subscribe
        };
        if bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
            Ok(())
        } else {
            Err(Error::Protocol("access denied"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_are_role_scoped_and_missing_or_invalid_values_fail_closed() {
        let credentials = Credentials::new("p".repeat(48), "s".repeat(48)).unwrap();
        credentials.authorize(true, Some(&"p".repeat(48))).unwrap();
        credentials.authorize(false, Some(&"s".repeat(48))).unwrap();
        assert!(credentials.authorize(false, Some(&"p".repeat(48))).is_err());
        assert!(credentials.authorize(true, Some(&"s".repeat(48))).is_err());
        assert!(credentials.authorize(true, None).is_err());
        assert!(credentials.authorize(true, Some("short")).is_err());
        assert!(Credentials::new("x".repeat(48), "x".repeat(48)).is_err());
    }
}
