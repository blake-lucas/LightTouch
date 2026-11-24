#!/usr/bin/env python3
"""Offline MinGW daemon + owned libusbmuxd build; never executes target code.

Run only in the integrator's allocated build slot. Dependencies must already
have a passing services-mobile-deps01-style receipt. This builds the transport
milestone, not the remaining libimobiledevice/TLS/IPA stack or its qualification.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import shlex
import shutil
import subprocess
import tarfile


HERE = Path(__file__).resolve().parent
CC = '/usr/bin/x86_64-w64-mingw32-gcc-posix'
SYSTEM_DLLS = {
    'advapi32.dll', 'bcrypt.dll', 'comdlg32.dll', 'crypt32.dll', 'gdi32.dll',
    'iphlpapi.dll', 'kernel32.dll', 'msvcrt.dll', 'ntdll.dll', 'ole32.dll',
    'oleaut32.dll', 'rpcrt4.dll', 'secur32.dll', 'setupapi.dll', 'shell32.dll',
    'shlwapi.dll', 'user32.dll', 'userenv.dll', 'version.dll', 'winmm.dll',
    'ws2_32.dll',
}
TOOLCHAIN_DLLS = {'libwinpthread-1.dll', 'libgcc_s_seh-1.dll'}


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--deps-packet', type=Path, required=True)
    parser.add_argument('--daemon-source', type=Path, required=True)
    parser.add_argument('--libusbmuxd-archive', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    deps = args.deps_packet.resolve()
    prefix = deps / 'prefix'
    result = json.loads((deps / 'RESULT.json').read_text())
    if result.get('status') != 'PASS':
        raise ValueError('Dependency build must have an actual PASS receipt.')
    providers = json.loads((deps / 'OUTPUTS.json').read_text())
    for name, record in providers['files'].items():
        path = prefix / name
        if prefix not in path.resolve().parents or sha(path) != record['sha256']:
            raise ValueError('Dependency provider changed: ' + name)
    pins = json.loads((HERE / 'SOURCES.json').read_text())
    for name, digest in pins['daemon']['preimage_sha256'].items():
        if sha(args.daemon_source / name) != digest:
            raise ValueError('Daemon preimage mismatch: ' + name)
    if sha(args.libusbmuxd_archive) != pins['libusbmuxd']['archive_sha256']:
        raise ValueError('libusbmuxd archive mismatch')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    commands = []
    env = dict(os.environ)
    for name in ('CPATH', 'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'LIBRARY_PATH',
                 'PKG_CONFIG_SYSROOT_DIR', 'CONFIG_SITE', 'MAKEFLAGS', 'MFLAGS',
                 'CFLAGS', 'CXXFLAGS', 'CPPFLAGS', 'LDFLAGS', 'LIBS',
                 'LD_PRELOAD', 'LD_LIBRARY_PATH'):
        env.pop(name, None)
    env.update(PATH='/usr/bin:/bin', LC_ALL='C', CC=CC,
               AR='/usr/bin/x86_64-w64-mingw32-ar',
               RANLIB='/usr/bin/x86_64-w64-mingw32-ranlib',
               STRIP='/usr/bin/x86_64-w64-mingw32-strip',
               PKG_CONFIG='/usr/bin/pkg-config', PKG_CONFIG_PATH='',
               PKG_CONFIG_LIBDIR=str(prefix / 'lib/pkgconfig'),
               CFLAGS='-O2 -g0 -D_WIN32_WINNT=0x0601',
               CPPFLAGS=shlex.join(['-I', str(HERE)]))

    def run(argv, cwd=output):
        argv = list(map(str, argv))
        commands.append({'argv': argv, 'cwd': str(cwd)})
        (output / 'COMMANDS.json').write_text(json.dumps(commands, indent=2) + '\n')
        print(json.dumps(commands[-1]), flush=True)
        subprocess.run(argv, cwd=cwd, env=env, check=True, timeout=100)

    def patch(tree, name):
        run(['/usr/bin/patch', '--batch', '--fuzz=0', '-p1', '-i', HERE / name], tree)

    daemon = output / 'daemon'
    (daemon / 'src').mkdir(parents=True)
    for name in pins['daemon']['preimage_sha256']:
        shutil.copyfile(args.daemon_source / name, daemon / name)
    patch(daemon, 'windows-host.patch')
    patch(daemon, '../../build-support/patches/usbmuxd-qemu-large-transactions.patch')
    objects = output / 'objects'
    objects.mkdir()
    common = [CC, '-std=gnu11', '-O2', '-Wall',
              '-Werror=implicit-function-declaration', '-Werror=incompatible-pointer-types',
              '-D_WIN32_WINNT=0x0601', '-DWIN32', '-DHAVE_SYS_TYPES_H',
              '-I', HERE, '-I', prefix / 'include']
    units = pins['daemon']['translation_units'] + ['windows-main.c', 'windows-peer.c']
    daemon_objects = []
    for name in units:
        source = HERE / name if name.startswith('windows-') else daemon / 'src' / name
        target = objects / (Path(name).stem + '.o')
        run(common + ['-include', HERE / 'windows-compat.h', '-I', daemon / 'src',
                      '-c', source, '-o', target])
        daemon_objects.append(target)
    runtime = output / 'bin'
    runtime.mkdir()
    run([CC, '-static-libgcc', '-o', runtime / 'lighttouch-usbmuxd.exe',
         *daemon_objects, '-L', prefix / 'lib',
         *('-l' + name for name in pins['daemon']['link_libraries'])])

    with tarfile.open(args.libusbmuxd_archive) as archive:
        members = archive.getmembers()
        if len(members) > 2000 or sum(m.size for m in members) > 64 * 1024 * 1024:
            raise ValueError('Unexpected libusbmuxd archive size')
        names = set()
        for member in members:
            path = PurePosixPath(member.name)
            if (path.is_absolute() or '..' in path.parts or not path.parts or
                    path.parts[0] != 'libusbmuxd-2.1.1' or
                    not (member.isfile() or member.isdir()) or member.name in names):
                raise ValueError('Unsafe archive member: ' + member.name)
            names.add(member.name)
        archive.extractall(output, filter='data')
    library = output / 'libusbmuxd-2.1.1'
    for name, digest in pins['libusbmuxd']['preimage_sha256'].items():
        if sha(library / name) != digest:
            raise ValueError('libusbmuxd preimage mismatch: ' + name)
    patch(library, 'libusbmuxd-owned.patch')
    run([library / 'configure', '--host=x86_64-w64-mingw32',
         '--build=x86_64-pc-linux-gnu', '--prefix=' + str(output / 'installed'),
         '--enable-shared', '--disable-static', '--without-inotify'], library)
    connector_objects = []
    for name in ('windows-connect.c', 'windows-peer.c'):
        target = objects / ('connector-' + Path(name).stem + '.lo')
        # No daemon compatibility macros in libusbmuxd.
        # MinGW libtool rejects ordinary .o additions to a shared .la. Use its
        # generated compile mode to produce the matching .lo and .libs object.
        run([library / 'libtool', '--tag=CC', '--mode=compile', *common,
             '-Wextra', '-Werror', '-c', HERE / name, '-o', target], library)
        connector_objects.append(target)
    # Override LIBADD rather than regenerating autotools files or relying on
    # upstream's mixed-case -lIphlpapi, absent on case-sensitive Linux hosts.
    libadd = shlex.join(list(map(str, connector_objects)) + ['-lws2_32', '-liphlpapi', '-ladvapi32'])
    run(['/usr/bin/make', '-C', library / 'src', '-j1', 'libusbmuxd-2.0.la',
         'libusbmuxd_2_0_la_LIBADD=' + libadd], library)
    dlls = list((library / 'src/.libs').glob('libusbmuxd-2.0*.dll'))
    if len(dlls) != 1:
        raise ValueError('Expected exactly one native connector DLL')
    shutil.copyfile(dlls[0], runtime / dlls[0].name)

    spec = importlib.util.spec_from_file_location('pe_inventory', HERE.parent / 'tools/pe_inventory.py')
    pe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(pe)
    pending = list(runtime.iterdir())
    inspected = {}
    while pending:
        path = pending.pop()
        key = path.name.lower()
        if key in inspected:
            continue
        inspected[key] = pe.inspect_pe(path.read_bytes(), path.name)
        for name in inspected[key]['imports']:
            lowered = name.lower()
            if lowered in SYSTEM_DLLS or lowered.startswith(('api-ms-win-', 'ext-ms-win-')):
                continue
            destination = runtime / name
            if not destination.exists():
                candidates = [p for p in (prefix / 'bin').glob('*') if p.name.lower() == lowered]
                if not candidates and lowered in TOOLCHAIN_DLLS:
                    query = subprocess.check_output([CC, '-print-file-name=' + name],
                                                    env=env, text=True, timeout=5).strip()
                    if Path(query).is_absolute() and Path(query).is_file():
                        candidates = [Path(query)]
                if len(candidates) != 1:
                    raise ValueError('Missing or ambiguous DLL provider: ' + name)
                shutil.copyfile(candidates[0], destination)
            pending.append(destination)
    notices = output / 'licenses'
    shutil.copytree(deps / 'licenses', notices)
    for name in pins['daemon']['license_files']:
        destination = notices / 'usbmuxd' / name
        destination.parent.mkdir(exist_ok=True)
        shutil.copyfile(args.daemon_source / name, destination)
    for source in library.glob('COPYING*'):
        destination = notices / 'libusbmuxd' / source.name
        destination.parent.mkdir(exist_ok=True)
        shutil.copyfile(source, destination)
    for package in ('mingw-w64-common', 'gcc-mingw-w64-base'):
        shutil.copyfile(Path('/usr/share/doc') / package / 'copyright', notices / (package + '.txt'))
    inventory = {str(p.relative_to(output)): {'size': p.stat().st_size, 'sha256': sha(p)}
                 for directory in (runtime, notices) for p in directory.rglob('*') if p.is_file()}
    (output / 'OUTPUTS.json').write_text(json.dumps({
        'files': inventory, 'imports': inspected, 'windows_executed': False,
        'shared_apps_enabled': False,
        'adapter_sources': {p.name: sha(p) for p in HERE.iterdir() if p.is_file()},
    }, indent=2) + '\n')
    print('PASS cross-linked daemon and owned libusbmuxd; Windows execution remains unqualified.')


if __name__ == '__main__':
    main()
