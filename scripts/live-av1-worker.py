"""Live synthetic YUV -> AV1 worker. Requires av==16.1.0; no prerecorded packets."""
import sys, struct, os
from fractions import Fraction
import av

target_rate = int(os.environ.get('PERI_TEST_BITRATE','300000'))
codec = av.CodecContext.create('libaom-av1', 'w')
codec.width, codec.height, codec.pix_fmt = 320, 180, 'yuv420p'
codec.time_base = Fraction(1, 15)
codec.framerate = Fraction(15, 1)
codec.bit_rate, codec.gop_size, codec.thread_count = target_rate, 60, 1
codec.options = {'usage': 'realtime', 'cpu-used': '8', 'lag-in-frames': '0', 'auto-alt-ref': '0'}
codec.open()

def make_frame(index):
    frame = av.VideoFrame(320, 180, 'yuv420p')
    for channel, plane in enumerate(frame.planes):
        data = bytearray(plane.buffer_size)
        for y in range(plane.height):
            row = bytes((((x * 17) ^ (y * 31) ^ ((index // 5) * 13)) % 200 + 24 if channel == 0 else 100 + channel * 20) for x in range(plane.width))
            data[y * plane.line_size:y * plane.line_size + plane.width] = row
        plane.update(data)
    return frame

def encode(index, force):
    frame = make_frame(index)
    frame.pts, frame.time_base = index, Fraction(1, 15)
    if force or index == 0:
        frame.pict_type = av.video.frame.PictureType.I
    packets = codec.encode(frame)
    if len(packets) != 1 or packets[0].pts != index:
        raise RuntimeError('expected one low-delay AV1 packet per frame')
    packet = packets[0]
    if (force or index == 0) and not packet.is_keyframe:
        raise RuntimeError('encoder did not honor keyframe request')
    return bool(packet.is_keyframe), bytes(packet)

if len(sys.argv) == 3 and sys.argv[1] == '--check':
    with open(sys.argv[2], 'xb') as output:
        output.write(struct.pack('<4sHH4sHHIIII', b'DKIF', 0, 32, b'AV01', 320, 180, 15, 1, 30, 0))
        raw = open(sys.argv[2] + ".yuv", "xb")
        keys = []
        for index in range(30):
            original = make_frame(index)
            for plane in original.planes:
                data = bytes(plane)
                for y in range(plane.height): raw.write(data[y*plane.line_size:y*plane.line_size+plane.width])
            key, data = encode(index, index == 10)
            if key: keys.append(index)
            output.write(struct.pack('<IQ', len(data), index) + data)
    raw.close()
    assert keys == [0, 10], keys
    print('LIVE_ENCODER_CHECK keys=0,10 frames=30')
else:
    while True:
        request = sys.stdin.buffer.read(13)
        if not request: break
        if len(request) != 13: raise RuntimeError('truncated request')
        index, force, bitrate = struct.unpack('>QBI', request)
        if not 100000 <= bitrate <= 300000: raise RuntimeError('bitrate bounds')
        if bitrate != target_rate:
            if not force: raise RuntimeError('rate transition must be independent')
            codec = av.CodecContext.create('libaom-av1', 'w')
            codec.width, codec.height, codec.pix_fmt = 320, 180, 'yuv420p'
            codec.time_base, codec.framerate = Fraction(1,15), Fraction(15,1)
            codec.bit_rate, codec.gop_size, codec.thread_count = bitrate, 60, 1
            codec.options = {'usage':'realtime','cpu-used':'8','lag-in-frames':'0','auto-alt-ref':'0'}
            codec.open()
            target_rate = bitrate
        if force not in (0, 1): raise RuntimeError('invalid force flag')
        key, data = encode(index, bool(force))
        if not 0 < len(data) <= 1024 * 1024 - 68: raise RuntimeError('packet bounds')
        sys.stdout.buffer.write(struct.pack('>IQB', len(data), index, key) + data)
        sys.stdout.buffer.flush()
