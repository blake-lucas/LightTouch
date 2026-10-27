#!/usr/bin/env python3
"""The last release step, run by scripts/build-release.sh once every package is in WORKSPACE/out:
adds the source archives, the licence and the guest tools, checks the guest tools match the ones in
the APK, and writes SHA256SUMS and RELEASE-MANIFEST.json. If out/NOTES.md exists, its CHECKSUMS
placeholder is replaced with the package checksums.

usage: finish.py WORKSPACE VERSION GUEST_TOOLS_DIR
"""
import hashlib, json, os, pathlib, shutil, struct, subprocess, sys, zipfile, zlib

R, VERSION, GUEST = pathlib.Path(sys.argv[1]), sys.argv[2], pathlib.Path(sys.argv[3])
out, app, engine = R / 'out', R / 'app', R / 'engine'


def sha(path):
    h = hashlib.sha256()
    with open(path, 'rb') as s:
        for b in iter(lambda: s.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()


def rev(repo):
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()


def itpack_manifest(path):
    """The first family manifest inside an .itpack (ITPACK01, header length, JSON header, zlib data)."""
    b = path.read_bytes()
    assert b[:8] == b'ITPACK01', path
    n = struct.unpack('<I', b[8:12])[0]
    header, data, off = json.loads(b[12:12 + n]), zlib.decompress(b[12 + n:]), 0
    for e in header['entries']:
        if e['name'].endswith('/manifest.json'):
            return json.loads(data[off:off + e['size']])
        off += e['size']
    raise ValueError('no manifest in ' + str(path))


app_rev, engine_rev = rev(app), rev(engine)
for repo, prefix, name in ((app, 'LightTouch-' + VERSION, 'LightTouch-' + VERSION + '-source.tar.gz'),
                           (engine, 'qemu-ios-lighttouch-' + VERSION, 'qemu-ios-' + VERSION + '-source.tar.gz')):
    subprocess.run(['git', 'archive', '--format=tar.gz', '--prefix=' + prefix + '/', '-o', out / name, 'HEAD'],
                   cwd=repo, check=True)
shutil.copy2(app / 'LICENSE', out / 'LICENSE.txt')
for arch in ('armv6', 'armv7'):  # published for reference: every package carries its own
    shutil.copy2(GUEST / (arch + '.itpack'), out / (arch + '.itpack'))

apk = out / ('LightTouch-' + VERSION + '-android-arm64.apk')
with zipfile.ZipFile(apk) as z:
    for arch in ('armv6', 'armv7'):
        inside = hashlib.sha256(z.read('assets/guest-tools/' + arch + '.itpack')).hexdigest()
        assert inside == sha(GUEST / (arch + '.itpack')), arch + '.itpack in the APK differs from ' + str(GUEST)
tools = itpack_manifest(out / 'armv6.itpack')

files = ['LightTouch-' + VERSION + '-android-arm64.apk', 'LightTouch-' + VERSION + '-Linux-x86_64.AppImage',
         'LightTouch-' + VERSION + '-Windows-x86_64.zip', 'LightTouch-' + VERSION + '-macOS-arm64.zip',
         'armv6.itpack', 'armv7.itpack', 'LightTouch-' + VERSION + '-source.tar.gz', 'qemu-ios-' + VERSION + '-source.tar.gz']
records = [dict(file=f, bytes=(out / f).stat().st_size, sha256=sha(out / f)) for f in files]
manifest = dict(version=VERSION, status='alpha' if os.environ.get('LT_PRERELEASE') == 'false' else 'provisional alpha (pre-release)',
                application_revision=app_rev, engine_revision=engine_rev,
                guest_tools=dict(files=['armv6.itpack', 'armv7.itpack'], serial=tools['serial'], version=tools['version'],
                                 android_apk_bundles=['armv6.itpack', 'armv7.itpack']),
                packages=records)
(out / 'RELEASE-MANIFEST.json').write_text(json.dumps(manifest, indent=2) + '\n')
lines = [r['sha256'] + '  ' + r['file'] for r in records]
lines += [sha(out / f) + '  ' + f for f in ('LICENSE.txt', 'RELEASE-MANIFEST.json')]
(out / 'SHA256SUMS').write_text('\n'.join(lines) + '\n')

notes = out / 'NOTES.md'
if notes.is_file() and 'CHECKSUMS' in notes.read_text():
    block = '\n'.join(r['sha256'] + '  ' + r['file'] + ' (' + str(r['bytes']) + ' bytes)' for r in records)
    notes.write_text(notes.read_text().replace('CHECKSUMS', block))
print(json.dumps(manifest, indent=2))
