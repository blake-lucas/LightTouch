#!/usr/bin/env python3
"""Cross-build the private guest app-services executables for Android arm64-v8a.

Outputs (OUTPUT/arm64-v8a), both static, stripped PIE executables named so the
APK carries them as native libraries and the app can exec them from
nativeLibraryDir (which requires extracted native libs):

  libusbmuxd_qemu.so   usbmuxd-qemu daemon (samhenrigold/usbmuxd QEMU backend)
  liblt_appservice.so  Android/native-services/lt_appservice.c

Every source is hash-pinned by build-support/dependencies.json (the daemon by
git revision). OpenSSL, libusb, libplist, libimobiledevice-glue, libusbmuxd,
libtatsu and libimobiledevice are built static. libtatsu is configured against
Android/packaging/curl-stub (no network code at all): only libimobiledevice's
tools use TSS and nothing in either executable references libtatsu, which this
script checks. Allowed system libraries: libc, libm, libdl, liblog.

  build-mobile-android.py OUTPUT [--ndk DIR] [--source-cache DIR ...] [--jobs N]
  build-mobile-android.py OUTPUT --matches [--ndk DIR]

--matches exits 0 if OUTPUT's libraries and daemon were built from the current
recipe (the helper is relinked whenever its source changes), else 1: a stale
OUTPUT must be rebuilt from scratch, apart from OUTPUT/sources.

Archives are taken from OUTPUT/sources, then from --source-cache directories
(searched a few levels deep; each candidate's sha256 must match), and only
then downloaded from the pinned URL.
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
HERE = Path(__file__).resolve().parent
ORDER = ('openssl', 'libusb', 'libplist', 'libimobiledevice-glue', 'libusbmuxd', 'libtatsu', 'libimobiledevice')
DAEMON = '5cb90897479b9021a69db9698fea071ef7b2eceb'
DAEMON_URL = 'https://github.com/samhenrigold/usbmuxd.git'
API = 29
TRIPLE = 'aarch64-linux-android'
ABI = 'arm64-v8a'
ALLOWED_NEEDED = {'libc.so', 'libm.so', 'libdl.so', 'liblog.so'}
LIB_PATCH = APP / 'build-support/patches/libimobiledevice-sslv3-ios1.patch'
# The device names each reply's length and libimobiledevice allocated it: refuse more than 32 MiB (upstream
# LightTouchMac 6978b7a3). libplist copies a shared bplist object per reference: cap nodes and bytes (87974f89).
LIB_LIMITS_PATCH = APP / 'build-support/patches/libimobiledevice-reply-limits.patch'
PLIST_PATCH = APP / 'build-support/patches/libplist-bin-limits.patch'
DAEMON_PATCHES = (  # applied in this order
    ('linux-standard-headers', APP / 'Linux/packaging/patches/usbmuxd-linux-standard-headers.patch'),
    ('private-qemu-config', APP / 'Linux/packaging/patches/usbmuxd-private-qemu-config.patch'),
    ('android-parent-death', HERE / 'patches/usbmuxd-android-parent-death.patch'),
    # 32760-byte transactions: twice the data per guest turnaround (docs/STORAGE.md, "USB uploads").
    ('large-transactions', APP / 'build-support/patches/usbmuxd-qemu-large-transactions.patch'),
)
HELPER = APP / 'Android/native-services/lt_appservice.c'
CURL_STUB = (HERE / 'curl-stub/curl/curl.h', HERE / 'curl-stub/curl_stub.c')
DEFAULT_CACHE = Path.home() / 'Documents/LightTouch-Development/port-workspace/builds'


def sha(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            value.update(block)
    return value.hexdigest()


def find_cached(name, digest, roots, depth=6):
    for root in roots:
        root = Path(root)
        if not root.is_dir():
            continue
        base = len(root.parts)
        for directory, subdirs, files in os.walk(root):
            if len(Path(directory).parts) - base >= depth:
                subdirs[:] = []
            if name in files:
                candidate = Path(directory) / name
                if candidate.is_file() and not candidate.is_symlink() and sha(candidate) == digest:
                    return candidate
    return None


def fetch(package, sources, caches):
    archive = sources / package['archive']
    if archive.exists() and sha(archive) == package['sha256']:
        return archive
    archive.unlink(missing_ok=True)
    cached = find_cached(package['archive'], package['sha256'], caches)
    if cached:
        print('reuse', cached, flush=True)
        shutil.copyfile(cached, archive)
    else:
        print('download', package['url'], flush=True)
        partial = archive.with_suffix(archive.suffix + '.part')
        urllib.request.urlretrieve(package['url'], partial)
        partial.rename(archive)
    if sha(archive) != package['sha256']:
        archive.unlink()
        raise RuntimeError('Mobile source checksum mismatch: ' + package['archive'])
    return archive


def extract(archive, directory, destination):
    tree = destination / directory
    if tree.exists():
        return tree
    with tarfile.open(archive) as stream:
        for member in stream.getmembers():
            parts = Path(member.name).parts
            if not parts or parts[0] != directory or Path(member.name).is_absolute() or '..' in parts:
                raise RuntimeError('Unexpected source archive member: ' + member.name)
        stream.extractall(destination, filter='data')
    return tree


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('output', type=Path)
    parser.add_argument('--ndk', type=Path, default=Path.home() / 'Android/Sdk/ndk/28.2.13676358')
    parser.add_argument('--source-cache', type=Path, action='append')
    parser.add_argument('--jobs', type=int, default=max(1, min(8, os.cpu_count() or 1)))
    parser.add_argument('--matches', action='store_true', help='Only check whether OUTPUT is up to date')
    args = parser.parse_args()
    output = args.output.resolve()
    ndk = args.ndk.resolve()
    caches = args.source_cache if args.source_cache is not None else [DEFAULT_CACHE]
    toolchain = ndk / 'toolchains/llvm/prebuilt/linux-x86_64'
    tools = toolchain / 'bin'
    cc = tools / '{}{}-clang'.format(TRIPLE, API)
    if not cc.exists():
        raise RuntimeError('NDK clang not found: ' + str(cc))
    ndk_revision = next(line.split('=', 1)[1].strip() for line in (ndk / 'source.properties').read_text().splitlines()
                        if line.startswith('Pkg.Revision'))

    sources, build, prefix, out = output / 'sources', output / 'build', output / 'prefix', output / ABI
    pinned = {p['name']: p for p in json.loads((APP / 'build-support/dependencies.json').read_text())['packages']}
    packages = [pinned[name] for name in ORDER]

    recipe = dict(schema=1, abi=ABI, android_api=API, ndk_revision=ndk_revision, packages=packages,
                  daemon_revision=DAEMON, recipe_sha256=sha(__file__),
                  patch_sha256=sha(LIB_PATCH), limits_patch_sha256=sha(LIB_LIMITS_PATCH),
                  plist_patch_sha256=sha(PLIST_PATCH),
                  daemon_patches={name: sha(path) for name, path in DAEMON_PATCHES},
                  curl_stub_sha256={p.name: sha(p) for p in CURL_STUB},
                  helper_source_sha256=sha(HELPER),
                  compiler=subprocess.check_output([str(cc), '--version'], text=True).splitlines()[0])
    stamp = output / 'build-recipe.json'
    # The helper is the last link step and feeds no library, so editing it reuses the output.
    library_inputs = lambda r: {k: v for k, v in r.items() if k != 'helper_source_sha256'}
    current = stamp.exists() and library_inputs(json.loads(stamp.read_text())) == library_inputs(recipe)
    if args.matches:
        raise SystemExit(0 if current else 1)
    if stamp.exists() and not current:
        raise RuntimeError('Use a fresh output for changed mobile build inputs')
    for directory in (sources, build, prefix / 'lib/pkgconfig', prefix / 'include', out):
        directory.mkdir(parents=True, exist_ok=True)
    stamp.write_text(json.dumps(recipe, indent=2) + '\n')

    trees = {}
    for package in packages:
        trees[package['name']] = extract(fetch(package, sources, caches), package['directory'], build)

    flags = '-O2 -fPIC -ffunction-sections -fdata-sections'
    ldflags = '-Wl,--gc-sections -Wl,-z,max-page-size=16384 -Wl,--build-id=none'
    env = {key: value for key, value in os.environ.items()
           if key not in ('CPATH', 'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'LIBRARY_PATH', 'PKG_CONFIG_SYSROOT_DIR',
                          'MAKEFLAGS', 'MFLAGS', 'CONFIG_SITE', 'CPPFLAGS', 'LIBS', 'LD_LIBRARY_PATH')}
    env.update(PATH=os.pathsep.join([str(tools), '/usr/bin', '/bin']), LC_ALL='C',
               ANDROID_NDK_ROOT=str(ndk), ANDROID_NDK_HOME=str(ndk),
               CC=str(cc), CXX=str(tools / '{}{}-clang++'.format(TRIPLE, API)),
               AR=str(tools / 'llvm-ar'), RANLIB=str(tools / 'llvm-ranlib'), NM=str(tools / 'llvm-nm'),
               STRIP=str(tools / 'llvm-strip'), OBJDUMP=str(tools / 'llvm-objdump'),
               CFLAGS=flags, CXXFLAGS=flags, LDFLAGS=ldflags + ' -L' + str(prefix / 'lib'),
               PKG_CONFIG='pkg-config --static', PKG_CONFIG_PATH='',
               PKG_CONFIG_LIBDIR=str(prefix / 'lib/pkgconfig'), NOCONFIGURE='1')

    def run(command, cwd, **extra):
        command = list(map(str, command))
        print('+', ' '.join(command), flush=True)
        subprocess.run(command, cwd=cwd, env=dict(env, **extra), check=True)

    def autotools(name, options):
        tree = trees[name]
        if (tree / '.lighttouch-installed').exists():
            return
        if not (tree / 'Makefile').exists():
            run([tree / 'configure', '--host=' + TRIPLE, '--prefix=' + str(prefix), '--libdir=' + str(prefix / 'lib'),
                 '--disable-shared', '--enable-static', '--with-pic'] + options, tree)
        run(['make', '-j{}'.format(args.jobs)], tree)
        run(['make', 'install'], tree)
        (tree / '.lighttouch-installed').touch()

    # OpenSSL: SSLv3 for iPhone OS 1.x lockdownd (libimobiledevice-sslv3-ios1.patch picks it below 2.0
    # only). No config autoload and nonexistent engine/module dirs: nothing is read from the device.
    tree = trees['openssl']
    if not (tree / '.lighttouch-installed').exists():
        if not (tree / 'Makefile').exists():
            run(['perl', './Configure', 'android-arm64', '-D__ANDROID_API__={}'.format(API), 'no-shared', 'no-tests',
                 'no-docs', 'no-apps', 'no-engine', 'no-module', 'no-autoload-config', 'enable-ssl3', 'enable-ssl3-method',
                 '--prefix=' + str(prefix), '--libdir=lib', '--openssldir=/var/empty/ssl'] + flags.split(), tree,
                CFLAGS='', CXXFLAGS='', LDFLAGS='')
        run(['make', '-j{}'.format(args.jobs), 'build_libs', 'MODULESDIR=/var/empty/lib/ossl-modules',
             'ENGINESDIR=/var/empty/lib/engines-3'], tree, CFLAGS='', CXXFLAGS='', LDFLAGS='')
        run(['make', 'install_dev'], tree, CFLAGS='', CXXFLAGS='', LDFLAGS='')
        (tree / '.lighttouch-installed').touch()

    autotools('libusb', ['--disable-udev'])

    # libtatsu's configure-time libcurl: a fail-closed stub (see curl-stub/curl/curl.h).
    stub_lib = prefix / 'lib/libcurl.a'
    if not stub_lib.exists():
        (prefix / 'include/curl').mkdir(parents=True, exist_ok=True)
        shutil.copyfile(CURL_STUB[0], prefix / 'include/curl/curl.h')
        stub_obj = build / 'curl_stub.o'
        run([cc] + flags.split() + ['-I', prefix / 'include', '-c', CURL_STUB[1], '-o', stub_obj], build)
        run([env['AR'], 'rcs', stub_lib, stub_obj], build)
        (prefix / 'lib/pkgconfig/libcurl.pc').write_text(
            'prefix={}\nlibdir=${{prefix}}/lib\nincludedir=${{prefix}}/include\n\n'
            'Name: libcurl\nDescription: LightTouch fail-closed curl stub (no network)\n'
            'Version: 7.0.0\nLibs: -L${{libdir}} -lcurl\nCflags: -I${{includedir}}\n'.format(prefix))

    # Bionic has pthreads in libc and no libpthread, but libimobiledevice's configure demands
    # -lpthread (AC_CHECK_LIB pthread_once). An empty archive satisfies it and contributes nothing.
    shim = prefix / 'lib/libpthread.a'
    if not shim.exists():
        run([env['AR'], 'rcs', shim], build)

    if not (trees['libplist'] / '.lighttouch-limits-patch').exists():
        with PLIST_PATCH.open('rb') as stream:
            subprocess.run(['patch', '-p1'], cwd=trees['libplist'], stdin=stream, check=True)
        (trees['libplist'] / '.lighttouch-limits-patch').touch()
    autotools('libplist', ['--without-cython', '--without-tests'])
    autotools('libimobiledevice-glue', [])
    autotools('libusbmuxd', [])
    autotools('libtatsu', [])
    tree = trees['libimobiledevice']
    if not (tree / '.lighttouch-ssl-patch').exists():
        with LIB_PATCH.open('rb') as stream:
            subprocess.run(['patch', '-p1'], cwd=tree, stdin=stream, check=True)
        (tree / '.lighttouch-ssl-patch').touch()
    if not (tree / '.lighttouch-limits-patch').exists():
        with LIB_LIMITS_PATCH.open('rb') as stream:
            subprocess.run(['patch', '-p1'], cwd=tree, stdin=stream, check=True)
        (tree / '.lighttouch-limits-patch').touch()
    autotools('libimobiledevice', ['--without-cython', '--without-readline', '--disable-wireless-pairing',
                                   '--with-openssl'])

    # The daemon.
    daemon = build / 'usbmuxd-source'
    if not daemon.exists():
        # A cached clone (e.g. the Linux desktop build's) saves the network; the revision check is the pin.
        origin = next((str(Path(c) / 'linux-desktop/dependencies/usbmuxd-source') for c in caches
                       if (Path(c) / 'linux-desktop/dependencies/usbmuxd-source/.git').exists()), None)
        run(['git', 'clone', '--no-hardlinks', '--quiet', origin or DAEMON_URL, daemon], build)
        if origin:
            try:
                run(['git', 'cat-file', '-e', DAEMON + '^{commit}'], daemon)
            except subprocess.CalledProcessError:
                run(['git', 'fetch', '--quiet', DAEMON_URL, DAEMON], daemon)
        run(['git', '-c', 'advice.detachedHead=false', 'checkout', '--quiet', '--detach', DAEMON], daemon)
    actual = subprocess.check_output(['git', '-C', str(daemon), 'rev-parse', 'HEAD'], text=True).strip()
    if actual != DAEMON:
        raise RuntimeError('Unexpected QEMU usbmuxd source revision')
    for name, patch in DAEMON_PATCHES:
        marker = daemon / ('.lighttouch-' + name)
        if not marker.exists():
            with patch.open('rb') as stream:
                subprocess.run(['patch', '-p1'], cwd=daemon, stdin=stream, check=True)
            marker.touch()
    if not (daemon / 'configure').exists():
        run(['./autogen.sh'], daemon)
    if not (daemon / 'Makefile').exists():
        run(['./configure', '--host=' + TRIPLE, '--prefix=' + str(prefix), '--without-systemd',
             '--with-systemdsystemunitdir=no', '--with-udevrulesdir=' + str(prefix / 'unused-udev')], daemon)
    run(['make', '-j{}'.format(args.jobs)], daemon)
    executable = daemon / 'src/usbmuxd'
    if executable.read_bytes()[:4] != b'\x7fELF':
        raise RuntimeError('Refuse to package a libtool build-tree wrapper')

    # The helper.
    pc = subprocess.check_output(['pkg-config', '--static', '--cflags', '--libs', 'libimobiledevice-1.0', 'libplist-2.0'],
                                 env=env, text=True).split()
    helper = build / 'lt_appservice'
    run([cc] + flags.split() + ['-Wall', '-Wextra', '-Wno-unused-parameter', '-Werror=implicit-function-declaration',
                                '-fPIE', '-pie', HELPER, '-o', helper] + ldflags.split() + pc + ['-lm'], build)

    results = {}
    readelf = tools / 'llvm-readelf'
    nm = tools / 'llvm-nm'
    for source, name in ((executable, 'libusbmuxd_qemu.so'), (helper, 'liblt_appservice.so')):
        unstripped = build / (name + '.unstripped')
        shutil.copyfile(source, unstripped)
        # Nothing from libtatsu or the curl stub may reach a shipped executable.
        symbols = subprocess.check_output([str(nm), '--defined-only', str(unstripped)], text=True)
        if any(word.startswith(('tss_', 'curl_')) for line in symbols.splitlines() for word in line.split()[-1:]):
            raise RuntimeError(name + ' unexpectedly links libtatsu/curl code')
        target = out / name
        run([env['STRIP'], '--strip-all', '-o', target, unstripped], build)
        target.chmod(0o755)
        dynamic = subprocess.check_output([str(readelf), '-d', str(target)], text=True)
        needed = sorted(line.split('[', 1)[1].split(']', 1)[0] for line in dynamic.splitlines() if '(NEEDED)' in line)
        if not set(needed) <= ALLOWED_NEEDED:
            raise RuntimeError('{} needs non-system libraries: {}'.format(name, needed))
        headers = subprocess.check_output([str(readelf), '-hl', str(target)], text=True)
        if 'DYN' not in headers or '/system/bin/linker64' not in headers or 'AArch64' not in headers:
            raise RuntimeError(name + ' is not an AArch64 Android PIE executable')
        results[name] = dict(sha256=sha(target), size=target.stat().st_size, needed=needed)

    recipe['outputs'] = {'{}/{}'.format(ABI, name): value for name, value in results.items()}
    (output / 'dependency-manifest.json').write_text(json.dumps(recipe, indent=2) + '\n')
    for name, value in results.items():
        print('{} {} bytes NEEDED={}'.format(out / name, value['size'], ','.join(value['needed'])))


if __name__ == '__main__':
    main()
