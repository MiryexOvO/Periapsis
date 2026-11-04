"""Live synthetic YUV -> AV1 worker. Requires av==16.1.0; no prerecorded packets."""
import sys, struct, os
from fractions import Fraction
import av

target_rate = int(os.environ.get('PERI_TEST_BITRATE','300000'))
def make_codec(rate):
    context=av.CodecContext.create('libaom-av1','w')
    context.width,context.height,context.pix_fmt=320,180,'yuv420p'
    context.time_base,context.framerate=Fraction(1,15),Fraction(15,1)
    context.bit_rate,context.gop_size,context.thread_count=rate,60,1
    context.options={'usage':'realtime','cpu-used':'8','lag-in-frames':'0','auto-alt-ref':'0'}
    intra_pct = int(os.environ.get('PERI_MAX_INTRA_PCT', '0'))
    if intra_pct not in (0, 100, 200): raise RuntimeError('PERI_MAX_INTRA_PCT must be 0, 100 or 200')
    if intra_pct: context.options['aom-params'] = f'max-intra-rate={intra_pct}'
    # 恒定码率候选用200ms编码缓冲抑制突发；默认仍保留原有VBR对照。
    rate_control = os.environ.get('PERI_RATE_CONTROL', 'vbr')
    if rate_control not in ('vbr', 'cbr'): raise RuntimeError('PERI_RATE_CONTROL must be vbr or cbr')
    if rate_control == 'cbr':
        context.options.update(minrate=str(rate), maxrate=str(rate), bufsize=str(rate // 5))
    context.open()
    if rate_control == 'cbr' and any(k in context.options for k in ('minrate','maxrate','bufsize')):
        raise RuntimeError('encoder did not consume CBR options')
    return context
pool={rate:make_codec(rate) for rate in (100000,150000,225000,300000)} if os.environ.get('PERI_ENCODER_POOL')=='1' else None
codec=pool[target_rate] if pool is not None else make_codec(target_rate)


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

def exchange(index,force,bitrate):
    global codec,target_rate
    if force not in (0,1) or not 100000<=bitrate<=300000: raise RuntimeError('request bounds')
    if bitrate!=target_rate and not force: raise RuntimeError('rate transition must be independent')
    if pool is not None:
        if bitrate not in pool: raise RuntimeError('unsupported pool rate')
        for rate,context in pool.items():
            codec=context
            packet=encode(index,bool(force) and rate==bitrate)
            if rate==bitrate: selected=packet
        target_rate=bitrate
        return selected
    if bitrate!=target_rate: codec=make_codec(bitrate);target_rate=bitrate
    return encode(index,bool(force))

if len(sys.argv)==3 and sys.argv[1]=='--pool-check':
    if pool is None: raise RuntimeError('pool check needs PERI_ENCODER_POOL=1')
    with open(sys.argv[2],'xb') as output:
        output.write(struct.pack('<4sHH4sHHIIII',b'DKIF',0,32,b'AV01',320,180,15,1,90,0))
        for index in range(90):
            rate=(300000,225000,150000,100000,300000,300000)[index//15]
            force=index in (0,15,30,45,60)
            key,data=exchange(index,force,rate)
            if force and not key: raise RuntimeError('missing pool key')
            output.write(struct.pack('<IQ',len(data),index)+data)
    print('POOL_CHECK frames=90 switches=4 contexts=4')
elif len(sys.argv) == 3 and sys.argv[1] == '--check':
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
        key,data=exchange(index,force,bitrate)
        if not 0 < len(data) <= 1024 * 1024 - 68: raise RuntimeError('packet bounds')
        sys.stdout.buffer.write(struct.pack('>IQB', len(data), index, key) + data)
        sys.stdout.buffer.flush()
