//! Deterministic PERI capability selection, independent of transport and codecs.
//! The responder calls `negotiate(local, remote)`; explicit network acknowledgement
//! is required before a media session can become active.
//! PERI 能力协商与具体传输和编解码实现解耦，进入活动状态前需要显式网络确认。

pub mod handshake;
pub mod wire;

use peri_types::{
    AcceptedExtension, Agreement, Hello, MAX_CAPABILITIES, NegotiationError, PERI_VERSION,
};

pub fn validate(hello: &Hello) -> Result<(), NegotiationError> {
    let invalid = NegotiationError::InvalidHello;
    if hello.profiles.is_empty() || hello.profiles.len() > MAX_CAPABILITIES {
        return Err(invalid("profile count"));
    }
    for (i, profile) in hello.profiles.iter().enumerate() {
        if hello.profiles[..i].contains(profile) {
            return Err(invalid("duplicate profile"));
        }
    }
    if hello.codecs.is_empty() || hello.codecs.len() > MAX_CAPABILITIES {
        return Err(invalid("codec count"));
    }
    for (i, codec) in hello.codecs.iter().enumerate() {
        if codec.is_empty() || codec.len() > 128 || hello.codecs[..i].contains(codec) {
            return Err(invalid("invalid or duplicate codec"));
        }
    }
    if hello.extensions.len() > MAX_CAPABILITIES {
        return Err(invalid("extension count"));
    }
    for (i, extension) in hello.extensions.iter().enumerate() {
        let id = extension.id.as_bytes();
        if id.is_empty()
            || id.len() > 64
            || !id[0].is_ascii_lowercase()
            || !id
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".-".contains(c))
            || extension.version == 0
            || hello.extensions[..i].iter().any(|e| e.id == extension.id)
        {
            return Err(invalid("invalid or duplicate extension"));
        }
    }
    Ok(())
}

pub fn negotiate(local: &Hello, remote: &Hello) -> Result<Agreement, NegotiationError> {
    validate(local)?;
    validate(remote)?;
    for endpoint in [local, remote] {
        if endpoint.version != PERI_VERSION {
            return Err(NegotiationError::UnsupportedVersion(endpoint.version));
        }
    }
    let profile = local
        .profiles
        .iter()
        .find(|p| remote.profiles.contains(p))
        .copied()
        .ok_or(NegotiationError::NoCommonProfile)?;
    let codecs: Vec<String> = local
        .codecs
        .iter()
        .filter(|c| remote.codecs.contains(c))
        .cloned()
        .collect();
    if codecs.is_empty() {
        return Err(NegotiationError::NoCommonCodec);
    }
    for (a, b) in [(local, remote), (remote, local)] {
        for extension in &a.extensions {
            if extension.required
                && !b
                    .extensions
                    .iter()
                    .any(|e| e.id == extension.id && e.version == extension.version)
            {
                return Err(NegotiationError::RequiredExtension {
                    id: extension.id.clone(),
                    version: extension.version,
                });
            }
        }
    }
    let extensions = local
        .extensions
        .iter()
        .filter(|e| {
            remote
                .extensions
                .iter()
                .any(|r| r.id == e.id && r.version == e.version)
        })
        .map(|e| AcceptedExtension {
            id: e.id.clone(),
            version: e.version,
        })
        .collect();
    Ok(Agreement {
        version: PERI_VERSION,
        profile,
        codecs,
        extensions,
        buffer: profile.buffer_policy(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_types::{Extension, Profile};

    fn hello() -> Hello {
        Hello {
            version: PERI_VERSION,
            profiles: vec![Profile::Live],
            codecs: vec!["application/octet-stream".into()],
            extensions: vec![],
        }
    }

    #[test]
    fn unknown_optional_extension_is_omitted() {
        let local = hello();
        let mut remote = hello();
        remote.extensions.push(Extension {
            id: "future.feature".into(),
            version: 1,
            required: false,
        });
        let agreement = negotiate(&local, &remote).unwrap();
        assert!(agreement.extensions.is_empty());
        assert_eq!(agreement.buffer.target_ms, 100);
        assert_eq!(agreement.buffer.maximum_ms, 300);
    }

    #[test]
    fn required_extension_is_enforced_in_both_directions() {
        let mut remote = hello();
        remote.extensions.push(Extension {
            id: "peri.clock".into(),
            version: 1,
            required: true,
        });
        assert!(matches!(
            negotiate(&hello(), &remote),
            Err(NegotiationError::RequiredExtension { .. })
        ));
        assert!(matches!(
            negotiate(&remote, &hello()),
            Err(NegotiationError::RequiredExtension { .. })
        ));
    }

    #[test]
    fn differing_extension_versions_do_not_satisfy_a_requirement() {
        let mut local = hello();
        let mut remote = hello();
        local.extensions.push(Extension {
            id: "peri.clock".into(),
            version: 1,
            required: true,
        });
        remote.extensions.push(Extension {
            id: "peri.clock".into(),
            version: 2,
            required: false,
        });
        assert!(matches!(
            negotiate(&local, &remote),
            Err(NegotiationError::RequiredExtension { .. })
        ));
    }

    #[test]
    fn no_compatible_media_is_rejected() {
        let mut remote = hello();
        remote.codecs = vec!["video/av1".into()];
        assert_eq!(
            negotiate(&hello(), &remote),
            Err(NegotiationError::NoCommonCodec)
        );
        remote = hello();
        remote.profiles = vec![Profile::Interactive];
        assert_eq!(
            negotiate(&hello(), &remote),
            Err(NegotiationError::NoCommonProfile)
        );
        remote = hello();
        remote.version = 2;
        assert_eq!(
            negotiate(&hello(), &remote),
            Err(NegotiationError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn duplicate_capabilities_are_rejected() {
        let mut remote = hello();
        remote.codecs.push(remote.codecs[0].clone());
        assert!(matches!(
            negotiate(&hello(), &remote),
            Err(NegotiationError::InvalidHello(_))
        ));
    }

    #[test]
    fn responder_preference_selects_common_profile() {
        let mut local = hello();
        let mut remote = hello();
        local.profiles = vec![Profile::Interactive, Profile::Live];
        remote.profiles = vec![Profile::Live, Profile::Interactive];
        assert_eq!(
            negotiate(&local, &remote).unwrap().profile,
            Profile::Interactive
        );
    }
}
