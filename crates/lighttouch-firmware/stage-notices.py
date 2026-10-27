#!/usr/bin/env python3
"""Stage the notices of a Rust binary this workspace ships, for an app's licence screen.

    stage-notices.py OUT --target TRIPLE --source-reference URL [--package NAME] [--features LIST]

NAME is lighttouch-firmware (firmwarekit, the default: the APK's assets/licenses/firmwarekit and the desktop's
resources/licenses/firmware) or lighttouch (the desktop app: resources/licenses/frontend). OUT gets the licence and
notice files of every crate the binary links for TRIPLE (normal dependencies, from `cargo metadata`) as
<crate>-<version>-<file>, LightTouch's licence, a NOTICE.txt naming what is compiled in from C, and manifest.json in
the schema the licence screens read ({"schema": 1, "source_reference", "files": [{"path", "sha256"}]}).

A few crates publish no licence file (egui's, objc2's). For those the declared licence's standard text comes from
build-support/licenses, headed by the crate's authors and repository, plus any licence files in its subfolders
(epaint_default_fonts' font licences).
"""
import argparse, hashlib, json, re, shutil, subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
TEXTS = REPO / "build-support/licenses"
TERMS = re.compile(r"^(LICEN[SC]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE)", re.I)
NESTED = re.compile(r"(licen[sc]e|copying|^OFL|^UFL)[^/]*\.txt$", re.I)
PREFERRED = ["MIT", "Zlib", "Apache-2.0", "BSL-1.0"]  # the text used for "A OR B"
COMPILED_IN = {"openssl-src"}  # build dependencies whose output is linked (vendored OpenSSL)

NOTICES = {
    "lighttouch-firmware": """firmwarekit (crates/lighttouch-firmware in LightTouch's source) prepares an emulated device from an IPSW. It is written in Rust and links, statically:

- the Rust standard library (MIT or Apache-2.0);
- the crates whose licences follow, as <crate>-<version>-<file>; rusqlite's libsqlite3-sys compiles in SQLite (public domain, https://sqlite.org/copyright.html);
- iBoot32Patcher (LukeZGD's fork, GPL-3.0-or-later: iBoot32Patcher-LICENSE), which patches the iPad's iBoot. With it, firmwarekit as distributed is a combined work under the GNU GPL version 3 or later; LightTouch's own code in it is GPL-2.0-or-later (LightTouch-LICENSE). The source is at the reference in manifest.json;
- LightTouch's activation.c (GPL-2.0-or-later).
""",
    "lighttouch": """The LightTouch desktop app (crates/lighttouch in LightTouch's source) is written in Rust and is GPL-2.0-or-later (LightTouch-LICENSE). It links, statically:

- the Rust standard library (MIT or Apache-2.0);
- the crates whose licences follow, as <crate>-<version>-<file>; on Windows the openssl-src crate compiles in OpenSSL (Apache-2.0);
- LightTouch's shared C++ core (Portable/, GPL-2.0-or-later).

The source is at the reference in manifest.json. The engine, firmwarekit and the USB helper have their own notices.
""",
}


def linked(package, target, features):
    command = ["cargo", "metadata", "--format-version", "1", "--filter-platform", target, "--locked"]
    if features:
        command += ["--features", features]
    meta = json.loads(subprocess.check_output(command, cwd=HERE))
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == package)
    seen, todo = set(), [root]
    while todo:
        pid = todo.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]) or dep["name"].replace("_", "-") in COMPILED_IN:
                todo.append(dep["pkg"])  # normal (linked) dependencies only
    return [packages[p] for p in sorted(seen) if packages[p]["source"]]  # crates.io/git, not the workspace


def declared(p):
    """The standard text for a crate that ships no licence file, headed by who it is from."""
    expression = p.get("license") or ""
    ids = set(re.findall(r"[A-Za-z0-9.+-]+", expression)) - {"OR", "AND", "WITH"}
    choice = next((i for i in PREFERRED if i in ids), None)
    if choice is None:
        raise SystemExit(f"stage-notices: {p['name']} {p['version']} ({expression}) ships no licence file and "
                         f"build-support/licenses has none of its licences")
    head = (f"The published {p['name']} {p['version']} crate includes no licence file. Its manifest declares the "
            f"licence {expression}; this is the {choice} text.\n\n"
            f"Copyright (c) {', '.join(p.get('authors') or ['the ' + p['name'] + ' authors'])}\n"
            f"Repository: {p.get('repository') or 'https://crates.io/crates/' + p['name']}\n\n")
    return (head + (TEXTS / f"{choice}.txt").read_text()).encode()


def main():
    a = argparse.ArgumentParser()
    a.add_argument("out", type=Path)
    a.add_argument("--target", required=True)
    a.add_argument("--source-reference", required=True)
    a.add_argument("--package", default="lighttouch-firmware", choices=sorted(NOTICES))
    a.add_argument("--features", default="")
    args = a.parse_args()
    if args.out.exists():
        shutil.rmtree(args.out)
    args.out.mkdir(parents=True)
    staged = {"NOTICE.txt": NOTICES[args.package].encode(), "LightTouch-LICENSE": (REPO / "LICENSE").read_bytes()}
    if args.package == "lighttouch-firmware":
        staged["iBoot32Patcher-LICENSE"] = (HERE / "vendor/iBoot32Patcher/LICENSE").read_bytes()
    for p in linked(args.package, args.target, args.features):
        root = Path(p["manifest_path"]).parent
        prefix = re.sub(r"[^A-Za-z0-9._-]", "_", f"{p['name']}-{p['version']}-")  # 300.6.1+3.6.3
        terms = sorted(f for f in root.iterdir() if f.is_file() and TERMS.match(f.name))
        for f in terms:
            staged[prefix + f.name] = f.read_bytes()
        if not terms:
            staged[prefix + "LICENSE"] = declared(p)
        if not terms or p["name"] in COMPILED_IN:
            for f in sorted(root.glob("*/*")):  # one level down: fonts/OFL.txt, openssl/LICENSE.txt
                if f.is_file() and NESTED.search(f.name):
                    staged[prefix + f.name] = f.read_bytes()
    files = []
    for path, data in sorted(staged.items()):
        if not re.fullmatch(r"[A-Za-z0-9._-]+", path):  # what the licence screens accept
            raise SystemExit(f"stage-notices: unusable notice name {path!r}")
        (args.out / path).write_bytes(data)
        files.append({"path": path, "sha256": hashlib.sha256(data).hexdigest()})
    manifest = {"schema": 1, "source_reference": args.source_reference, "files": files}
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"{len(files)} notice files in {args.out}")


if __name__ == "__main__":
    main()
