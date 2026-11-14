# Phira — private multiplayer chart sharing

This fork of [TeamFlos/Phira](https://github.com/TeamFlos/phira) lets a room host share an imported chart privately with other players through a matching self-hosted multiplayer server. This is an experimental community build; login and account authentication are still required for multiplayer.

> **Score limitation:** these public-source Android, Windows and Linux builds cannot submit official scores, including scores for Phira catalogue charts. They lack the closed official score-record/attestation implementation. Private multiplayer scores are displayed in the room only. Use the official client for official scores and RKS. See the [upstream Android build guide](https://teamflos.github.io/phira-docs/phira_build_guide/Android.html).

## Faster private sharing (protocol v2)

[PR #3](https://github.com/shinova55/phira-custom-mp/pull/3) is merged into `main`. Chart uploads and downloads now stream over the existing multiplayer connection, and clients check a validated cache before downloading. The host reuses its uploaded chart snapshot instead of downloading it again.

**User-reported testing on October 17, 2026:** the PR #3 builds worked, with chart loading reduced from approximately **30 seconds to 2–4 seconds** in the tested setup. This is an observed result, not a guaranteed loading time; chart size, network conditions, server load and cache state affect performance.

> **Update every client and the multiplayer server together.** Current `main` uses protocol **v2**, with no v1 fallback. The older `v0.8.2-private-mp.1` release below uses the previous protocol and does not include these improvements.

## Download this fork

### iPhone and iPad

**[Download Phira 0.8.3 Private Multiplayer Edition for iOS (unsigned IPA)](https://github.com/shinova55/phira-custom-mp/releases/download/v0.8.3-private-mp-ios.1/Phira-iOS-unsigned.ipa)**

Sign build 5 with your usual sideloading tool and install it on your arm64 iPhone or iPad. In Phira, set **Multiplayer Server Address** to your custom protocol v2 server and log in with your Phira account. The Linux server linked below supports this protocol.

This edition includes the font and menu assets required at startup. The app reached the Play screen in an iPhone simulator, and the maintainer confirmed it works on device. [Release notes and SHA-256 checksum](https://github.com/shinova55/phira-custom-mp/releases/tag/v0.8.3-private-mp-ios.1).

### Android, Windows and Linux

**[v0.8.2-private-mp.2 — protocol-v2 testing prerelease](https://github.com/shinova55/phira-custom-mp/releases/tag/v0.8.2-private-mp.2)** contains the merged PR #3 improvements. All packages were built from commit `4964909`. Release downloads do not have the 30-day expiry of Actions artifacts.

| Platform / package | Download | Installation |
| --- | --- | --- |
| Android ARM64, Android 8+ | [APK](https://github.com/shinova55/phira-custom-mp/releases/download/v0.8.2-private-mp.2/phira-arm64-test.apk) | Install the APK; read the signing note below first. |
| Windows x64 | [Portable ZIP](https://github.com/shinova55/phira-custom-mp/releases/download/v0.8.2-private-mp.2/phira-windows-x64.zip) | Extract the entire ZIP and run `Start-Phira.cmd`. Keep the complete `assets` folder, including `phigros.ttf`, beside the executable. The launcher pauses on errors. |
| Linux x64 client | [Client tar.gz](https://github.com/shinova55/phira-custom-mp/releases/download/v0.8.2-private-mp.2/phira-linux-x64.tar.gz) | Targets Ubuntu 24.04 (glibc 2.39) with a desktop/OpenGL environment. Extract and run `./Start-Phira.sh`; see [dependencies](docs/linux-test-build.md). |
| Linux x64 multiplayer server | [Server tar.gz](https://github.com/shinova55/phira-custom-mp/releases/download/v0.8.2-private-mp.2/phira-mp-server-only-linux-x64.tar.gz) | For hosting only. Extract and run `./phira-mp-server --port 12346` on Ubuntu 24.04 x64. Restart the old server with this version. |

**Update all players and the server together.** The [previous protocol-v1 release](https://github.com/shinova55/phira-custom-mp/releases/tag/v0.8.2-private-mp.1) remains available for older installations but is incompatible with this version. Do not mix its packages with protocol v2.

**Android signing:** the APK uses a new ephemeral test certificate and the official package ID. It cannot update the official app or earlier test builds signed with another key, including the PR #3 test APK. Back up/export local data before reinstalling, or use a separate Android user/profile. No release-signing secrets are needed. Linux is not an AppImage and requires system libraries; other distributions and older systems are not verified.

The [release page](https://github.com/shinova55/phira-custom-mp/releases/tag/v0.8.2-private-mp.2) includes **Source code (ZIP / tar.gz)** for the tagged client and modified server, [SHA-256 checksums](https://github.com/shinova55/phira-custom-mp/releases/download/v0.8.2-private-mp.2/SHA256SUMS.txt), and [verification reports](https://github.com/shinova55/phira-custom-mp/releases/download/v0.8.2-private-mp.2/verification-reports.zip). The checksum manifest uses the published download filenames. Source archives exclude closed official code and signing keys; builds still download dependencies and upstream assets, including the Android APK shell.

### What has been verified

- All three platform builds, formatting, Clippy on Linux/macOS/iOS, workspace tests, Windows client tests and multiplayer workspace tests passed. Package source commits, checksums and architectures were checked. Android signature/alignment checks passed. Uploaded release files were downloaded back and SHA-256 verified before publication.
- One controlled latency benchmark is intentionally ignored by normal test runs; it is not a failed test.
- Earliy PR #3 Android/Windows test builds work and reported the 30-second to 2–4-second loading improvement above. These newly packaged release binaries have not separately undergone a full real-device multiplayer test. Not every device, network condition or disconnect/rejoin scenario is covered.

See [private multiplayer details](docs/multiplayer-host-local-charts.md) for the protocol and cache behavior, and the [Windows](docs/windows-test-build.md), [Android](docs/android-test-build.md) and [Linux](docs/linux-test-build.md) notes for platform setup. The score-upload limitation remains unchanged.

## Share a private chart locally

1. Run the matching server described below and give its address to every player.
2. Each player uses this modified client, logs in with their normal Phira account, and connects to the same server and room.
3. The room host imports a chart into **Local**, then selects that imported chart in the room. Imported charts use private transfer even if their metadata contains a catalogue ID.
4. Selection streams the chart to the room server and installs the host’s exact uploaded snapshot locally. Starting/readying first checks the selected archive’s metadata and validated cache; only players without a valid matching cache download and verify the ZIP.

Charts are shared with members of that room without publishing them to the Phira catalogue. Downloaded catalogue charts retain normal catalogue selection; built-in charts must first be imported as a chart ZIP to use private sharing.

## Cache and practical limitations

Currently, transferred charts are cached under `mp-local/<SHA-256>`. They aren’t added to the visible Local library. The hash identifies the archive bytes. With protocol v2, a validated cache hit skips downloading the archive; the host can reuse its uploaded snapshot.

- There is no **Save to Local** feature. Receiving a chart does not add a duplicate entry to the visible library or make it available there for solo play.
- The hash identifies archive bytes, not a catalogue level ID. Repacking a chart can create a different cache folder. Cached files can remain on disk after a match; there is no bounded cache eviction policy.
- Clients request chart metadata before downloading and validate cached file contents against a recorded manifest. Missing, modified or legacy cache entries without a valid manifest trigger a download. A valid cache hit avoids the archive transfer, but metadata checks and room readiness coordination still take time.
- Archives are limited to **32 MiB** compressed, **128 MiB** extracted, and **4096** ZIP entries. Protocol-v2 transfers use ordered **256 KiB** chunks, a **2 MiB** flow-control window and SHA-256 verification.
- Private scores are room-only and are not independently validated by the server. Official score submission is unavailable in these builds, including for catalogue charts.
- All players and the server must use the matching **protocol-v2** implementation. Older private-sharing builds and normal upstream MP servers are not supported. Successful room entry alone is not proof that a differently modified server implements the required upload commands.

## Server structure and setup

The repository includes the modified multiplayer components in a separate Rust workspace:

| Directory | Role |
| --- | --- |
| `phira/` | Game client, chart selection and multiplayer UI |
| `vendor/phira-mp/phira-mp-client/` | Client connection and transfer handling |
| `vendor/phira-mp/phira-mp-common/` | Shared protocol and commands |
| `vendor/phira-mp/phira-mp-server/` | Multiplayer rooms and private chart relay |

```text
Host imports/selects a chart
    -> uploads ZIP to the custom MP server's room memory
    -> host reuses its uploaded snapshot
    -> other room members check metadata and validate their cache
    -> cache misses stream/download and verify the ZIP
    -> clients use mp-local/<SHA-256> and play
```

This is a server-relayed transfer. The server holds the selected archive in room memory while the room exists; it does not publish it to the catalogue or keep a permanent chart library. Only the room host can upload/replace it during chart selection. Authenticated room members can download the current chart. Normal Phira account authentication remains required.

For the prebuilt Linux server, extract the **protocol-v2 server package** above into a writable folder, enter that folder, and run:

```sh
./phira-mp-server --port 12346
```

The server writes logs in its working directory; the package does not install a background service. To build from source instead, install the repository's Rust toolchain and build dependencies, then run from the repository root:

```sh
cargo run --locked --release --manifest-path vendor/phira-mp/Cargo.toml -p phira-mp-server -- --port 12346
```

Allow access to the server port as appropriate for your network, then set **Multiplayer Server Address** in each client to your server's host and port. Use the matching protocol-v2 release server or build current `main` from this repository’s `vendor/phira-mp`; installing an upstream MP server separately will not provide this feature. The server check workflow checks/tests the code; the three-platform prerelease workflow also packages the Linux x64 server.

See [private multiplayer details](docs/multiplayer-host-local-charts.md) for limits and development checks. 
## Upstream Phira

![Phira Showcase](https://phira.5wyxi.com/files/github-showcase.png)

[中文版本](./README-zh_CN.md)

## Official upstream downloads

- [GitHub Release](https://github.com/TeamFlos/phira/releases): For Android, Windows and Linux
- [616.sb mirror](https://616.sb/#phira): Android only
- [App Store](https://apps.apple.com/us/app/phira/id6447435864): For iOS
- ~~[.ipa Release](https://github.com/F-Unction/phira_ipa/releases): For iOS~~

## Contributions

For this fork, use [this repository’s issues](https://github.com/shinova55/phira-custom-mp/issues) and pull requests. The following links and contact information belong to the upstream Phira project.

Issues & pull requests are welcome! Also check these good first [issues](https://github.com/TeamFlos/phira/issues?q=label%3A%22good+first+issue%22) and [pull requests](https://github.com/TeamFlos/phira/issues?q=label%3A%22good+first+issue%22).

If you want to talk with us in depth, email [contact@phira.cn](mailto://contact@phira.cn). Currently we are looking for **Rust** Programmers *with rhythm/music game experience*.

## Translation

See our [Weblate](https://hosted.weblate.org/engage/phira/) page for the latest translation progress. You can easily contribute to translations there.

![Translation Progress](https://hosted.weblate.org/widget/phira/svg-badge.svg)

![Translation Status](https://hosted.weblate.org/widget/phira/multi-auto.svg)

## Star History

[![Stargazers over time](https://starchart.cc/TeamFlos/phira.svg?variant=adaptive)](https://starchart.cc/TeamFlos/phira)
