#!/usr/bin/env python3
"""Build the hash-pinned shared mobile stack for the private QEMU USB transport.

  build-mobile-deps.py OUTPUT [--prepare-only] [--daemon-cache CLONE]
  build-mobile-deps.py --matches OUTPUT     exit 0 if OUTPUT was built from the current recipe, else 1

--prepare-only downloads, checks and unpacks every source (the daemon included: a clone of CLONE, if given, saves the
network) and builds nothing, so the build itself needs no network.
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

APP = Path(__file__).resolve().parents[2]
NAMES = ('libplist', 'libimobiledevice-glue', 'libusbmuxd', 'libtatsu', 'libimobiledevice')
# Size caps on what a device sends (upstream LightTouchMac 6978b7a3, 87974f89): libimobiledevice refuses a reply
# declaring more than 32 MiB, and libplist's binary parser stops at 2^20 nodes or 32 MiB.
LIMIT_PATCHES = {'libplist': 'libplist-bin-limits.patch', 'libimobiledevice': 'libimobiledevice-reply-limits.patch'}
DAEMON = '5cb90897479b9021a69db9698fea071ef7b2eceb'
DAEMON_URL = 'https://github.com/samhenrigold/usbmuxd.git'


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def inputs(packages):
    """What an output is built from, apart from the compiler (which only the build image can name)."""
    return dict(schema=1, packages=packages, daemon_revision=DAEMON,
                daemon_patch_sha256=sha(APP / 'Linux/packaging/patches/usbmuxd-linux-standard-headers.patch'),
                daemon_private_config_patch_sha256=sha(APP / 'Linux/packaging/patches/usbmuxd-private-qemu-config.patch'),
                daemon_large_transactions_patch_sha256=sha(APP / 'build-support/patches/usbmuxd-qemu-large-transactions.patch'),
                recipe_sha256=sha(__file__),
                patch_sha256=sha(APP / 'build-support/patches/libimobiledevice-sslv3-ios1.patch'),
                limits_patch_sha256={n: sha(APP / 'build-support/patches' / n) for n in sorted(set(LIMIT_PATCHES.values()))})


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('output', type=Path)
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--matches', action='store_true', help='Only check whether OUTPUT is up to date')
    parser.add_argument('--daemon-cache', type=Path, help='A clone of the daemon repository to clone from')
    parser.add_argument('--daemon-only-from', type=Path,
                        help='Fresh daemon build reusing a previously manifested public mobile stack')
    args = parser.parse_args()
    output = args.output.resolve()
    packages = json.loads((APP / 'build-support/dependencies.json').read_text())['packages']
    packages = [next(p for p in packages if p['name'] == name) for name in NAMES]
    if args.matches:
        stamp = output / 'build-recipe.json'
        built = json.loads(stamp.read_text()) if stamp.exists() else {}
        built.pop('compiler', None)
        raise SystemExit(0 if built == inputs(packages) else 1)
    reuse = args.daemon_only_from.resolve() if args.daemon_only_from else None
    if reuse and (args.prepare_only or output.exists()):
        raise RuntimeError('Daemon-only reuse requires a fresh output and cannot prepare-only')
    output.mkdir(parents=True, exist_ok=True)
    sources = output / 'sources'
    sources.mkdir(exist_ok=True)
    reuse_manifest = None
    if reuse:
        reuse_manifest = reuse / 'dependency-manifest.json'
        previous = json.loads(reuse_manifest.read_text())
        if previous['packages'] != packages or previous['daemon_revision'] != DAEMON:
            raise RuntimeError('Reused mobile stack has different pinned sources')
        if previous['patch_sha256'] != sha(APP / 'build-support/patches/libimobiledevice-sslv3-ios1.patch'):
            raise RuntimeError('Reused mobile stack has a different library patch')
        for name, checksum in previous['outputs'].items():
            if Path(name).is_absolute() or '..' in Path(name).parts or sha(reuse / 'prefix' / name) != checksum:
                raise RuntimeError('Changed reused mobile output: ' + name)
        shutil.copytree(reuse / 'sources', sources, dirs_exist_ok=True, symlinks=True)
        shutil.copytree(reuse / 'prefix', output / 'prefix', symlinks=True)
    for package in packages:
        archive = sources / package['archive']
        if not archive.exists():
            urllib.request.urlretrieve(package['url'], archive)
        if sha(archive) != package['sha256']:
            raise RuntimeError('Mobile source checksum mismatch: ' + str(archive))
        tree = sources / package['directory']
        if not tree.exists():
            with tarfile.open(archive) as stream:
                stream.extractall(sources, filter='data')
    daemon = output / 'usbmuxd-source'
    if not daemon.exists():
        origin = str(reuse / 'usbmuxd-source') if reuse else str(args.daemon_cache) if args.daemon_cache else DAEMON_URL
        subprocess.run(['git', 'clone', '--no-hardlinks', '--quiet', origin, str(daemon)], check=True)
        if subprocess.run(['git', 'cat-file', '-e', DAEMON + '^{commit}'], cwd=daemon).returncode:
            subprocess.run(['git', 'fetch', '--quiet', DAEMON_URL, DAEMON], cwd=daemon, check=True)
        subprocess.run(['git', '-c', 'advice.detachedHead=false', 'checkout', '--quiet', '--detach', DAEMON],
                       cwd=daemon, check=True)
    if args.prepare_only:
        print('Hash-verified mobile source preparation complete')
        return
    prefix = output / 'prefix'
    env = dict(os.environ, PKG_CONFIG_PATH=str(prefix / 'lib/pkgconfig'),
               CFLAGS='-O2 -fPIC', LDFLAGS='-Wl,-rpath,' + str(prefix / 'lib'),
               LD_LIBRARY_PATH=str(prefix / 'lib'), NOCONFIGURE='1')
    recipe = dict(inputs(packages), compiler=subprocess.check_output(['cc', '--version'], text=True).splitlines()[0])
    if reuse_manifest:
        recipe['reused_dependency_manifest_sha256'] = sha(reuse_manifest)
        recipe['reused_dependency_manifest'] = previous
    stamp = output / 'build-recipe.json'
    if stamp.exists() and json.loads(stamp.read_text()) != recipe:
        raise RuntimeError('Use a fresh output for changed mobile build inputs')
    stamp.write_text(json.dumps(recipe, indent=2) + '\n')

    def run(command, cwd):
        print(' '.join(map(str, command)), flush=True)
        subprocess.run(list(map(str, command)), cwd=cwd, env=env, check=True)

    for package in ([] if reuse else packages):
        tree = sources / package['directory']
        if package['name'] == 'libimobiledevice' and not (tree / '.lighttouch-ssl-patch').exists():
            with (APP / 'build-support/patches/libimobiledevice-sslv3-ios1.patch').open('rb') as stream:
                subprocess.run(['patch', '-p1'], cwd=tree, stdin=stream, check=True)
            (tree / '.lighttouch-ssl-patch').touch()
        if package['name'] in LIMIT_PATCHES and not (tree / '.lighttouch-limits-patch').exists():
            with (APP / 'build-support/patches' / LIMIT_PATCHES[package['name']]).open('rb') as stream:
                subprocess.run(['patch', '-p1'], cwd=tree, stdin=stream, check=True)
            (tree / '.lighttouch-limits-patch').touch()
        if not (tree / 'Makefile').exists():
            options = ['--prefix=' + str(prefix), '--libdir=' + str(prefix / 'lib'), '--enable-shared', '--disable-static']
            if package['name'] in ('libplist', 'libimobiledevice'):
                options += ['--without-cython']
            run([tree / 'configure'] + options, tree)
        run(['make', '-j2'], tree)
        run(['make', 'install'], tree)
    actual = subprocess.check_output(['git', '-C', str(daemon), 'rev-parse', 'HEAD'], text=True).strip()
    if actual != DAEMON:
        raise RuntimeError('Unexpected QEMU usbmuxd source revision')
    if not (daemon / '.lighttouch-linux-standard-headers').exists():
        with (APP / 'Linux/packaging/patches/usbmuxd-linux-standard-headers.patch').open('rb') as stream:
            subprocess.run(['patch', '-p1'], cwd=daemon, stdin=stream, check=True)
        (daemon / '.lighttouch-linux-standard-headers').touch()
    if not (daemon / '.lighttouch-private-qemu-config').exists():
        with (APP / 'Linux/packaging/patches/usbmuxd-private-qemu-config.patch').open('rb') as stream:
            subprocess.run(['patch', '-p1'], cwd=daemon, stdin=stream, check=True)
        (daemon / '.lighttouch-private-qemu-config').touch()
    if not (daemon / '.lighttouch-large-transactions').exists():
        with (APP / 'build-support/patches/usbmuxd-qemu-large-transactions.patch').open('rb') as stream:
            subprocess.run(['patch', '-p1'], cwd=daemon, stdin=stream, check=True)
        (daemon / '.lighttouch-large-transactions').touch()
    if not (daemon / 'configure').exists():
        run(['./autogen.sh'], daemon)
    if not (daemon / 'Makefile').exists():
        run(['./configure', '--prefix=' + str(prefix), '--without-systemd', '--with-systemdsystemunitdir=no'], daemon)
    run(['make', '-j2'], daemon)
    # Only the executable is copied. Never install daemon units/rules globally.
    (prefix / 'bin').mkdir(parents=True, exist_ok=True)
    executable = daemon / 'src/.libs/usbmuxd'
    if not executable.exists():
        executable = daemon / 'src/usbmuxd'
    if executable.read_bytes()[:4] != b'\x7fELF':
        raise RuntimeError('Refuse to package a libtool build-tree wrapper')
    shutil.copyfile(executable, prefix / 'bin/usbmuxd-qemu')
    (prefix / 'bin/usbmuxd-qemu').chmod(0o755)
    recipe['outputs'] = {str(p.relative_to(prefix)): sha(p.resolve()) for p in sorted(prefix.rglob('*')) if p.is_file()}
    (output / 'dependency-manifest.json').write_text(json.dumps(recipe, indent=2) + '\n')


if __name__ == '__main__':
    main()
