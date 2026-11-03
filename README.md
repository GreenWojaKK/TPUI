# TPUI

A Windows desktop viewer for the raw data and structure of local Parquet files and SQLite tables and for reading Markdown and JSON files, plus a UI component crate shared with other GPUI apps. Built with Rust, [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) and [arrow-rs](https://github.com/apache/arrow-rs).

The viewer focuses on locating source rows, inspecting full raw values and reading documents as they stand on disk. Its display rules, such as Korean line breaking, are built in.

## Documentation

| Document | Contents |
|---|---|
| [Usage guide](usage.md) | Screens, keyboard shortcuts, context menus, query syntax, message files and query logs |
| [Architecture](architecture.md) | Component boundaries, state ownership, engine contracts, rendering, scheduling and memory |
| This README | Build and run instructions, shared UI integration and dependency provenance |

## Features

- Read-only Parquet files and SQLite tables or views, opened in separate tabs
- Explorer, nested schema tree, and a grid virtualized over rows and columns
- Full value detail and copy support for strings, Binary and nested values
- Find, typed filters and single-column sorting, controlled by menus or query text
- Parquet file structure and SQLite table structure views
- Markdown and JSON documents, read when their tab is shown and followed as their files change
- Native query input on Windows, message-file input for external programs, and query logs
- Light and dark themes that initially follow the Windows app color setting

## Crates

| Crate | Contents |
|---|---|
| `viewer-core` | Parquet and SQLite reading, filter/sort/find computation, query text, file and table structure, Markdown and JSON documents |
| `viewer-ui` | GPUI components: light and dark themes, context menu, query window (Windows), message file, tab bar, tables with fixed or wrapping rows, styled wrapping text, buttons, line/band charts, bar charts, strip charts, heatmap, route map |
| `viewer-app` | The viewer app |

## Build and run

Requires Windows x64, Visual Studio Build Tools 2022 (MSVC) and the Windows SDK. The Rust toolchain is set by [rust-toolchain.toml](rust-toolchain.toml).

Run these commands from the repository root:

```powershell
cargo build --release -p viewer-app
.\target\release\viewer-app.exe
```

Start with [opening a file](usage.md#opening-files), then use the [query reference](usage.md#query-text) for find, filter and sort.

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

GPUI is the UI framework in Zed Industries' [Zed](https://github.com/zed-industries/zed) repository, licensed under the Apache License 2.0. This repository takes GPUI from the fork [`GreenWojaKK/zed`](https://github.com/GreenWojaKK/zed), branch `tpui/v1.26.2-slim-image`, commit `f941f6a1`.

Starting from the Zed v1.26.2 release (`c4273271`), the fork only narrows the `image` crate features in Zed's root `Cargo.toml` to the gif, jpeg, png and webp that gpui uses, to shrink the executable. GPUI source code is unchanged. The change is described in the fork's [`FORK.md`](https://github.com/GreenWojaKK/zed/blob/tpui/v1.26.2-slim-image/FORK.md) and in a comment next to the changed lines.

## License

The code in this repository is licensed under the [Apache License 2.0](LICENSE).

Dependencies follow their own licenses. GPUI and arrow-rs are Apache License 2.0; rusqlite and pulldown-cmark are MIT, and the bundled SQLite is in the public domain. When distributing a built executable, include the license notices of these dependencies.
