"""Install the pinned Windows PyAV wheel into this project's ignored dist directory."""
import hashlib, io, json, pathlib, platform, sys, urllib.request, zipfile
if sys.platform != 'win32' or platform.machine().lower() not in ('amd64', 'x86_64'):
    raise SystemExit('Use pip install --target dist/live-python av==16.1.0 on this platform')
version = f'cp{sys.version_info.major}{sys.version_info.minor}'
with urllib.request.urlopen('https://pypi.org/pypi/av/16.1.0/json', timeout=30) as response:
    meta = json.load(response)
wheel = next((item for item in meta['urls'] if f'{version}-{version}-win_amd64.whl' in item['filename']), None)
if wheel is None: raise SystemExit('No pinned wheel for this Python version')
with urllib.request.urlopen(wheel['url'], timeout=60) as response:
    data = response.read(100 * 1024 * 1024 + 1)
if len(data) > 100 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != wheel['digests']['sha256']:
    raise SystemExit('Wheel size/hash mismatch')
target = (pathlib.Path(__file__).resolve().parent.parent / 'dist' / 'live-python').resolve()
with zipfile.ZipFile(io.BytesIO(data)) as archive:
    for item in archive.infolist():
        if not (target / item.filename).resolve().is_relative_to(target): raise SystemExit('Invalid wheel path')
    for item in archive.infolist():
        destination = target / item.filename
        if item.is_dir(): destination.mkdir(parents=True, exist_ok=True); continue
        content = archive.read(item)
        if destination.exists() and destination.read_bytes() == content: continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)
print(f'Installed {wheel["filename"]} into {target}')
