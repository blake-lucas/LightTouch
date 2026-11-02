# Game benchmarks

How the matrix runs: [tools/bench](../tools/bench/README.md). Devices are prepared with their games installed and
without Game Center (`--no-game-center`), warmed up once (each game launched twice, iOS's push notice answered), and
every run starts from that state.

## Baseline, 2026-11-02 (develop 7a38c41c, engine 123cd6d9bb = v0.0.13)

Hosts: Poco F3 (Snapdragon 870, on the charger, debuggable `org.lighttouch.android.bench`) and the Linux build VM
(headless; guest GL through llvmpipe, so GPU-heavy games are host-bound there). 20 s of frames after each game
settles; fps (1% low). Only cells whose screenshots show the game itself are listed.

| Config | Game (screen measured) | Poco F3 | Linux |
| --- | --- | --- | --- |
| iPod touch 2G 3.1.3 | Angry Birds (title) | 60 (54) | 60 (51) |
| | Cut the Rope (menu) | 59 (23) | 60 (33) |
| | Tiny Wings (title) | 60 (47) | 60 (60) |
| | Real Racing (menu) | 59 (30) | 60 (55) |
| | Asphalt 5 (attract race; the game caps ~25) | 25 (2) | 25 (3) |
| iPod touch 3G 4.2.1 | Angry Birds (title) | 60 (30) | 60 (55) |
| | Doodle Jump (menu) | 59 (21) | 60 (60) |
| | Tiny Wings (title) | 58 (27) | 60 (55) |
| | Real Racing (menu) | 50 (3) | 60 (33) |
| iPod touch 4G 6.1.6 | Fruit Ninja (title) | 24 (3) | 29 (4) |
| | Tiny Wings (title) | 60 (46) | 60 (54) |
| | Real Racing (menu) | 45 (2) | 60 (43) |
| | Epic Citadel (flythrough) | 43 (8) | 27 (5) |
| iPhone 4 6.1.3 | Fruit Ninja (title) | 29 (3) | 30 (2) |
| | Tiny Wings (title) | 60 (46) | 59 (24) |
| | Real Racing (menu) | 44 (2) | 60 (47) |
| | Temple Run (a run) | 59 (15) | 60 (32) |
| | Infinity Blade (title) | 25 (5) | 25 (9) |
| | Epic Citadel (flythrough) | 35 (18) | 26 (14) |

Plants vs. Zombies' and Asphalt 5's title screens redraw slowly by design (9-28 fps on both hosts), so they are not
listed; nor is the iPhone 3GS 5.1.1, where every game but Plants vs. Zombies sat under an alert (below).

## Compatibility found

- **Fruit Ninja** (the store's copy for each firmware) quits at launch on the iPod touch 2G 3.1.3 and 3G 4.2.1.
- **Angry Birds 6.1.1** quits at launch on iOS 6 (iPod touch 4G, iPhone 4); a crash report is on the device.
- **Doodle Jump** (2G 3.1.3 copy) quits at launch. **Tiny Wings** never comes to the front on the 3GS 5.1.1.
- **Poco only:** on the iPad 5.1.1, Infinity Blade and Epic Citadel launch but the screen stays on the previous
  game's last frame (no new frames); the desktop draws both at ~24 fps. Epic Citadel also quit on the Poco's 3G.

## Not measured yet (per-game work left)

- iPhone 3GS 5.1.1: with Game Center off, most games show "Could not connect to Game Center server" (Cancel /
  Retry) at each launch; Fruit Ninja also asks for Location Services.
- Late push notices (Infinity Blade and Epic Citadel on the 3G, 3GS and iPad; Asphalt 5 on iOS 6) come after the
  warm-up's taps.
- Game screens: Cut the Rope 2.6's age gate, Doodle Jump 3.16's submit prompt, Jetpack's "Cloud saving" panel and
  results screens, Temple Run reopening paused, Plants vs. Zombies' name entry (2G, 3G).
