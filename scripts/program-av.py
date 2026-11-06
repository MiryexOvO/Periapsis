"""Bridge an OBS WebM program to PERI and serve a native HTML video element.
将 OBS WebM 节目拆成 PERI 对象，并向浏览器 video 元素提供音视频。
"""
import argparse
from contextlib import closing
from fractions import Fraction
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import queue
from pathlib import Path
import shutil
import struct
import subprocess
import threading
import time

import av

CLOCK = Fraction(1, 1_000_000)
HEADER = struct.Struct(">BQII")
MAX_PACKET = 1024 * 1024 - 68


def pipe_process(args, mode, **pipes):
    env = os.environ.copy()
    env.pop("PERI_PUBLISH_TOKEN" if mode == "av-subscribe" else "PERI_SUBSCRIBE_TOKEN", None)
    return subprocess.Popen([args.peri, mode, args.address, args.name, args.cert, args.resource],
                            env=env, **pipes)


def publish(args):
    # Listen only on loopback for the local OBS program feed.
    # OBS 节目输入只监听回环地址。
    source = "pipe:0" if args.stdin_program else args.file or "tcp://127.0.0.1:4455?listen=1"
    # The local pipe already declares both codecs. Limit startup probing on that path.
    # 本机管道已经声明两条轨道的编码，仅在这条路径限制开播探测。
    options = {"probesize": "262144", "analyzeduration": "250000"} if args.stdin_program else {}
    with av.open(source, format="nut" if args.stdin_program else "matroska", options=options) as container:
        video, = container.streams.video
        audio, = container.streams.audio
        v, a = video.codec_context, audio.codec_context
        if v.name not in ("av1", "libdav1d") or a.name not in ("opus", "libopus"):
            raise ValueError("expected AV1 video and Opus audio")
        tracks = [dict(id=1, kind="video", codec="video/av1", init=list(v.extradata or b""),
                       width=v.width, height=v.height, sample_rate=None, channels=None),
                  dict(id=2, kind="audio", codec="audio/opus", init=list(a.extradata or b""),
                       width=None, height=None, sample_rate=a.sample_rate, channels=a.channels)]
        if not (0 < v.width <= 2560 and 0 < v.height <= 1440):
            raise ValueError("program dimensions exceed 2560x1440")
        if a.channels not in (1, 2):
            raise ValueError("expected mono or stereo program audio")
        catalog = json.dumps(dict(tracks=tracks)).encode()
        with pipe_process(args, "av-publish", stdin=subprocess.PIPE) as child:
            with closing(child.stdin) as sink:
                sink.write(struct.pack(">I", len(catalog)) + catalog)
                start = None
                sample_time = time.monotonic()
                video_frames = video_bytes = 0
                for packet in container.demux(video, audio):
                    if not packet.size or packet.pts is None:
                        continue
                    if packet.size > MAX_PACKET:
                        raise ValueError("encoded packet exceeds PERI object limit")
                    pts = int(packet.pts * packet.time_base / CLOCK)
                    if start is None:
                        if packet.stream != video or not packet.is_keyframe:
                            continue
                        start, wall = pts, time.monotonic()
                    if pts < start:
                        continue
                    timestamp = pts - start
                    if args.file:
                        time.sleep(max(0, wall + timestamp / 1e6 - time.monotonic()))
                    tag = 3 if packet.stream == audio else (1 if packet.is_keyframe else 2)
                    duration = int(packet.duration * packet.time_base / CLOCK)
                    # Local bridge fixes video at 60 fps and Opus at 20 ms.
                    # 本机桥接固定视频为 60 fps、Opus 为 20 ms，NUT 可省略包时长。
                    if not duration and args.stdin_program:
                        duration = 20000 if tag == 3 else 16667
                    if not duration:
                        raise ValueError("input packet has no duration")
                    sink.write(HEADER.pack(tag, timestamp, duration, packet.size))
                    sink.write(bytes(packet))
                    sink.flush()
                    if tag != 3:
                        video_frames += 1
                        video_bytes += packet.size
                        now = time.monotonic()
                        elapsed = now - sample_time
                        if elapsed >= 5:
                            print(f"PERI_PROGRAM_VIDEO {v.width}x{v.height} "
                                  f"{video_frames / elapsed:.1f} fps "
                                  f"{video_bytes * 8 / elapsed / 1e6:.2f} Mbps", flush=True)
                            sample_time, video_frames, video_bytes = now, 0, 0
            if child.wait():
                raise RuntimeError("PERI publisher failed")


def exact(pipe, length):
    data = pipe.read(length)
    if len(data) != length:
        raise EOFError("subscription ended")
    return data


class ProgramFeed:
    """Share a single QUIC subscription across HTTP viewers. / 网页观众共享一个 QUIC 订阅。"""
    def __init__(self, config):
        self.child = pipe_process(config, "av-subscribe", stdout=subprocess.PIPE)
        self.lock = threading.Lock()
        self.viewers = set()
        self.catalog = None
        self.error = None
        self.started = False

    def join(self):
        # Bound queued references per viewer. A stalled HTTP client reconnects separately.
        # 限制单观众排队引用数，慢客户端独立重连。
        packets = queue.Queue(maxsize=64)
        with self.lock:
            if self.error:
                raise EOFError(self.error)
            self.viewers.add(packets)
            if not self.started:
                self.started = True
                threading.Thread(target=self.read, daemon=True).start()
        return packets

    def read(self):
        try:
            with self.child.stdout as source:
                length, = struct.unpack(">I", exact(source, 4))
                if not 0 < length <= 16384:
                    raise ValueError("invalid catalog")
                video, audio = sorted(json.loads(exact(source, length))["tracks"], key=lambda t: t["id"])
                if video["width"] > 2560 or video["height"] > 1440 or audio["channels"] not in (1, 2):
                    raise ValueError("unsupported program catalog")
                self.catalog = video, audio
                while True:
                    tag, pts, duration, size = HEADER.unpack(exact(source, HEADER.size))
                    if not 0 < size <= MAX_PACKET or tag not in (1, 2, 3):
                        raise ValueError("invalid PERI adapter packet")
                    packet = tag, pts, duration, exact(source, size)
                    with self.lock:
                        for viewer in tuple(self.viewers):
                            if viewer.full():
                                self.viewers.remove(viewer)
                            else:
                                viewer.put_nowait(packet)
        except (EOFError, OSError, ValueError, KeyError, TypeError) as error:
            with self.lock:
                self.error = str(error) or "subscription ended"
        finally:
            self.child.terminate()
            self.child.wait()

    def next_packet(self, viewer):
        while True:
            with self.lock:
                if viewer not in self.viewers:
                    raise EOFError("viewer fell behind")
                if self.error and viewer.empty():
                    raise EOFError(self.error)
            try:
                return viewer.get(timeout=.25)
            except queue.Empty:
                continue


class ViewerSocket:
    """Keep the first HTTP write failure across WebM finalization.
    WebM 收尾时保留首次 HTTP 写入错误，避免重复写断开的连接。
    """
    def __init__(self, stream):
        self.stream = stream
        self.failed = None

    def writable(self):
        return True

    def write(self, data):
        if self.failed is not None:
            return len(data)
        try:
            return self.stream.write(data)
        except OSError as error:
            self.failed = error
            raise


class Viewer(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            body = """<!doctype html><html lang="zh-CN"><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>PERI 直播</title>
<style>*{box-sizing:border-box}body{margin:0;background:#101114;color:#eee;font:16px system-ui}
main{width:min(100%,2560px);margin:auto}video{display:block;width:100%;height:min(100dvh,56.25vw);
background:#000;object-fit:contain}p{margin:12px 16px}</style>
<main><video controls playsinline muted autoplay src="/live.webm"></video>
<p role="status">正在连接直播…</p></main>
<script>
const v=document.querySelector('video'),statusLine=document.querySelector('p');
let retry;
v.onplaying=()=>{clearTimeout(retry);statusLine.textContent=v.muted?'直播中 · 可在播放器中开启声音':''};
v.onvolumechange=()=>{if(!v.paused) v.onplaying()};
v.onwaiting=()=>{statusLine.textContent='正在等待直播画面…'};
// Catch up gradually only when surplus media has accumulated.
// 只在已缓冲媒体积压时轻微追帧；缓冲不足时恢复原速。
v.preservesPitch=true;
v.ontimeupdate=()=>{
  if(v.paused || v.seeking || !v.buffered.length) return;
  const lead=v.buffered.end(v.buffered.length-1)-v.currentTime;
  if(lead>0.65 && v.playbackRate===1) v.playbackRate=1.04;
  if(lead<0.4 && v.playbackRate===1.04) v.playbackRate=1;
};
function reconnect(){
  clearTimeout(retry);
  statusLine.textContent='直播暂时不可用，正在重新连接…';
  retry=setTimeout(()=>{v.load();v.play().catch(()=>{})},3000);
}
v.onended=reconnect;
v.onerror=()=>{if(!v.canPlayType('video/webm; codecs="av01.0.12M.08,opus"')){
  statusLine.textContent='当前浏览器无法播放 AV1 / Opus WebM，请换用支持该格式的浏览器。';
}else reconnect()};
</script></html>""".encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path != "/live.webm":
            self.send_error(404)
            return
        if not self.server.slots.acquire(blocking=False):
            self.send_error(503, "viewer limit reached")
            return
        try:
            with self.server.feed_lock:
                feed = self.server.feed
                if feed is None or feed.error:
                    feed = self.server.feed = ProgramFeed(self.server.config)
                viewer = feed.join()
            try:
                first = feed.next_packet(viewer)
                while first[0] != 1:
                    first = feed.next_packet(viewer)
                video, audio = feed.catalog
                self.log_message("subscribed: AV1 %sx%s + Opus", video["width"], video["height"])
                self.send_response(200)
                self.send_header("Content-Type", "video/webm")
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                self.remux(feed, viewer, first, video, audio)
            finally:
                with self.server.feed_lock, feed.lock:
                    feed.viewers.discard(viewer)
                    if not feed.viewers and self.server.feed is feed:
                        self.server.feed = None
                        feed.child.terminate()
        except (EOFError, OSError, ValueError, av.error.FFmpegError) as error:
            sink = getattr(self, "socket_sink", None)
            if sink and sink.failed is not None:
                self.log_message("viewer HTTP write ended: %s", sink.failed)
            else:
                self.log_message("stream ended: %s", error)
            if not getattr(self, "streaming", False):
                self.send_error(503)
        finally:
            self.server.slots.release()

    def remux(self, feed, viewer, first, video, audio):
        self.streaming = True
        self.connection.settimeout(5)
        socket_sink = self.socket_sink = ViewerSocket(self.wfile)
        with av.open(socket_sink, "w", format="webm",
                     options={"live": "1", "cluster_time_limit": "100", "flush_packets": "1"}) as out:
            vs = out.add_stream("libaom-av1", rate=max(1, min(120, round(1_000_000 / first[2]))))
            vs.codec_context.thread_count = 1
            vs.width, vs.height = video["width"], video["height"]
            vs.codec_context.extradata = bytes(video["init"])
            aus = out.add_stream("libopus", rate=audio["sample_rate"])
            aus.layout = "mono" if audio["channels"] == 1 else "stereo"
            aus.codec_context.extradata = bytes(audio["init"])
            origin = first[1]
            self.log_message("first video keyframe: %d bytes", len(first[3]))
            packet = first
            while True:
                tag, pts, duration, payload = packet
                if pts < origin:
                    packet = feed.next_packet(viewer)
                    continue
                pkt = av.Packet(payload)
                # Both tracks share the first keyframe's origin; preserve source timing.
                # 两轨共用首个关键帧的时间原点，保留源端的音画相对时间。
                pkt.stream = aus if tag == 3 else vs
                pkt.time_base = CLOCK
                pkt.pts = pkt.dts = pts - origin
                pkt.duration, pkt.is_keyframe = duration, tag != 2
                try:
                    out.mux(pkt)
                except av.error.FFmpegError:
                    if socket_sink.failed is not None:
                        raise socket_sink.failed from None
                    raise
                if socket_sink.failed is not None:
                    raise socket_sink.failed
                packet = feed.next_packet(viewer)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("publish", "watch"))
    for name in ("address", "name", "cert", "resource"):
        parser.add_argument(name)
    parser.add_argument("--peri", default="peri")
    sources = parser.add_mutually_exclusive_group()
    sources.add_argument("--stdin-program", action="store_true", help="read a live program from stdin")
    sources.add_argument("--file", type=Path, help="pace a local AV1/Opus WebM fixture")
    parser.add_argument("--bind", help="HTTP listen address")
    parser.add_argument("--port", type=int, default=0, help="HTTP listen port; 0 selects an available port")
    args = parser.parse_args()
    if args.mode == "publish":
        publish(args)
    else:
        # Verify the native subscriber before accepting HTTP viewers.
        # 开放观看端口前验证原生订阅程序，避免旧版本对每个观众重复返回 503。
        binary = shutil.which(args.peri)
        if binary is None:
            parser.error(f"找不到原生订阅程序 {args.peri}，请用 --peri 指定其路径")
        try:
            help_result = subprocess.run([binary, "--help"], capture_output=True, timeout=5)
        except (OSError, subprocess.TimeoutExpired) as error:
            parser.error(f"无法验证订阅程序 {binary}：{error}")
        if help_result.returncode or b"av-subscribe" not in help_result.stdout:
            parser.error(f"{binary} 未提供 av-subscribe；请同步原生源码并运行 "
                         "cargo build --release --locked -p peri-native --bin peri，"
                         "然后用 --peri 指定新程序")
        args.peri = binary
        print(f"PERI viewer subscriber: {binary}", flush=True)
        if not 0 <= args.port <= 65535:
            parser.error("--port must be between 0 and 65535")
        if not os.environ.get("PERI_SUBSCRIBE_TOKEN"):
            parser.error("missing PERI_SUBSCRIBE_TOKEN; use peri-server.py watch")
        host, sep, relay_port = args.address.rpartition(":")
        wildcard = sep and host == "0.0.0.0"
        if wildcard:
            args.address = f"127.0.0.1:{relay_port}"
        bind = args.bind or ("0.0.0.0" if wildcard else "127.0.0.1")
        with ThreadingHTTPServer((bind, args.port), Viewer) as server:
            server.config = args
            server.feed = None
            server.feed_lock = threading.Lock()
            server.slots = threading.BoundedSemaphore(4)
            print(f"http://{server.server_address[0]}:{server.server_port}/", flush=True)
            server.serve_forever()


if __name__ == "__main__":
    main()
