//! Read existing deployment settings and issue compatible OBS tokens. / 读取现有部署配置并签发兼容的主播令牌。
use crate::Error;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub public: IpAddr,
    pub udp: u16,
    pub external: u16,
    pub tcp: u16,
    pub web_external: u16,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub name: String,
    pub resource: String,
    #[serde(default)]
    pub stream_key: String,
    pub tokens: BTreeMap<String, String>,
    #[serde(skip)]
    pub subscribe: String,
}
// The viewer reads only public connection settings and the subscription credential.
// 观看进程只读取连接参数和订阅凭据，独立部署可使用最小化配置。
#[derive(Clone, Deserialize)]
pub struct WatchConfig {
    pub udp: u16,
    pub tcp: u16,
    pub cert: PathBuf,
    pub name: String,
    pub resource: String,
    #[serde(default)]
    tokens: BTreeMap<String, String>,
    #[serde(skip)]
    pub subscribe: String,
}
impl WatchConfig {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let mut cfg: Self = serde_json::from_slice(&private_config(path)?)?;
        cfg.subscribe = std::env::var("PERI_SUBSCRIBE_TOKEN")
            .ok()
            .or_else(|| cfg.tokens.remove("PERI_SUBSCRIBE_TOKEN"))
            .ok_or("missing subscription credential")?;
        if cfg.udp == 0 || cfg.tcp == 0 {
            return Err("ports must be 1..65535".into());
        }
        Ok(cfg)
    }
}
fn private_config(path: &Path) -> Result<Vec<u8>, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err("configuration requires mode 0600 or 0400".into());
        }
    }
    Ok(std::fs::read(path)?)
}
impl Config {
    pub fn path() -> Result<PathBuf, Error> {
        if let Some(path) = std::env::var_os("PERI_CONFIG") {
            return Ok(path.into());
        }
        Ok(
            PathBuf::from(std::env::home_dir().ok_or("home unavailable")?)
                .join(".config/peri/server.json"),
        )
    }
    pub fn load(path: &Path) -> Result<Self, Error> {
        let mut c: Self = serde_json::from_slice(&private_config(path)?)?;
        if c.stream_key.is_empty()
            && c.tokens.get("PERI_PUBLISH_TOKEN").is_none_or(|v| v.is_empty())
        {
            return Err("missing publishing credential: expected stream_key or legacy tokens.PERI_PUBLISH_TOKEN".into());
        }
        c.subscribe = c
            .tokens
            .get("PERI_SUBSCRIBE_TOKEN")
            .ok_or("missing subscribe credential")?
            .clone();
        Ok(c)
    }
    pub fn publish_key(&self) -> String {
        // Preserve legacy credentials rather than silently rotating them during upgrade.
        if self.stream_key.is_empty() {
            return self.tokens.get("PERI_PUBLISH_TOKEN").cloned().unwrap_or_default();
        }
        format!(
            "{:x}",
            Sha256::digest(
                [
                    b"peri-stream-key-v1\0".as_slice(),
                    self.stream_key.as_bytes()
                ]
                .concat()
            )
        )
    }
    pub fn ticket(&self, ip: IpAddr) -> Result<String, Error> {
        if ip.is_unspecified() {
            return Err("use obs-token --public-ip ADDRESS".into());
        }
        let data = serde_json::json!({"address":std::net::SocketAddr::new(ip,self.external).to_string(),"name":self.name,"resource":self.resource,"token":self.publish_key(),"cert":STANDARD.encode(std::fs::read(&self.cert)?)});
        Ok(format!(
            "peri://v1/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&data)?)
        ))
    }
    pub fn save(&self, path: &Path, new: bool) -> Result<(), Error> {
        let dir = path.parent().ok_or("missing directory")?;
        std::fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(&serde_json::to_vec_pretty(self)?)?;
        tmp.as_file().sync_all()?;
        if new {
            tmp.persist_noclobber(path)?;
        } else {
            tmp.persist(path)?;
        }
        Ok(())
    }
}
fn ask(label: &str, default: &str) -> Result<String, Error> {
    print!("{label} [{default}]: ");
    std::io::stdout().flush()?;
    let mut v = String::new();
    if std::io::stdin().read_line(&mut v)? == 0 {
        return Err("input ended".into());
    }
    Ok(if v.trim().is_empty() {
        default.into()
    } else {
        v.trim().into()
    })
}

#[cfg(test)]
mod upgrade_tests {
    use super::*;

    fn legacy() -> serde_json::Value {
        serde_json::json!({
            "public":"127.0.0.1", "udp":4444, "external":4444,
            "tcp":4455, "web_external":4455, "cert":"cert.der", "key":"key.key",
            "name":"localhost", "resource":"live/av1",
            "tokens":{"PERI_PUBLISH_TOKEN":"legacy-publish-credential-1234567890",
                      "PERI_SUBSCRIBE_TOKEN":"legacy-subscribe-credential-1234567890"}
        })
    }

    fn load(value: &serde_json::Value) -> Result<Config, Error> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&serde_json::to_vec(value)?)?;
        Config::load(file.path())
    }

    #[test]
    fn legacy_upgrade_preserves_credentials_and_file() {
        let value = legacy();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let original = serde_json::to_vec(&value).unwrap();
        file.write_all(&original).unwrap();
        let cfg = Config::load(file.path()).unwrap();
        assert_eq!(cfg.publish_key(), value["tokens"]["PERI_PUBLISH_TOKEN"].as_str().unwrap());
        assert_eq!(cfg.subscribe, value["tokens"]["PERI_SUBSCRIBE_TOKEN"].as_str().unwrap());
        assert_eq!(std::fs::read(file.path()).unwrap(), original);
        assert_eq!(cfg.cert, PathBuf::from("cert.der"));
    }

    #[test]
    fn explicit_stream_key_takes_precedence_over_legacy_token() {
        let mut value = legacy();
        value["stream_key"] = "new-stream-key".into();
        let cfg = load(&value).unwrap();
        assert_eq!(cfg.publish_key(), format!("{:x}", Sha256::digest(b"peri-stream-key-v1\0new-stream-key")));
    }

    #[test]
    fn missing_publish_credential_fails_without_default_secret() {
        let mut value = legacy();
        value["tokens"].as_object_mut().unwrap().remove("PERI_PUBLISH_TOKEN");
        assert!(load(&value).err().unwrap().to_string().contains("missing publishing credential"));
    }
}
fn port(label: &str, default: u16) -> Result<u16, Error> {
    let n = ask(label, &default.to_string())?.parse()?;
    if n == 0 {
        return Err("invalid port".into());
    }
    Ok(n)
}
fn sibling() -> Result<PathBuf, Error> {
    Ok(std::env::current_exe()?.with_file_name(format!("peri{}", std::env::consts::EXE_SUFFIX)))
}
pub fn manage(action: &str, args: &[String], path: &Path) -> Result<(), Error> {
    if action == "setup" {
        if path.exists() {
            return Err("configuration already exists".into());
        }
        let dir = path.parent().ok_or("missing directory")?;
        std::fs::create_dir_all(dir)?;
        let public = ask("主播可达 IP", "0.0.0.0")?.parse()?;
        let udp = port("Relay UDP", 4444)?;
        let external = port("公网 UDP", udp)?;
        let tcp = port("网页 TCP", 4455)?;
        let web_external = port("公网 TCP", tcp)?;
        let resource = ask("直播资源", "live/av1")?;
        let stream_key = ask("推流码", "stream")?;
        let cert_text = ask("已有 cert.der 路径或 new", "new")?;
        let (cert, key, name) = if cert_text == "new" {
            let tls = tempfile::Builder::new().prefix("tls-").tempdir_in(dir)?;
            if !std::process::Command::new(sibling()?)
                .arg("init")
                .arg(tls.path())
                .status()?
                .success()
            {
                return Err("TLS init failed".into());
            }
            let tls = tls.keep();
            (
                tls.join("cert.der"),
                tls.join("key.key"),
                "localhost".into(),
            )
        } else {
            let cert = std::fs::canonicalize(cert_text)?;
            let key = std::fs::canonicalize(ask(
                "Relay 私钥",
                &cert.with_file_name("key.key").to_string_lossy(),
            )?)?;
            (cert, key, ask("证书名", "localhost")?)
        };
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|_| "secure random unavailable")?;
        let sub = secret
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Config {
            public,
            udp,
            external,
            tcp,
            web_external,
            cert,
            key,
            name,
            resource,
            stream_key,
            tokens: BTreeMap::from([("PERI_SUBSCRIBE_TOKEN".into(), sub)]),
            subscribe: String::new(),
        }
        .save(path, true)?;
        return Ok(());
    }
    let mut c = Config::load(path)?;
    match action {
        "viewer-config" => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "udp":c.udp,"tcp":c.tcp,"cert":c.cert,"name":c.name,"resource":c.resource
            }))?
        ),
        "obs-token" => {
            let ip = match args {
                [] => c.public,
                [flag, ip] if flag == "--public-ip" => ip.parse()?,
                _ => return Err("obs-token [--public-ip IP]".into()),
            };
            println!("{}", c.ticket(ip)?);
        }
        "relay" => {
            let mut cmd = std::process::Command::new(sibling()?);
            cmd.args(["relay", &format!("0.0.0.0:{}", c.udp)])
                .arg(&c.cert)
                .arg(&c.key)
                .arg(&c.resource)
                .env("PERI_PUBLISH_TOKEN", c.publish_key())
                .env("PERI_SUBSCRIBE_TOKEN", c.subscribe);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                return Err(cmd.exec().into());
            }
            #[cfg(not(unix))]
            if !cmd.status()?.success() {
                return Err("Relay exited with error".into());
            }
        }
        "watch-port" => {
            c.tcp = port("网页监听 TCP", c.tcp)?;
            c.web_external = port("公网 TCP", c.tcp)?;
            c.save(path, false)?;
        }
        "stream-key" => {
            c.stream_key = ask("新推流码", "stream")?;
            c.save(path, false)?;
        }
        "link" if c.stream_key.is_empty() => println!(
            "Legacy publishing credential retained; use obs-token to generate the OBS token."
        ),
        "link" => println!(
            "服务器 peri://{}\n推流码 {}",
            std::net::SocketAddr::new(c.public, c.external),
            c.stream_key
        ),
        _ => {
            return Err(
                "expected setup | relay | watch | obs-token | watch-port | stream-key | link"
                    .into(),
            );
        }
    }
    Ok(())
}
