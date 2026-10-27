# Android application source assembly

This is a source-delivery and build-input recipe for the Kotlin/JNI/Portable
application, alongside the separate engine and firmware component kits. It does
not contain guest assets, authorize a target operation, establish an executed
rebuild/relink, or settle the complete APK's license classification. Use
[dependency coverage](../../Android/DEPENDENCY-NOTICES.md),
[frontend notices](../../Android/FRONTEND-NOTICES.md),
and [the preparer](README.md) together.

## Existing generators and their boundaries

| Existing material | Reuse | Boundary |
| --- | --- | --- |
| Product's scoped Git archive, per-file Git mode/SHA inventory and terminal build receipts | Exact application export and actual producer binding | Retained candidate plans are private evidence, not a public generic exporter or proof that the next candidate compiled |
| Engine `contrib/portable/audit-selected-sources.py`, `package-notices.py`, `package-source.py` | Each final ABI's selected source/notices, dependency archives, patches, objects/static libraries and relink recipe | Use the clean producer and matching build/dependency stamps; an earlier source kit does not cover a changed engine |
| Exported engine `prepare-bundle-source.py` and `relink-bundle.py` | Writable source preparation and original/replacement relink | Validation-only, actual relink and full source rebuild are separate results |
| Firmware `tools/export-xpwn-source.py` and `tools/package-notices.py` | Pinned selected XPwn and original notices | The firmware kit's standalone `BUILD-FIRMWARE.sh` does not assemble the Android APK |
| Firmware correspondence assembler retained with producer receipts | Dependency source archives, generated-source recipe, CActivation, actual per-ABI firmware mappings and compile inputs | Existing private assemblers need explicit new producer bindings; preserve old immutable kits |
| Android `recordApkRuntimeArtifacts`, `package-frontend-notices.py`, `stage-frontend-notices.py` | Exact selected runtime graph and original AAR/JAR/POM/nested-JAR notice bytes | Verification metadata or notices are not dependency source archives or an offline build cache |
| Android `stage-engine-notices.py`, `stage-angle-runtime.py`, `check-apk.py` | Hash-bound staging and actual packaged closure/notice checks | Provider staging is not an ANGLE source exporter, nor proof of guest execution |

No inspected Android script currently creates and verifies a complete application
source assembly in one command. Reuse the scoped Git export and the component
generators; a future wrapper should join their manifests rather than recursively
copying a Gradle cache, guest handoff or worktree.

## Minimum application source export

Freeze the actual app/native revisions before exporting. For the common case
where both come from one revision, the existing export can be reproduced without
reading untracked assets:

```sh
# Run in the application Git repository; FINAL_APP_REV is a full verified commit.
# OUT is a fresh source-only directory outside the repository.
mkdir -m 700 "$OUT"
git archive --format=tar --prefix=application/ \
  --output="$OUT/application-source.tar" "$FINAL_APP_REV" \
  Android Portable crates Cargo.toml Cargo.lock \
  README.md LICENSE Portable/firmware/firmware-catalog.json
sha256sum "$OUT/application-source.tar"
```

Inventory every member's relative name, SHA256, Git blob and executable mode;
reject unsafe paths and unexpected links. Preserve all tracked build scripts,
AIDL, Kotlin/Java, C/C++, JNI helper headers, CMake files, resources, tests,
verification metadata, wrapper properties/JAR and catalog. This export includes
root grants and CActivation that earlier incomplete snapshots omitted. Do not
copy a source kit's intentionally omitted wrapper executable policy into a
whole-application export without an exact replacement/provisioning recipe.

If app and native producer revisions differ, export the actual Android tree and
actual Portable/CActivation tree separately. Bind each file to its producer and
record any byte-equality proof explicitly. Do not substitute current HEAD.
Export the three consumed QEMU bridge headers/source from their exact engine
revision and retain their hashes. Engine source can be supplied by its separate
kit, with an accessible relative reference, without copying it a second time.
Keep test-only overlays/copies distinct from main source, including their original
tracked location, destination and actual byte proof. A source file inventory is
not a claim that every file was compiled.

## Build input receipt

Store a versioned assembly manifest beside the source archive. Leave final
artifact fields unset until actual terminal receipts exist. At minimum bind:

- App/native/bridge Git revisions, dirty status, archive and per-file hashes,
  modes, root grants, catalog, staging/overlay and generator revisions.
- Main APK hash/size/package, build variant, supported ABI/API, public signing
  certificate fingerprint, and a separately identified test APK when conveyed.
- Per ABI, engine producer manifest, source kit and notices; actual unstripped
  to packaged engine/libc++/JNI/firmware/provider mappings, strip tool and flags.
- Actual CMake configure/build directories, compile commands, dependency files,
  generated source, link commands and effective flags. Preserve assertions,
  debug RPC/provider gates, optimization choices and compiled header identities.
- Exact build invocation, working-directory remapping, relevant environment,
  terminal result and provenance linking any preserved prelaunch command. A
  prelaunch `executed=false` field stays historical even when an adjacent
  executed-command addendum proves the subsequent run.
- Tool provenance: OS/host ABI, JDK vendor/full runtime/build and binary hashes,
  Gradle distribution/wrapper hashes, AGP/Kotlin plugin versions, SDK platform,
  build-tools, CMake/Ninja, NDK properties/compiler/strip hashes and package
  revisions. Record dependency compiler/API stamps separately from app API.
- Selected variant Maven coordinates/versions/artifact hashes, original
  LICENSE/NOTICE/POM provenance, source references and exact provisioning
  instructions. Include build plugin/tool dependency verification, too. Label
  an offline build unproven until its required dependency inputs are supplied
  and a fresh-cache replay succeeds; an existing warm `--offline` build is not
  that proof. Do not distribute an entire credential-bearing Gradle cache.
- Firmware selected XPwn pin/patch generation, OpenSSL/JSON/tinyxml2 source
  archives and original grants, crypto build stamps and exact CActivation.
  ANGLE requires each ABI's actual GN arguments, source/dependency/toolchain
  pins, selected linked inputs/patches, original notices and source-access
  route; a closure JSON or repository URL alone does not supply those sources.

The inspected source baseline pins Gradle8.11.1 (distribution SHA256
`f397b287023acdba1e9f6fc5ea72d22dd63669d59ed4a289a29b1a76eee151c6`),
AGP8.9.2, Kotlin2.1.20, NDK28.2.13676358, CMake3.22.1,
compile/target API35, build-tools35.0.0, minimum API29 and Java/Kotlin17.
These source pins do not identify a JDK patch level or prove which tool executable
built a later APK. The inspected current host JDK release is Ubuntu17.0.20.1;
record it as a current observation unless the producer captured the same bytes.
The inspected firmware receipt records Ninja1.10.2, while its OpenSSL producer
records1.11.1: do not collapse those into one assumed tool version.

Never include a private signing key, debug keystore, credentials, SDK guest
firmware or writable guest state. A recipient can sign a rebuild with their own
key; it will have a different identity and may not update the installed app.
Do not prescribe uninstall/data loss to work around that difference. Retaining
public certificate fingerprints is sufficient for artifact identity comparison;
it does not make the signer's key reproducible.

## Final assembly and source access

Use a fresh assembly directory containing the application archive/inventory,
normalized build instructions and receipt, plus references to accessible engine,
firmware and provider source material. Hardlink unchanged immutable legal source
archives where appropriate; never relabel an old correspondence manifest for new
binaries. Resolve placeholders to newly generated build roots/prefixes so the
recipient need not possess the original producer's absolute paths. Keep private
raw logs/cache locations outside a public source package.

The proposed common debug package adds the fixed ARM64 ANGLE trio alongside the
existing x86 trio. If all other inputs stay unchanged this would yield20 native
DSOs and261 notice payloads; those numbers are a planning expectation, not a
compiled-artifact receipt. Derive the actual per-ABI set and notice groups from
the final APK, including separate ARM/x86 provider metadata. A diagnostic
provider remains explicitly selected; platform default and target qualification
are separate. The final engine may change after this plan.

For local delivery to the same Blake UID, retain a receipt of effective user,
UID/GID, exact source-kit paths/manifest hashes, ancestor traversal permissions,
file readability and actual opened manifest/README bytes. This can establish
local access without duplicating the engine kits in the guest ZIP. Check access
for the actual recipient, not only a privileged operator. A manifest hash alone
is not delivery, and local host access does not prove source delivery to a phone
or public availability. Preserve covered-code rights independently of privacy
rules for the mixed guest-asset archive.

Final checks must distinguish source bytes supplied, actual source/configure
replay, original/replacement relink, APK rebuild, and target acceptance. Missing
executed replay evidence should remain explicit; it must not become a fabricated
build pass or a blanket license-clearance conclusion.
