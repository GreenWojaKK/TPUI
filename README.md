# TPUI

A Windows desktop viewer for the raw data and file structure of local Parquet files, plus a UI component crate shared with other GPUI apps. Built with Rust, [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) and [arrow-rs](https://github.com/apache/arrow-rs).

Analysis and aggregation belong in other tools; this viewer focuses on showing raw values without truncation. The design and behavior contracts are in [architecture.md](architecture.md).

## Features

- Opening a file reads only its metadata, and only the visible rows and columns are decoded. Decoded blocks live in a 128 MiB cache shared by all tabs.
- Explorer, tabs, schema tree, and a grid virtualized over rows and columns
- Value detail: long strings, Binary and nested values shown without truncation and copied exactly as stored
- Find cells whose whole value matches, per-column filters, column sort
- File structure: size, compression, encodings, null counts and min/max per column and Row Group
- Light and dark themes: follows the Windows app color setting, switchable from the toolbar

## Crates

| Crate | Contents |
|---|---|
| `viewer-core` | Parquet reading, filter/sort/find computation, file structure. No GPUI dependency |
| `viewer-ui` | GPUI components: light and dark themes, single-line input, tab bar, table, buttons, line/band charts, heatmap, route map |
| `viewer-app` | The Parquet viewer app |

## Build

Requires Windows x64, Visual Studio Build Tools 2022 (MSVC) and the Windows SDK. The Rust toolchain is set by `rust-toolchain.toml`.

```
cargo build --release -p viewer-app
```

The result is `target/release/viewer-app.exe`.

The chart example draws probabilistic forecast explorer data (`explorer-data.json`).

```
cargo run -p viewer-ui --example forecast_charts -- <explorer-data.json> [YYYY-MM-DD]
```

The map example draws the route map blocks of analysis runs over a background of area polygons.

```
cargo run -p viewer-ui --example route_map -- <runs-directory> <map-context.json>
```

## Using `viewer-ui` in another app

Add it as a git dependency and pin GPUI to the same fork commit as this repository. With a different commit, GPUI is built twice and the types do not match.

```toml
[dependencies]
viewer-ui = { git = "https://github.com/GreenWojaKK/TPUI", rev = "<TPUI commit>" }
gpui = { git = "https://github.com/GreenWojaKK/zed", rev = "f941f6a1c26a295c051e2a3776d9897093b3db8c" }
```

## GPUI fork

GPUI is the UI framework in Zed Industries' [Zed](https://github.com/zed-industries/zed) repository, licensed under the Apache License 2.0. This repository takes GPUI from the fork [`GreenWojaKK/zed`](https://github.com/GreenWojaKK/zed), branch `tpui/v1.26.2-slim-image`, commit `f941f6a1`, instead of the original.

Starting from the Zed v1.26.2 release (`c4273271`), the fork only narrows the `image` crate features in Zed's root `Cargo.toml` to the gif, jpeg, png and webp that gpui uses, to shrink the executable. GPUI source code is unchanged. The change is described in the fork's [`FORK.md`](https://github.com/GreenWojaKK/zed/blob/tpui/v1.26.2-slim-image/FORK.md) and in a comment next to the changed lines.

## License

The code in this repository is licensed under the [MIT License](LICENSE).

Dependencies follow their own licenses. GPUI and arrow-rs are Apache License 2.0. When distributing a built executable, include the license notices of these dependencies.
