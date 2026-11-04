# InfernoAndroid

Android-приложение, которое запускает готовый образ iOS 14 (iPhone 11, t8030) на движке
[MakrSas/Inferno](https://github.com/MakrSas/Inferno) (ветка `ios`, коммит в `engine/build-engine.sh`).
Образ в приложение не входит: нужен свой набор файлов, собранный по гайду ChefKiss (тот же, что
использует Inferno на iOS и на компьютере).

Работает только на arm64-телефонах. Эмуляция идёт через TCG, без аппаратной виртуализации, так что
это медленно: первая загрузка занимает минуты.

## Сборка

Нужны NDK r28c (`28.2.13676358`), Android SDK 35, `meson`, `ninja`, `cmake`, `autoconf`/`automake`/`libtool`.

```sh
export ANDROID_NDK_HOME=/path/to/sdk/ndk/28.2.13676358
InfernoAndroid/engine/build-deps.sh        # зависимости → build/deps-prefix
InfernoAndroid/engine/build-engine.sh      # движок → build/engine/libqemu-aarch64-softmmu.so

cd InfernoAndroid
echo "sdk.dir=/path/to/sdk" > local.properties
./gradlew :app:assembleDebug               # → app/build/outputs/apk/debug/app-debug.apk
```

Движок берётся из `build/engine/`, другой путь можно задать через `-PengineDir=/path`. Без движка APK
тоже соберётся, но запустить ВМ не даст.

То же самое руками можно запустить на GitHub: Actions → **InfernoAndroid APK**, APK придёт артефактом.

## Файлы образа

Всё лежит в папке приложения `Android/data/org.lighttouch.inferno/files/`, в той же раскладке, что у Inferno на iOS:

```
files/
├── AppleSEPROM-Cebu-B1
└── InfernoData/
    ├── root.qcow2            (или root — сырой диск)
    ├── firmware  syscfg  ctrl_bits  nvram  effaceable  panic_log
    ├── sep_nvram  sep_ssc
    ├── root_ticket.der
    ├── sep-firmware.n104.RELEASE.new.img4
    └── Restore/              (kernelcache, DeviceTree, trustcache, BuildManifest.plist)
```

Варианты импорта в приложении:

- **Папка** — выбираешь папку, где где-то внутри лежат `InfernoData/` и `AppleSEPROM-Cebu-B1`.
- **ZIP** — архив с той же раскладкой.
- **Файл** — один файл. `AppleSEPROM-Cebu-B1` кладётся рядом с `InfernoData`, остальное — в `InfernoData/`.

Нули при копировании пропускаются, поэтому сырой `root` на 34 ГБ занимает на телефоне столько места,
сколько в нём реально данных. С компьютера можно копировать и напрямую:
`adb push InfernoData AppleSEPROM-Cebu-B1 /sdcard/Android/data/org.lighttouch.inferno/files/`.
qcow2 всё равно удобнее.

Ядро, дерево устройств и trustcache берутся из `Restore/BuildManifest.plist` (identity `n104ap` с Erase).
Если манифеста нет, используются имена iOS 14.0 beta 5.

## Как устроено

- `app/src/main/cpp/inferno_jni.c` открывает движок через `dlopen`, запускает `qemu_init` в своём
  потоке, затем `inferno_display_attach()` и `qemu_main_loop()`. Картинку копирует из
  `inferno_display_read()` прямо в Surface (BGRA→RGBA, только изменившиеся строки).
- ВМ живёт в отдельном процессе `:vm` (`VmService` + `VmActivity`). Если QEMU вызовет `exit()`, лаунчер
  не упадёт, а каждый запуск начинается в новом процессе: `qemu_init` можно вызвать только один раз.
- Аргументы запуска такие же, как в `VMConfig.swift` у Inferno-iOS (`VmConfig.kt`).
- Кнопки: F3 — тише, F4 — громче, F5 — питание, F6 — Home. Кнопки громкости телефона работают как
  кнопки iPhone, «назад» — как Home.
- Батарея телефона показывается в iOS, а Taptic Engine гостя передаётся на вибромотор.
- Журнал движка лежит в `files/logs/engine.log`, консоль iOS — в `files/guest-console.log`, оба видны в
  приложении. Последовательный порт и QMP открыты на `127.0.0.1:4555` и `:4556`
  (`adb forward tcp:4555 tcp:4555`).

## Если не запускается

- Сначала смотри «Журнал → Движок» в приложении: там вся командная строка и то, что сказал QEMU.
- Если падает на выделении памяти под JIT, включи «Дополнительно → split-wx».
- Не хватает памяти: поставь 2 ГБ и половинное разрешение.
