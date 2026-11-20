#!/usr/bin/env python3
"""Fetch the pinned release inputs CI uses (ci/release-inputs.json) into ROOT, each checked by its SHA-256.

  fetch-inputs.py ROOT NAME...     NAME: a "files" entry, or msys2 (the Windows MinGW sysroot)

A file entry is saved at ROOT/<save>, or (extract) unpacked into ROOT: the ci-inputs tarballs hold
builds/... (the LT_INPUTS layout scripts/build.sh and build-release.sh read) and notices/ (LT_DESKTOP_NOTICES).
msys2 downloads the packages into ROOT/win-deps/packages and extracts them into ROOT/win-deps/sysroot (LT_WIN_DEPS).
Downloads are kept in ROOT/downloads, so a rerun fetches nothing it has.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

MANIFEST = Path(__file__).resolve().parents[2] / 'ci/release-inputs.json'


def sha(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def fetch(url, expected, dest):
    if dest.exists() and sha(dest) == expected:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    for attempt in range(4):
        try:
            part = dest.with_suffix(dest.suffix + '.part')
            request = urllib.request.Request(url, headers={'User-Agent': 'lighttouch-ci'})
            with urllib.request.urlopen(request, timeout=120) as response, open(part, 'wb') as out:
                while block := response.read(1 << 20):
                    out.write(block)
            part.rename(dest)
            break
        except OSError as error:
            print(f'{url}: {error}; retrying', file=sys.stderr)
            time.sleep(5 * (attempt + 1))
    else:
        raise SystemExit(f'could not download {url}')
    actual = sha(dest)
    if actual != expected:
        dest.unlink()
        raise SystemExit(f'{url}: SHA-256 {actual}, expected {expected}')
    return dest


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    root = Path(sys.argv[1]).resolve()
    manifest = json.loads(MANIFEST.read_text())
    downloads = root / 'downloads'
    for name in sys.argv[2:]:
        if name == 'msys2':
            m = manifest['msys2']
            sysroot = root / 'win-deps/sysroot'
            sysroot.mkdir(parents=True, exist_ok=True)
            for p in m['packages']:
                path = fetch(m['mirror'] + p['file'], p['sha256'], root / 'win-deps/packages' / p['file'])
                subprocess.run(['tar', '--zstd', '-xf', str(path), '-C', str(sysroot), '--exclude=.BUILDINFO',
                                '--exclude=.MTREE', '--exclude=.PKGINFO', '--exclude=.INSTALL'], check=True)
            print(f'msys2: {len(m["packages"])} packages in {sysroot}')
            continue
        entry = manifest['files'].get(name) or sys.exit(f'unknown input: {name}')
        if entry.get('extract'):
            path = fetch(entry['url'], entry['sha256'], downloads / entry['url'].rsplit('/', 1)[1])
            subprocess.run(['tar', '-xzf', str(path), '-C', str(root)], check=True)
            print(f'{name}: unpacked into {root}')
        else:
            path = fetch(entry['url'], entry['sha256'], root / entry['save'])
            print(f'{name}: {path}')


if __name__ == '__main__':
    main()
