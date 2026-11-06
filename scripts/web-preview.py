"""Serve a shared PERI video preview. / 提供共享的 PERI 视频预览。"""
import argparse
from functools import partial
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import subprocess
import tempfile
import time

PAGE = """<!doctype html><html lang="zh-CN"><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>PERI 直播预览</title>
<style>body{background:#111;color:#eee;font:1rem system-ui;max-width:64rem;margin:2rem auto;padding:0 1rem}img{width:100%;background:#222}</style>
<h1>PERI 直播预览</h1><img alt="直播画面" hidden><p role="status">等待直播画面…</p>
<script>
const frame=document.querySelector('img'), statusLine=document.querySelector('p');
let previous;
async function refresh(){
  try {
    const response=await fetch('/frame.jpg',{cache:'no-store'});
    if(!response.ok)throw Error();
    const url=URL.createObjectURL(await response.blob());
    frame.src=url;
    if(previous)URL.revokeObjectURL(previous);
    previous=url; frame.hidden=false; statusLine.textContent='';
  } catch {frame.hidden=true;statusLine.textContent='等待直播画面…';}
  setTimeout(refresh,frame.hidden?1000:200);
}
refresh();
</script></html>""".encode()


class Viewer(BaseHTTPRequestHandler):
    def __init__(self, *args, snapshot, **kwargs):
        self.snapshot = snapshot
        super().__init__(*args, **kwargs)

    def do_GET(self):
        if self.path == "/":
            content, mime = PAGE, "text/html; charset=utf-8"
        elif self.path == "/frame.jpg":
            # Hide stale frames when the publisher stops. / 停播后隐藏过期画面。
            try:
                with self.snapshot.open("rb") as image:
                    if time.time() - os.fstat(image.fileno()).st_mtime > 3:
                        raise FileNotFoundError
                    content = image.read()
            except FileNotFoundError:
                self.send_error(503)
                return
            mime = "image/jpeg"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        self.wfile.write(content)

    def log_request(self, code="-", size="-"):
        # Avoid per-frame access logs. / 避免逐帧刷写访问日志。
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("address", "server_name", "cert", "resource"):
        parser.add_argument(key)
    parser.add_argument("--peri", default="peri")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument("--frames", type=int, default=10000, choices=range(1, 10001), metavar="1..10000")
    parser.add_argument("--follow", action="store_true",
                        help="keep the same viewing URL across publisher sessions")
    args = parser.parse_args()
    if not os.environ.get("PERI_SUBSCRIBE_TOKEN"):
        parser.error("PERI_SUBSCRIBE_TOKEN is required")
    with tempfile.TemporaryDirectory() as directory:
        handler = partial(Viewer, snapshot=Path(directory) / "frame.jpg")
        with ThreadingHTTPServer(("127.0.0.1", args.port), handler) as server:
            server.timeout = 0.25
            env = dict(os.environ, PERI_PREVIEW_DIR=directory)
            env.pop("PERI_PUBLISH_TOKEN", None)
            command = [args.peri, "play", args.address, args.server_name, args.cert,
                       args.resource, str(args.frames), "--preview"]
            print(f"http://127.0.0.1:{server.server_port}/", flush=True)
            delay = 1
            while True:
                started = time.monotonic()
                with subprocess.Popen(command, env=env) as player:
                    try:
                        while player.poll() is None:
                            server.handle_request()
                    finally:
                        if player.poll() is None:
                            player.terminate()
                        player.wait()
                if not args.follow:
                    return player.returncode
                # Keep serving the waiting page while the relay has no publisher.
                # Relay 暂无主播时，重试间隔内继续提供等待页面。
                if player.returncode == 0 or time.monotonic() - started >= 30:
                    delay = 1
                retry_at = time.monotonic() + delay
                while time.monotonic() < retry_at:
                    server.handle_request()
                delay = min(5, delay * 2)



if __name__ == "__main__":
    raise SystemExit(main())
