# TPUI

A Windows desktop viewer for the raw data and structure of local Parquet files and SQLite tables, plus a UI component crate shared with other GPUI apps. Built with Rust, [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) and [arrow-rs](https://github.com/apache/arrow-rs).

Analysis and aggregation belong in other tools; this viewer focuses on showing raw values without truncation. The design and behavior contracts are in [architecture.md](architecture.md).

## Features

- Opening a file reads only its metadata, and only the visible rows and columns are decoded. Decoded blocks live in a 128 MiB cache shared by all tabs.
- Explorer, tabs, schema tree, and a grid virtualized over rows and columns
- Value detail: long strings, Binary and nested values shown without truncation and copied exactly as stored
- Find cells whose whole value matches, filters and column sort from the right-click menus of headers and cells. Conditions on different columns are joined with `and`; conditions on one column also with `or`, shown in parentheses. Shift+click adds with `or`, Alt+click adds the opposite condition, `▾` boxes between conditions switch `and` and `or`, and dragging across conditions of one column puts them in one set of parentheses. The conditions read as query text (`filter (age < 20 or age >= 60) and status == "활성"`), which `Copy Query` copies
- File structure: size, compression, encodings, null counts and min/max per column and Row Group
- SQLite: databases expand into their tables and views in the explorer; each opens as a tab with the same grid, value detail, find, filter and sort, plus a table structure view (columns, declared and stored types, indexes, `CREATE` statement). Databases are opened read-only and never locked for writers.
- Queries as text: the toolbar's `Query` menu opens a query window for writing `find`, `filter` and `sort` lines, and copies the path of a message file that agents and other programs write queries to. Every applied query is appended to a daily log in `%LOCALAPPDATA%\TPUI\query-log`
- Light and dark themes: follows the Windows app color setting, switchable from the toolbar

## Crates

| Crate | Contents |
|---|---|
| `viewer-core` | Parquet and SQLite reading, filter/sort/find computation, query text, file and table structure. No GPUI dependency |
| `viewer-ui` | GPUI components: light and dark themes, context menu, query window (Windows), message file, tab bar, tables with fixed or wrapping rows, buttons, line/band charts, bar charts, strip charts, heatmap, route map |
| `viewer-app` | The viewer app |

## Build

Requires Windows x64, Visual Studio Build Tools 2022 (MSVC) and the Windows SDK. The Rust toolchain is set by `rust-toolchain.toml`.

```
cargo build --release -p viewer-app
```

The result is `target/release/viewer-app.exe`.

## Using `viewer-ui` in another app

Add it as a git dependency and pin GPUI to the same fork commit as this repository. With a different commit, GPUI is built twice and the types do not match.

```toml
[dependencies]
viewer-ui = { git = "https://github.com/GreenWojaKK/TPUI", rev = "<TPUI commit>" }
gpui = { git = "https://github.com/GreenWojaKK/zed", rev = "f941f6a1c26a295c051e2a3776d9897093b3db8c" }
```

Components read colors by role and take their fonts and densities from the theme. An app with its own look starts from the default theme, changes the fields it needs and installs it once, before the first window opens. Colors of the app's own views come from `theme::pick(dark, light)`.

```rust
let mut theme = viewer_ui::theme::Theme::default();
theme.light.text = gpui::rgb(0x345575);
theme.metrics.list_row_height = gpui::px(32.);
viewer_ui::theme::install(theme);
```

## GPUI fork

GPUI is the UI framework in Zed Industries' [Zed](https://github.com/zed-industries/zed) repository, licensed under the Apache License 2.0. This repository takes GPUI from the fork [`GreenWojaKK/zed`](https://github.com/GreenWojaKK/zed), branch `tpui/v1.26.2-slim-image`, commit `f941f6a1`, instead of the original.

Starting from the Zed v1.26.2 release (`c4273271`), the fork only narrows the `image` crate features in Zed's root `Cargo.toml` to the gif, jpeg, png and webp that gpui uses, to shrink the executable. GPUI source code is unchanged. The change is described in the fork's [`FORK.md`](https://github.com/GreenWojaKK/zed/blob/tpui/v1.26.2-slim-image/FORK.md) and in a comment next to the changed lines.

## License

The code in this repository is licensed under the [Apache License 2.0](LICENSE).

Dependencies follow their own licenses. GPUI and arrow-rs are Apache License 2.0; rusqlite is MIT, and the bundled SQLite is in the public domain. When distributing a built executable, include the license notices of these dependencies.
