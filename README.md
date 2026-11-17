# TPUI

English | [한국어](docs-translate/ko/README.md)

TPUI is a desktop UI foundation built on a fork of [GPUI](https://github.com/GreenWojaKK/zed), developed for a workspace that brings coding agents, terminal sessions and their working files together. The intended setup uses Rust/GPUI for the interface, Go for the agent core, and tmux for terminal session management.

The aim is to keep the interface's added memory use small while several agents and development tools run alongside it.

The bundled `viewer-app` is the first application of this stack. It uses this foundation to browse source code, documents and structured data.

## UI components

| Area | Components |
|---|---|
| Text | Wrapping and styled text, selection |
| Layout and navigation | Tables, tabs, menus and buttons |
| Visualization | Line and band charts, bar and strip charts, heatmaps and route maps |
| App integration | Light and dark themes, Windows native input window, message-file inbox |

Apps supply data and handle component events. See [Architecture](architecture.md#shared-ui-components) for component responsibilities.

## Using `viewer-ui` in an app

Add it as a git dependency and pin GPUI to the same fork commit as this repository. With a different commit, GPUI is built twice and the types do not match.

```toml
[dependencies]
viewer-ui = { git = "https://github.com/GreenWojaKK/TPUI", rev = "<TPUI commit>" }
gpui = { git = "https://github.com/GreenWojaKK/zed", rev = "f941f6a1c26a295c051e2a3776d9897093b3db8c" }
```

Customize the default theme and install it before opening the first window:

```rust
let mut theme = viewer_ui::theme::Theme::default();
theme.light.text = gpui::rgb(0x345575);
theme.metrics.list_row_height = gpui::px(32.);
viewer_ui::theme::install(theme);
```

## GPUI fork

TPUI uses [GreenWojaKK/zed](https://github.com/GreenWojaKK/zed), based on Zed v1.26.2, with image codecs selected to reduce executable size. The dependency example above pins the required commit. Fork changes are recorded in [FORK.md](https://github.com/GreenWojaKK/zed/blob/tpui/v1.26.2-slim-image/FORK.md).

## Repository

| Location | Role |
|---|---|
| `crates/viewer-ui` | Reusable GPUI components and theme |
| `crates/viewer-core` | File readers, document models and table queries |
| `crates/viewer-app` | Viewer application and PDF worker |
| `core` | Source-code token service |

## Viewer application

`viewer-app` uses TPUI to browse files in an explorer and tabs.

| Files | View and actions |
|---|---|
| Source code and configuration files | Line numbers, text selection and copy; syntax colors for Python, Go, Rust, JavaScript and TypeScript |
| Markdown | Formatted text, tables, code blocks, local images, links and text selection |
| JSON and JSON Lines | Expandable trees, value paths and full values; key order, duplicate keys and number text are preserved |
| Parquet and SQLite tables or views | Data grid, schema, full values, find, typed filters, single-column sorting and storage structure |
| PDF | Pages fitted to the view width, rendered on Windows |

See [Viewer usage](usage.md) for controls, queries and editor integration.

### Build and run

The Windows x64 build requires Visual Studio Build Tools 2022 (MSVC), the Windows SDK and the toolchain in [rust-toolchain.toml](rust-toolchain.toml). Go 1.24 or later builds the source-code token service; without Go, source files display as plain text.

Run from the repository root:

```powershell
cargo build --release -p viewer-app
.\target\release\viewer-app.exe
```

Cargo builds `viewer-app.exe` and `tpui-pdf.exe`. When Go is available, the build also produces `tpui-core.exe`. Keep the built executables together when moving the viewer.

Component design and the viewer's implementation are described in [Architecture](architecture.md).

## License

The code in this repository is licensed under the [Apache License 2.0](LICENSE).

Dependencies follow their own licenses. GPUI and arrow-rs are Apache License 2.0; rusqlite and pulldown-cmark are MIT, the Go standard library in `tpui-core` is BSD, and the bundled SQLite is in the public domain. When distributing a built executable, include the license notices of these dependencies.
