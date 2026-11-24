#!/usr/bin/env python3
"""Build the Windows USB bridge (lighttouch-usbmuxd.exe and the DLLs it loads) from pinned sources, on Linux with MinGW.

  build.py OUTPUT [--source-cache DIR ...] [--daemon-clone DIR] [--jobs N]

OUTPUT/sources  libplist, libimobiledevice-glue and libusbmuxd archives (build-support/dependencies.json, checked by
                SHA-256; found in a --source-cache folder or downloaded) and the daemon at SOURCES.json's revision
                (cloned from --daemon-clone if given, else GitHub), unmodified.
OUTPUT/deps     the dependency packet build-native.py takes: libplist (with build-support/patches/libplist-bin-limits.patch)
                and libimobiledevice-glue cross-built as DLLs, their licences, RESULT.json and OUTPUTS.json. Rebuilt only
                when its recipe changes.
OUTPUT/bridge   build-native.py's output, made afresh every run: bin/ (the executable and its DLL closure), licenses/.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import urllib.request

HERE = Path(__file__).resolve().parent
APP = HERE.parents[1]
HOST = 'x86_64-w64-mingw32'
CC = '/usr/bin/x86_64-w64-mingw32-gcc-posix'
LIMITS_PATCH = APP / 'build-support/patches/libplist-bin-limits.patch'
DAEMON_URL = 'https://github.com/samhenrigold/usbmuxd.git'


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fetch(package, sources, caches):
    """The package's archive in `sources`, copied from a cache or downloaded, checked against its pin."""
    target = sources / package['archive']
    if not target.exists() or sha(target) != package['sha256']:
        found = next((p for c in caches for p in Path(c).rglob(package['archive'])
                      if p.is_file() and sha(p) == package['sha256']), None)
        if found:
            shutil.copyfile(found, target)
        else:
            urllib.request.urlretrieve(package['url'], target)
    if sha(target) != package['sha256']:
        raise SystemExit(package['archive'] + ' does not match its SHA-256 pin')
    return target


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('output', type=Path)
    parser.add_argument('--source-cache', type=Path, action='append', default=[])
    parser.add_argument('--daemon-clone', type=Path)
    parser.add_argument('--jobs', type=int, default=os.cpu_count() or 4)
    args = parser.parse_args()
    out = args.output.resolve()
    sources, deps = out / 'sources', out / 'deps'
    sources.mkdir(parents=True, exist_ok=True)
    pins = json.loads((HERE / 'SOURCES.json').read_text())
    pinned = {p['name']: p for p in json.loads((APP / 'build-support/dependencies.json').read_text())['packages']}
    packages = [pinned[n] for n in ('libplist', 'libimobiledevice-glue')]
    archives = {p['name']: fetch(p, sources, args.source_cache) for p in packages + [pinned['libusbmuxd']]}

    daemon = sources / 'usbmuxd'
    revision = pins['daemon']['revision']
    if not daemon.exists():
        subprocess.run(['git', 'clone', '--quiet', str(args.daemon_clone or DAEMON_URL), str(daemon)], check=True)
    if subprocess.run(['git', 'cat-file', '-e', revision + '^{commit}'], cwd=daemon).returncode:
        subprocess.run(['git', 'fetch', '--quiet', DAEMON_URL, revision], cwd=daemon, check=True)
    subprocess.run(['git', '-c', 'advice.detachedHead=false', 'checkout', '--quiet', '--force', '--detach', revision],
                   cwd=daemon, check=True)
    subprocess.run(['git', 'clean', '-qfdx'], cwd=daemon, check=True)

    compiler = subprocess.check_output([CC, '--version'], text=True).splitlines()[0]
    recipe = dict(packages=packages, script=sha(__file__), limits_patch=sha(LIMITS_PATCH), compiler=compiler)
    stamp = deps / 'build-recipe.json'
    if not stamp.exists() or json.loads(stamp.read_text()) != recipe:
        shutil.rmtree(deps, ignore_errors=True)
        build_deps(deps, packages, archives, args.jobs)
        stamp.write_text(json.dumps(recipe, indent=2) + '\n')

    bridge = out / 'bridge'
    shutil.rmtree(bridge, ignore_errors=True)
    subprocess.run(['python3', str(HERE / 'build-native.py'), '--deps-packet', str(deps), '--daemon-source', str(daemon),
                    '--libusbmuxd-archive', str(archives['libusbmuxd']), '--output', str(bridge)], check=True)


def build_deps(deps, packages, archives, jobs):
    prefix, work = deps / 'prefix', deps / 'work'
    work.mkdir(parents=True)
    env = {k: v for k, v in os.environ.items() if k not in ('CFLAGS', 'CPPFLAGS', 'LDFLAGS', 'LIBS', 'PKG_CONFIG_PATH')}
    env.update(CC=CC, PKG_CONFIG_LIBDIR=str(prefix / 'lib/pkgconfig'), PKG_CONFIG_PATH='', LC_ALL='C')
    for package in packages:
        with tarfile.open(archives[package['name']]) as archive:
            archive.extractall(work, filter='data')
        tree = work / package['directory']
        if package['name'] == 'libplist':
            with LIMITS_PATCH.open('rb') as stream:
                subprocess.run(['patch', '-p1', '--batch', '--fuzz=0'], cwd=tree, stdin=stream, check=True)
        subprocess.run(['./configure', '--quiet', '--host=' + HOST, '--build=x86_64-pc-linux-gnu', '--prefix=' + str(prefix),
                        '--enable-shared', '--disable-static', '--without-cython', '--without-tests'],
                       cwd=tree, env=env, check=True)
        # Upstream links -lIphlpapi, which a case-sensitive host's MinGW doesn't have: libtool then quietly drops
        # the DLL and installs a static library only.
        makefile = tree / 'src/Makefile'
        makefile.write_text(makefile.read_text().replace('-lIphlpapi', '-liphlpapi'))
        subprocess.run(['make', '-s', '-j' + str(jobs)], cwd=tree, env=env, check=True)
        subprocess.run(['make', '-s', 'install'], cwd=tree, env=env, check=True)
        notices = deps / 'licenses' / package['name']
        notices.mkdir(parents=True)
        for licence in tree.glob('COPYING*'):
            shutil.copyfile(licence, notices / licence.name)
    for dll in (prefix / 'bin').glob('*.dll'):
        subprocess.run(['/usr/bin/x86_64-w64-mingw32-strip', '--strip-unneeded', str(dll)], check=True)
    shutil.rmtree(work)
    files = {str(p.relative_to(prefix)): {'sha256': sha(p)} for p in prefix.rglob('*') if p.is_file() and not p.is_symlink()}
    (deps / 'OUTPUTS.json').write_text(json.dumps({'files': files}, indent=2) + '\n')
    (deps / 'RESULT.json').write_text(json.dumps({'status': 'PASS', 'packages': [p['name'] for p in packages]}) + '\n')


if __name__ == '__main__':
    main()
