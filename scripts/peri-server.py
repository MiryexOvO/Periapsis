"""Configure and run a PERI server. / 配置并运行 PERI 服务。"""
import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
CONFIG = Path.home() / ".config" / "peri" / "server.json"
PUBLISH = "PERI_PUBLISH_TOKEN"
SUBSCRIBE = "PERI_SUBSCRIBE_TOKEN"


def publish_token(key):
    # Encode the user key for the existing PERI authentication field.
    # 将用户推流码编码为现有 PERI 发布认证字段。
    if not isinstance(key, str) or not 1 <= len(key.encode("utf8")) <= 128 or not key.isprintable():
        raise ValueError("推流码应为 1 到 128 字节的可打印文本")
    return hashlib.sha256(b"peri-stream-key-v1\0" + key.encode("utf8")).hexdigest()


def port(value):
    number = int(value)
    if not 1 <= number <= 65535:
        raise ValueError("端口范围为 1..65535")
    return number


def prompt(label, default=None):
    value = input(f"{label}" + (f" [{default}]" if default else "") + "：").strip()
    return value or default


def tls_files(binary):
    # Prefer complete pairs in known deployment locations. / 优先选用常见部署目录中的完整证书对。
    # Recover certificates made before a previous setup could save its config.
    # 优先找回上次配置中断前生成的完整证书对。
    generated = sorted(CONFIG.parent.glob("tls-*"), key=lambda d: d.stat().st_mtime, reverse=True)
    dirs = (Path.cwd(), Path("/etc/peri"), *generated, ROOT / "dist/peri-local", CONFIG.parent / "tls")
    pair = next((d for d in dirs if all((d / f).is_file() for f in ("cert.der", "key.key"))), None)
    suggested = pair or Path.cwd()
    value = prompt("Relay 证书路径（输入 new 创建新证书对）",
                   str(suggested / "cert.der") if pair or (suggested / "cert.der").exists() else "new")
    if value != "new":
        cert = Path(value).expanduser().resolve()
        if not cert.is_file():
            raise ValueError(f"找不到证书 {cert}；请填写已有证书路径，或在证书提示处输入 new")
        suggested_key = cert.with_name("key.key")
        if not suggested_key.is_file():
            print(f"证书旁未找到 {suggested_key}。可填写配套私钥的实际路径，或输入 new 创建新证书对并重新分发推流码。")
        value = prompt("配套私钥路径", str(suggested_key))
        if value != "new":
            key = Path(value).expanduser().resolve()
            if not key.is_file():
                raise ValueError(f"找不到配套私钥 {key}；请重新运行 setup，填写实际路径或输入 new")
            return cert, key, prompt("证书服务器名", "localhost")
    CONFIG.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="tls-", dir=CONFIG.parent))
    subprocess.run([binary, "init", str(directory)], check=True)
    return directory / "cert.der", directory / "key.key", "localhost"


def setup(args, binary):
    if CONFIG.exists():
        raise ValueError(f"已有配置 {CONFIG}，端口可直接编辑，令牌会保留")
    public = str(ipaddress.IPv4Address(prompt("服务器 IPv4（0.0.0.0 监听全部接口，公网地址可在生成推流码时填写）", "0.0.0.0")))
    udp = port(prompt("Relay 监听 UDP 端口", "4444"))
    tcp = port(prompt("网页监听 TCP 端口"))
    external = args.public_udp or udp
    web_external = args.public_tcp or tcp
    resource = prompt("直播资源名", "live/av1")
    if not resource.isascii() or len(resource) > 256 or any(not (c.isalnum() or c in "/_-.") for c in resource):
        raise ValueError("资源名无效")
    stream_key = prompt("OBS 推流码", "stream")
    publish_token(stream_key)
    existing = {}
    env_file = Path("/etc/peri/relay.env")
    if env_file.is_file():
        for line in env_file.read_text().splitlines():
            k, _, v = line.partition("=")
            if k in (PUBLISH, SUBSCRIBE):
                existing[k] = v
    candidates = (os.environ.get(SUBSCRIBE, ""), existing.get(SUBSCRIBE, ""))
    sub_token = next((v for v in candidates if 32 <= len(v) <= 128 and
                      all("!" <= ch <= "~" for ch in v) and v != publish_token(stream_key)), None)
    tokens = {SUBSCRIBE: sub_token or secrets.token_hex(32)}
    cert, key, name = tls_files(binary)
    config = dict(public=public, external=external, udp=udp, tcp=tcp, web_external=web_external,
                  cert=str(cert), key=str(key), name=name, resource=resource, stream_key=stream_key, tokens=tokens)
    write_config(config)
    print(f"配置已保存 {CONFIG}；以后使用 relay、watch、link。")


def write_config(config, replace=False):
    CONFIG.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    # Atomically install a complete mode-0600 file without replacing a racing setup.
    # 原子安装完整的 0600 配置，不覆盖并发生成的凭据。
    with tempfile.NamedTemporaryFile("w", dir=CONFIG.parent, delete=False, encoding="utf8") as f:
        tmp = Path(f.name)
    try:
        with tmp.open("w", encoding="utf8") as f:
            json.dump(config, f, indent=2)
            f.flush()
            os.fsync(f.fileno())
        if replace:
            os.replace(tmp, CONFIG)
        else:
            os.link(tmp, CONFIG)
    finally:
        tmp.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("setup", "relay", "watch", "link", "stream-key", "watch-port"))
    parser.add_argument("--peri", help="path to the peri executable")
    parser.add_argument("--public-udp", type=port, help="advertised UDP port if different (setup)")
    parser.add_argument("--public-tcp", type=port, help="advertised HTTP port if different (setup or watch-port)")
    parser.add_argument("--public-ip", type=ipaddress.IPv4Address, help="client-facing IPv4 for link or watch")
    args = parser.parse_args()
    # Migrate the old private file without copying or rotating tokens.
    # 迁移旧配置，保留权限与原令牌。
    legacy = CONFIG.with_name("nat.json")
    if not CONFIG.exists() and legacy.is_file():
        os.link(legacy, CONFIG)
        legacy.unlink()
    exe = "peri.exe" if os.name == "nt" else "peri"
    binary = args.peri or next((str(p) for p in (ROOT / "target/release" / exe, ROOT / "target/debug" / exe,
                                                 Path("/opt/peri/bin/peri")) if p.is_file()), "peri")
    if args.action == "setup":
        setup(args, binary)
        return
    if not CONFIG.is_file():
        parser.error("先运行 setup")
    if os.name == "posix" and CONFIG.stat().st_mode & 0o077:
        parser.error(f"凭据文件权限过宽，请运行 chmod 600 {CONFIG}")
    c = json.loads(CONFIG.read_text(encoding="utf8"))
    if args.action == "watch-port":
        listen = port(prompt("网页监听 TCP 端口", str(c["tcp"])))
        default_public = listen if c["web_external"] == c["tcp"] else c["web_external"]
        public = args.public_tcp or port(prompt("观众访问的公网 TCP 端口", str(default_public)))
        c.update(tcp=listen, web_external=public)
        write_config(c, replace=True)
        print(f"已保存：网页监听 TCP {listen}，公网 TCP {public}。请重启 watch。")
        return
    if args.action == "stream-key":
        key = prompt("新的 OBS 推流码", "stream")
        if publish_token(key) == c["tokens"][SUBSCRIBE]:
            parser.error("请选择另一推流码，以便区分发布和订阅凭据")
        c["stream_key"] = key
        write_config(c, replace=True)
        print("推流码已保存，重启 Relay 后生效。")
        return
    host = str(args.public_ip or c["public"])
    if args.action == "link":
        suffix = "" if c["external"] == 4444 else f":{c['external']}"
        print(f"服务器：peri://{host}{suffix}\n推流码：{c.get('stream_key', 'stream')}")
        if host == "0.0.0.0":
            print("远程 OBS 请把 0.0.0.0 换成服务器可访问的 IP。")
        return
    env = dict(os.environ, **{SUBSCRIBE: c["tokens"][SUBSCRIBE]})
    if args.action == "relay":
        env[PUBLISH] = publish_token(c.get("stream_key", "stream"))
        command = [binary, "relay", f"0.0.0.0:{c['udp']}", c["cert"], c["key"], c["resource"]]
    else:
        env.pop(PUBLISH, None)
        command = [sys.executable, str(ROOT / "scripts/program-av.py"), "watch", f"127.0.0.1:{c['udp']}",
                   c["name"], c["cert"], c["resource"], "--peri", binary, "--bind", "0.0.0.0", "--port", str(c["tcp"])]
        if host == "0.0.0.0":
            print(f"网页监听全部接口；观众使用服务器可访问的 IP 和 TCP 端口 {c['web_external']}", flush=True)
        else:
            print(f"观众地址 http://{host}:{c['web_external']}/", flush=True)
    os.execvpe(command[0], command, env)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, EOFError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
