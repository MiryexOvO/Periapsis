"""Run a PERI relay behind IPv4 NAT. / 启动共享 IPv4 NAT 后的 PERI 服务。"""
import argparse
import base64
import ipaddress
import json
import os
from pathlib import Path
import secrets
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
CONFIG = Path.home() / ".config" / "peri" / "nat.json"
PUBLISH = "PERI_PUBLISH_TOKEN"
SUBSCRIBE = "PERI_SUBSCRIBE_TOKEN"


def port(value):
    number = int(value)
    if not 1 <= number <= 65535:
        raise ValueError("端口范围为 1..65535")
    return number


def prompt(label, default=None):
    value = input(f"{label}" + (f" [{default}]" if default else "") + "：").strip()
    return value or default


def setup():
    if CONFIG.exists():
        raise ValueError(f"已有配置 {CONFIG}，端口可直接编辑，令牌会保留")
    public = str(ipaddress.IPv4Address(prompt("共享公网 IPv4")))
    external = port(prompt("主播连接的公网 UDP 端口"))
    udp = port(prompt("Relay 监听的内网 UDP 端口", "4444"))
    tcp = port(prompt("网页监听的内网 TCP 端口"))
    web_external = port(prompt("观众连接的公网 TCP 端口", str(tcp)))
    resource = prompt("直播资源名", "live/av1")
    if not resource.isascii() or len(resource) > 256 or any(not (c.isalnum() or c in "/_-.") for c in resource):
        raise ValueError("资源名无效")
    cert = Path(prompt("Relay 证书路径", "./cert.der")).expanduser().resolve(strict=True)
    key = Path(prompt("Relay 私钥路径", str(cert.with_name("key.key")))).expanduser().resolve(strict=True)
    name = prompt("证书服务器名", "localhost")
    # Adopt existing credentials together; never rotate a live relay implicitly.
    # 成对复用已有凭据，避免隐式轮换正在使用的 Relay 令牌。
    existing = {}
    env_file = Path("/etc/peri/relay.env")
    if env_file.is_file():
        for line in env_file.read_text().splitlines():
            k, _, v = line.partition("=")
            if k in (PUBLISH, SUBSCRIBE):
                existing[k] = v
    existing.update({k: os.environ[k] for k in (PUBLISH, SUBSCRIBE) if k in os.environ})
    if not existing:
        existing = {k: secrets.token_hex(32) for k in (PUBLISH, SUBSCRIBE)}
    if (set(existing) != {PUBLISH, SUBSCRIBE} or existing[PUBLISH] == existing[SUBSCRIBE]
            or any(not 32 <= len(t) <= 128 or not all("!" <= c <= "~" for c in t)
                   for t in existing.values())):
        raise ValueError("已有凭据必须为两枚不同的有效令牌")
    config = dict(public=public, external=external, udp=udp, tcp=tcp, web_external=web_external,
                  cert=str(cert), key=str(key), name=name, resource=resource, tokens=existing)
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
        os.link(tmp, CONFIG)
    finally:
        tmp.unlink()
    print(f"配置已保存 {CONFIG}；以后使用 relay、watch、link。")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("setup", "relay", "watch", "link"))
    parser.add_argument("--peri", help="path to the peri executable")
    args = parser.parse_args()
    if args.action == "setup":
        setup()
        return
    if not CONFIG.is_file():
        parser.error("先运行 setup")
    if os.name == "posix" and CONFIG.stat().st_mode & 0o077:
        parser.error(f"凭据文件权限过宽，请运行 chmod 600 {CONFIG}")
    c = json.loads(CONFIG.read_text(encoding="utf8"))
    public_udp = f"{c['public']}:{c['external']}"
    if args.action == "link":
        invite = dict(address=public_udp, name=c["name"], resource=c["resource"],
                      cert=base64.b64encode(Path(c["cert"]).read_bytes()).decode(),
                      token=c["tokens"][PUBLISH])
        print("peri://v1/" + base64.urlsafe_b64encode(json.dumps(invite).encode()).decode().rstrip("="))
        return
    binary = args.peri or next((str(p) for p in (ROOT / "target/release/peri", ROOT / "target/debug/peri",
                                                 Path("/opt/peri/bin/peri")) if p.is_file()), "peri")
    env = dict(os.environ, **{SUBSCRIBE: c["tokens"][SUBSCRIBE]})
    if args.action == "relay":
        env[PUBLISH] = c["tokens"][PUBLISH]
        command = [binary, "relay", f"0.0.0.0:{c['udp']}", c["cert"], c["key"], c["resource"]]
    else:
        env.pop(PUBLISH, None)
        command = [sys.executable, str(ROOT / "scripts/program-av.py"), "watch", f"127.0.0.1:{c['udp']}",
                   c["name"], c["cert"], c["resource"], "--peri", binary, "--bind", "0.0.0.0", "--port", str(c["tcp"])]
        print(f"观众地址 http://{c['public']}:{c['web_external']}/", flush=True)
    os.execvpe(command[0], command, env)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, EOFError) as error:
        sys.exit(str(error))
