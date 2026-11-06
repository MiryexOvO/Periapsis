"""Bridge an OBS WebM program to PERI and serve a native HTML video element.
将 OBS WebM 节目拆成 PERI 对象，并向浏览器 video 元素提供音视频。
"""
import argparse
from contextlib import closing
from fractions import Fraction
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
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
    with av.open(source, format="nut" if args.stdin_program else "matroska") as container:
        video, = container.streams.video
        audio, = container.streams.audio
        v, a = video.codec_context, audio.codec_context
        if v.name not in ("av1", "libdav1d") or a.name not in ("opus", "libopus"):
            raise ValueError("expected AV1 video and Opus audio")
        tracks = [dict(id=1, kind="video", codec="video/av1", init=list(v.extradata or b""),
                       width=v.width, height=v.height, sample_rate=None, channels=None),
                  dict(id=2, kind="audio", codec="audio/opus", init=list(a.extradata or b""),
                       width=None, height=None, sample_rate=a.sample_rate, channels=a.channels)]
        if not (0 < v.width <= 1920 and 0 < v.height <= 1080):
            raise ValueError("program dimensions exceed 1920x1080")
        if a.channels not in (1, 2):
            raise ValueError("expected mono or stereo program audio")
        catalog = json.dumps(dict(tracks=tracks)).encode()
        with pipe_process(args, "av-publish", stdin=subprocess.PIPE) as child:
            with closing(child.stdin) as sink:
                sink.write(struct.pack(">I", len(catalog)) + catalog)
                start = None
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
                    # Local bridge fixes video at 30 fps and Opus at 20 ms.
                    # 本机桥接固定视频为 30 fps、Opus 为 20 ms，NUT 可省略包时长。
                    if not duration and args.stdin_program:
                        duration = 20000 if tag == 3 else 33333
                    if not duration:
                        raise ValueError("input packet has no duration")
                    sink.write(HEADER.pack(tag, timestamp, duration, packet.size))
                    sink.write(bytes(packet))
                    sink.flush()
            if child.wait():
                raise RuntimeError("PERI publisher failed")


def exact(pipe, length):
    data = pipe.read(length)
    if len(data) != length:
        raise EOFError("subscription ended")
    return data


class Viewer(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            body = ('<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width">'
                    '<title>PERI 直播</title><video controls playsinline preload="none" '
                    'style="max-width:100%" src="/live.webm"></video>').encode()
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
            self.send_error(503)
            return
        try:
            with pipe_process(self.server.config, "av-subscribe", stdout=subprocess.PIPE) as child:
                try:
                    length, = struct.unpack(">I", exact(child.stdout, 4))
                    if not 0 < length <= 16384:
                        raise ValueError("invalid catalog")
                    video, audio = sorted(json.loads(exact(child.stdout, length))["tracks"],
                                          key=lambda t: t["id"])
                    if video["width"] > 1920 or video["height"] > 1080 or audio["channels"] not in (1, 2):
                        raise ValueError("unsupported program catalog")
                    self.send_response(200)
                    self.send_header("Content-Type", "video/webm")
                    self.send_header("Cache-Control", "no-store")
                    self.end_headers()
                    self.remux(child.stdout, video, audio)
                finally:
                    child.terminate()
        except (EOFError, OSError, ValueError) as error:
            self.log_message("stream ended: %s", error)
            if not getattr(self, "streaming", False):
                self.send_error(503)
        finally:
            self.server.slots.release()

    def remux(self, source, video, audio):
        self.streaming = True
        self.connection.settimeout(5)
        with av.open(self.wfile, "w", format="webm",
                     options={"live": "1", "cluster_time_limit": "250"}) as out:
            vs = out.add_stream("libaom-av1", rate=30)
            vs.width, vs.height = video["width"], video["height"]
            vs.codec_context.extradata = bytes(video["init"])
            aus = out.add_stream("libopus", rate=audio["sample_rate"])
            aus.layout = "mono" if audio["channels"] == 1 else "stereo"
            aus.codec_context.extradata = bytes(audio["init"])
            origin = None
            while True:
                tag, pts, duration, size = HEADER.unpack(exact(source, HEADER.size))
                if not 0 < size <= MAX_PACKET or tag not in (1, 2, 3):
                    raise ValueError("invalid PERI adapter packet")
                pkt = av.Packet(exact(source, size))
                if origin is None:
                    origin = pts
                if pts < origin:
                    continue
                # Both tracks share the first keyframe's origin; preserve source timing.
                # 两轨共用首个关键帧的时间原点，保留源端的音画相对时间。
                pkt.stream = aus if tag == 3 else vs
                pkt.time_base = CLOCK
                pkt.pts = pkt.dts = pts - origin
                pkt.duration, pkt.is_keyframe = duration, tag != 2
                out.mux(pkt)


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
            server.slots = threading.BoundedSemaphore(4)
            print(f"http://{server.server_address[0]}:{server.server_port}/", flush=True)
            server.serve_forever()


if __name__ == "__main__":
    main()
