#!/usr/bin/env python3
"""Build the hash-pinned shared mobile stack for the private QEMU USB transport."""
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


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--daemon-only-from', type=Path,
                        help='Fresh daemon build reusing a previously manifested public mobile stack')
    args = parser.parse_args()
    output = args.output.resolve()
    reuse = args.daemon_only_from.resolve() if args.daemon_only_from else None
    if reuse and (args.prepare_only or output.exists()):
        raise RuntimeError('Daemon-only reuse requires a fresh output and cannot prepare-only')
    output.mkdir(parents=True, exist_ok=True)
    sources = output / 'sources'
    sources.mkdir(exist_ok=True)
    packages = json.loads((APP / 'build-support/dependencies.json').read_text())['packages']
    packages = [next(p for p in packages if p['name'] == name) for name in NAMES]
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
    if args.prepare_only:
        print('Hash-verified mobile source preparation complete')
        return
    prefix = output / 'prefix'
    env = dict(os.environ, PKG_CONFIG_PATH=str(prefix / 'lib/pkgconfig'),
               CFLAGS='-O2 -fPIC', LDFLAGS='-Wl,-rpath,' + str(prefix / 'lib'),
               LD_LIBRARY_PATH=str(prefix / 'lib'), NOCONFIGURE='1')
    recipe = dict(schema=1, packages=packages, daemon_revision=DAEMON, daemon_patch_sha256=sha(APP / 'Linux/packaging/patches/usbmuxd-linux-standard-headers.patch'),
                  daemon_private_config_patch_sha256=sha(APP / 'Linux/packaging/patches/usbmuxd-private-qemu-config.patch'),
                  daemon_large_transactions_patch_sha256=sha(APP / 'build-support/patches/usbmuxd-qemu-large-transactions.patch'),
                  recipe_sha256=sha(__file__),
                  patch_sha256=sha(APP / 'build-support/patches/libimobiledevice-sslv3-ios1.patch'),
                  limits_patch_sha256={n: sha(APP / 'build-support/patches' / n) for n in sorted(set(LIMIT_PATCHES.values()))},
                  compiler=subprocess.check_output(['cc', '--version'], text=True).splitlines()[0])
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
    daemon = output / 'usbmuxd-source'
    if not daemon.exists():
        run(['git', 'clone', '--no-hardlinks', str(reuse / 'usbmuxd-source') if reuse else 'https://github.com/samhenrigold/usbmuxd.git', daemon], output)
        run(['git', 'checkout', '--detach', DAEMON], daemon)
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
