# TPUI Viewer Usage

English | [한국어](docs-translate/ko/usage.md)

This guide covers `viewer-app`, the file viewer built with TPUI. Use it to open source code, documents and structured data and move between their views. For building the viewer or using TPUI components in an app, see [README](README.md). The [architecture](architecture.md) describes the shared UI and the viewer's implementation.

- [Opening files](#opening-files) · [Explorer](#explorer) · [Tabs](#tabs)
- [Code](#code-documents) · [Markdown](#markdown-documents) · [JSON](#json-documents) · [JSON Lines](#json-lines-documents) · [PDF](#pdf-documents)
- [Parquet and SQLite](#parquet-and-sqlite) · [Queries](#queries-and-message-files)
- [Theme and usage](#theme-and-usage)

## Opening files

- `Open File` or `Ctrl+O` opens the native file dialog. Cancelling keeps the current state.
- A file given on the command line, as Windows does for a file associated with the viewer, opens the same way. On Windows, when a viewer is already running, the file opens as a tab there and that window comes to the front; several files opened at once share the window. Starting the viewer without a file opens another window.
- A selected file with the `.md`, `.markdown`, `.json`, `.jsonl` or `.pdf` extension or a [code extension](#code-documents) opens as a [document](#markdown-documents). A selected file with the SQLite header opens its first table or view by name, and the explorer expands the database to show the others; a database without tables shows a notice in the status bar. Any other file is validated by the Parquet reader regardless of extension.
- Opening a new source creates and activates a tab, then reads the source for its view. A source is a Parquet file, a document, or a SQLite database and a table name. If the source is already open in a tab, that tab is activated. A SQLite tab is titled `table · file name`.
- For Parquet, the footer and metadata provide the row count and schema before the first screen of data is read. Opening a SQLite table also inspects its rows to determine stored types and row positions.
- When a file is opened directly and the explorer has no root, its parent directory becomes the explorer root. An existing root is kept.
- To associate files on Windows, keep the viewer executables in a stable folder and choose `viewer-app.exe` under `Open with → Choose another app` for the file type.

## Explorer

- `Open Folder` selects the root directory. Expanding a folder lists only its direct children.
- Directories, files with the `.parquet`, `.md`, `.markdown`, `.json`, `.jsonl` or `.pdf` extension or a [code extension](#code-documents), and SQLite databases are shown. A database is a file with the `.sqlite`, `.sqlite3`, `.db`, `.db3`, `.s3db` or `.sl3` extension that starts with the SQLite header; other `.db` files are hidden. Extensions are case-insensitive.
- Expanding a database lists its tables, views and virtual tables by name. Internal `sqlite_` tables and the shadow tables of virtual tables are left out. Views are tagged `view`.
- Directories come first, then names in order within each kind. Symbolic links and directory junctions are not followed automatically.
- Double-clicking a Parquet file, a document or a table, or selecting it and pressing Enter, opens it in a tab. A single click expands a folder or database.
- A failed folder listing shows an error and retry on that node. Grids of files already open are kept.

## Screen overview

The toolbar opens files and folders and switches the theme. The explorer lists files and database objects. Tabs select the active source, whose type determines the main view and supporting panels.

| Active source | Main view | Supporting panels |
|---|---|---|
| Code | Source lines and line numbers | Full-width document area |
| Markdown | Formatted document | Full-width document area |
| JSON and JSON Lines | Expandable tree | Value panel for the selected node |
| Parquet and SQLite | Data grid or storage structure | Schema and Value panels |
| PDF | Pages fitted to the view width | Full-width document area |

The status bar shows the active path, file-specific information and read status. Shared navigation uses the explorer and tabs; each format's controls are described below.

## Tabs

- Click a tab to switch files. `Ctrl+Tab` and `Ctrl+Shift+Tab` move between tabs; `Ctrl+W` closes the active tab.
- Drag tabs to reorder them. Hold a dragged tab near an edge to reach tabs outside the visible area. Scroll an overflowing tab bar with the wheel.
- Hover a tab to see its full path.
- Data tabs remember grid position, selection, schema expansion and row view. Document tabs remember their reading position; JSON also remembers expanded values and the selected path.
- Closing the active tab selects its right neighbour, or its left neighbour when there is none to the right.

## Documents and source code

### Code documents

- A file with one of these extensions opens as code, in a tab of its own that reads across the whole width: `.py`, `.pyi`, `.go`, `.rs`, `.ts`, `.tsx`, `.mts`, `.cts`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.c`, `.h`, `.cc`, `.cpp`, `.hpp`, `.cs`, `.java`, `.kt`, `.swift`, `.lua`, `.php`, `.rb`, `.css`, `.html`, `.xml`, `.sql`, `.sh`, `.bash`, `.ps1`, `.toml`, `.yaml`, `.yml`.
- Python, Go, Rust, JavaScript and TypeScript have syntax colors; other supported extensions display as plain text. Opening code does not execute it.
- Lines stay on one row; use `Shift` with the wheel to scroll horizontally. Very long lines show an omitted-length note. A selection reaching that note copies the whole line.
- Drag to select text, double-click a word, or triple-click a line. `Ctrl+A` selects the file and `Ctrl+C` copies the selection with its original line endings.
- Arrow keys, `Page Up`, `Page Down`, `Ctrl+Home` and `Ctrl+End` scroll the document.
- If the code token service cannot run, the status bar shows the reason and the file remains readable as plain text. Files containing a NUL byte are reported as binary.

### Markdown documents

- A file with the `.md` or `.markdown` extension opens as a document, in a tab of its own. The text fills a centered column, and the side panels make way for it.
- Text wraps at spaces and line breaks, so Korean words stay whole; a word wider than the whole line is broken between characters.
- Headings, paragraphs, bold, italic, strikethrough, inline code, bullet, numbered and task lists, quotes, tables with their column alignment, code blocks, rules, footnotes and a metadata block at the top are drawn.
- Code blocks keep their spaces and lines. A long line continues on the next line, tabs are drawn up to the next stop of four columns, and copying gives the original text.
- HTML, math and text in double brackets are shown as written.
- An image alone in its paragraph is drawn at up to the column's width, from a path relative to the document or an absolute path. A PNG, JPEG, WebP or GIF that can be read is shown; otherwise its alt text is. An image at a web address shows its alt text and address.
- Clicking a `http`, `https` or `mailto` link opens it in the browser. Other link destinations are shown as links and stay where they are.
- Dragging selects text across blocks, a double click selects a word and a triple click a whole text. `Ctrl+A` selects the document and `Ctrl+C` copies the selection as plain text, with table cells separated by tabs.
- Arrow keys, `Page Up`, `Page Down`, `Ctrl+Home` and `Ctrl+End` scroll. A vertical scrollbar follows the document.

### JSON documents

- A file with the `.json` extension opens as a tree. Objects and arrays expand on a click, or with the arrow keys and `Enter`. Large arrays and objects group their children into expandable ranges.
- Keys stay in the file's order, duplicates included, and numbers stay as written: `1.10` reads `1.10`.
- The Value panel shows the selected value's place in the document, such as `$.routes[3].name`, its type and its full text. `Ctrl+C` copies strings without their quotes and escapes, and other values as they stand in the file.
- The file must be JSON as defined by RFC 8259. A file with comments or trailing commas shows the reason with its line and character column.

### JSON Lines documents

- A file with the `.jsonl` extension opens as a tree of its lines, one JSON value per line. Each row shows the file's line number and the line's text. Blank lines are skipped, and long files group lines into expandable ranges.
- Clicking a line opens its value, which reads as in a JSON document. The Value panel shows the place as `line 12: $.routes[3].name`.
- A line that is not valid JSON stays in its place in the error color, and the other lines stay readable. Selecting it shows the reason and its character column in the Value panel, and the status bar counts these lines.
- `Ctrl+C` copies a line as it stands in the file, and on the `file` row the whole file.

### PDF documents

- On Windows, PDF pages appear vertically at the view width. This view supports page viewing and scrolling; zoom, text selection, find and links are unavailable.
- Use the wheel, arrow keys, `Page Up`, `Page Down`, `Ctrl+Home` or `Ctrl+End` to scroll. The status bar shows the page count and current page.
- A read or rendering failure shows the reason. `Retry` reloads the document and restarts its worker when needed.

### How documents are read

- Active documents follow saved file changes and restore the reading position after reloading.
- A failed read shows the reason and `Retry`. File checks continue so a later saved change can be picked up.
- Text files must be UTF-8; a UTF-8 byte order mark is accepted. JSON and JSON Lines files must be smaller than 4 GiB.
- Find, filter, sort and queries apply to Parquet and SQLite. On document tabs, `Ctrl+F` and queries show a notice.

### Editing documents

Right-click a Markdown or code document and choose `Open in Editor`. The file opens at the selection's start, or the right-clicked position when nothing is selected. Markdown positions refer to the source text; clicking outside text opens the file at its start.

Set `$VISUAL` or `$EDITOR` to choose the editor, in that priority order. Otherwise Windows uses the registered app or Notepad, and Linux uses `xdg-open`.

| Cursor support | Editors |
|---|---|
| Line and column | vim, Neovim, nano, gedit, Emacs, VS Code, VSCodium, Cursor, Notepad++ |
| Line | vi |
| File start | Other editors |

Terminal editors open in their own terminal window. On Linux, `$TERMINAL` selects the terminal; otherwise an installed terminal is chosen automatically. Saving in the editor refreshes the open document. A launch failure is shown in the status bar.

## Parquet and SQLite

These tabs provide a data grid, schema, full values and storage structure. Find, filter and sort select source rows and their display order.

### Schema view

- Shows the Arrow schema's field names, data types and nullability.
- Expand `Struct`, `List` and `Map` fields to inspect their child types.
- Grid columns are top-level fields. Expanding nested fields does not change the grid's column or row count.
- The file summary shows the total rows, top-level columns, Row Groups and file size.
- A file with zero rows still shows its schema and column headers, with an empty data state in the body.

### Data grid

- Uses fixed-height rows, fixed-width columns, and fixed header and row number areas.
- Provides vertical and horizontal scrollbars, mouse wheel, and Shift+wheel for horizontal movement.
- A click selects one cell and highlights it. The selected cell is the target of value detail.
- With the grid focused, arrow keys move the selected cell by rows and columns, Page Up/Down move by a screen, and Ctrl+Home/End go to the first and last rows.
- Cells being read show a placeholder and are updated when data arrives.
- A cell is a one-line preview of its value. Newlines, tabs and control characters become display symbols, and long values get an ellipsis at the preview length. The full raw value is shown in value detail.

### Value detail

- Select a cell to inspect its full value in the Value panel. Strings retain their line breaks, and Binary values appear as a hex dump. The panel keeps the value when the cell scrolls off screen.
- Expand `Struct`, `List` or `Map` and select a child to inspect it. The panel shows the source row, column, type and value size.
- `Ctrl+C` copies the selected cell or node. Strings copy as original text, Binary as lowercase hexadecimal, Null as `null` and other scalars as displayed text. Nested values copy as JSON; Map entries become key/value objects, and nested Binary, non-finite numbers and time values become strings.

### Row view

- Each tab has filter conditions and a sort key. Together they decide which rows the grid shows and in what order; this is the row view. Without filter or sort, the row view is the source file's row order.
- Grid row positions, scrolling, selection and key movement follow the row view's order. The row number area shows the source file's row numbers, and so does the Value panel.
- Changing the filter or sort makes the worker build a new row view. While it is being built, the grid keeps showing the previous row view, and the filter and sort bar above the grid shows progress and `Cancel`. When complete, the grid moves to the new row view's first row and clears the selection; the horizontal scroll position is kept. `Cancel` restores the conditions from before the change.
- The filter and sort bar is shown when there are conditions and reads as the query text: each condition and the sort is a chip removed with `×`, operators are boxes with `▾`, and conditions joined by `or` are in parentheses. `Copy Query` copies the whole query text, including the find value while the find bar is open, and `Clear All` returns to source order. The status bar shows the row view's row count together with the total.
- Each tab remembers its row view. Conditions and results survive tab switches.

### Find

- `Ctrl+F` opens the query window with the active tab's conditions and a `find` line, the cursor after `find` (on systems without the query window, the message file is pointed to). The value can also come from `Find` in a cell's context menu or from a `find` line in any query.
- The find bar above the grid shows the value being found, `Change…`, which opens the query window again, `Previous`, `Next`, the status and `×`. `Esc` in the grid closes the find bar.
- Find locates cells whose whole value exactly equals the input. Matching is case-sensitive.
- Find compares the complete displayed value: original text for strings, displayed numbers, times and Boolean values, and hexadecimal for Binary. Hexadecimal input is case-insensitive. Null cells and `Struct`, `List` and `Map` cells are not find targets.
- `F3` and `Next` move to the next match, `Shift+F3` and `Previous` to the previous one. The order is the row view's row order, then column order within a row. The search starts from the selected cell and wraps around at the end.
- The found cell is selected and scrolled into view. The find bar shows the number of matching cells in the whole row view, and the grid highlights matching cells.
- `Find` in a cell's context menu puts the cell's full value into the find bar and moves to its next match.
- The first search for a new value reads the file to compute match positions. During the computation, the find bar shows progress, and grid scrolling, cell selection and tab switching keep working.
- A find sent together with new conditions runs once they are applied. `F3` while conditions are being applied shows a notice in the find bar. After a tab switch, find results are recomputed for the new tab.

### Context menus

- Right-clicking a column header opens its menu: `Sort Ascending`, `Sort Descending`, `Clear Sort` on the sorted column, `Only Nulls`, `Exclude Nulls`, and `Remove Conditions on This Column` when the column has conditions.
- Right-clicking a cell selects it and opens its menu once the cell's full value is read. A cell with a value offers conditions comparing the column with that value (`=`, `≠`, and `>`, `≥`, `<`, `≤` where the column type allows them), `Find` and `Copy Value`. A Null cell offers `Only Nulls` and `Exclude Nulls`. Nested values offer no conditions.
- Condition items act with modifier keys: Shift+click joins the column's conditions with `or`, and Alt+click adds the opposite condition.
- Conditions use the cell's full value. The value text is the copy text of the cell: strings as is, Binary as lowercase hexadecimal, other values as displayed, which parses back to the same value.
- Choosing an item applies it and closes the menu. Clicking outside or `Esc` closes the menu without an action.

### Filter

- Conditions come from the context menus and from query text (see below).
- A filter has the shape of a spreadsheet filter: clauses that must all hold, joined by `and`. A clause is one or more conditions on one column of which at least one must hold, joined by `or` and drawn in parentheses when there are several: `(OBE_ID == "366" or OBE_ID == "977") and id >= 5 and id < 20`. So `or` joins conditions on one column only, and parentheses never nest.
- Clauses on the same column sit next to each other, in the order their columns first appear.
- A condition from a menu is added with `and` after the column's other clauses. With Shift it joins the column's last clause with `or`. An `=` condition on a column that already has an `=` condition joins that clause with `or` without Shift, since two different values joined by `and` match no row.
- Each operator box in the filter and sort bar switches between `and` and `or`. Inside parentheses, `and` splits the clause there into two; between two clauses on one column, `or` merges them. Between clauses on different columns only `and` can be chosen.
- Dragging across conditions in reading order sweeps a range. Releasing it over the conditions makes the range exactly one clause, joined by `or` inside and by `and` to the conditions just outside it; a range of one condition or of several columns changes nothing, and `Esc` or releasing elsewhere drops the sweep. While sweeping, the bar shows what releasing does.
- Operators depend on the column type:

| Column type | Operators |
|---|---|
| String | `=`, `≠`, `contains`, `<`, `≤`, `>`, `≥`, `null`, `not null` |
| Boolean, integer, float, Decimal, Date, Time, Timestamp | `=`, `≠`, `<`, `≤`, `>`, `≥`, `null`, `not null` |
| Binary | `=`, `≠`, `null`, `not null` |
| `Struct`, `List`, `Map` and other types | `null`, `not null` |

- Values of comparison operators are parsed as the column type and compared by value. For example, `= 1.5` on a Decimal column matches `1.50`. A value that cannot be parsed is not applied, and the filter and sort bar shows the reason.
- String comparison and `contains` are case-sensitive. String `<` and `>` use UTF-8 byte order. Binary values are entered as hexadecimal.
- Comparison operators and `contains` never match Null cells. Null cells are selected with the `null` operator.
- Columns with a filter show their header name in the accent color.

### Sort

- Each header click cycles ascending, descending, then no sort. The header's context menu sets a direction or clears the sort directly. The sort key is a single column; clicking another column's header switches to ascending on that column. The header shows the direction with `▲` or `▼`.
- With a filter, only rows that pass the filter are sorted.
- Null goes last regardless of direction. Rows with equal values keep the source file's order.
- Strings sort by UTF-8 byte order, and floating-point NaN sorts as the largest value.
- Columns of `Struct`, `List`, `Map` and types without display support are not sorted. Clicking such a header shows a notice in the filter and sort bar that the column cannot be sorted.

### File structure

- Switch between `Data` and `File Structure` at the tab bar. Each tab remembers its choice; `Ctrl+F` returns to data and opens find.
- Inspect the file summary, column storage information and Row Groups. Select a Row Group to show its columns; `All` shows the combined information. Hover truncated cells for full text and encodings.
- Statistics use `≈` when truncated in the file and `-` when absent or for nested leaves. Null counts are combined only when every Row Group records them.

### Table structure

- SQLite tabs provide `Table Structure` for the table or view definition, row order, columns, indexes and database properties.
- Column information includes the declared type and the Arrow type derived from stored values. The table's `CREATE` statement is shown beside the indexes; hover an index for its `CREATE INDEX` statement.

## Queries and message files

Queries apply to the active Parquet or SQLite tab. Menus, the query window and message files update the same filter, sort and find state.

### Query text

A query is the text form of a row view and a find value, one command per line. The filter and sort bar shows conditions in this form, context menus label their items with it, and `Copy Query` writes it.

```text
find 홍길동
filter age >= 30 and status == "활성"
sort -created_at
```

- `find <value>` finds cells whose whole value equals the value.
- `filter <expression>` keeps rows for which the expression holds. Conditions are joined with `and`, and conditions on one column also with `or`, in the shape of the filter above. One level joins with one word, so mixing `and` and `or` takes parentheses: `filter (a == 1 or a == 2) and b == 3`. A line of `or` alone needs none: `filter a == 1 or a == 2`. Several `filter` lines combine with `and`.
- `or` between different columns, `and` inside `or`, and mixing without parentheses are errors that point at the place.
- A written query puts each clause of several conditions in parentheses, as the bar shows it.
- Conditions are `<column> <op> <value>` with `==`, `!=`, `<`, `<=`, `>`, `>=` or `contains`, or `<column> is null` and `<column> is not null`. `=` is read as `==`. Operators follow the column type as in the filter table above.
- `sort <column>` sorts ascending and `sort -<column>` descending, by one column. A query has at most one `find` and one `sort` line.
- Values without spaces are written as is; others are quoted as `"…"` with `\"`, `\\`, `\n`, `\t` and `\r` escapes. Other backslashes stay as written, so Windows paths read back unchanged. Values of string columns are always written quoted.
- Column names with spaces or symbols are written in backticks, with a backtick inside doubled: `` `운행 일자` ``. Names must match exactly; a name that differs only in case is suggested in the error.
- Keywords are case-insensitive and blank lines are ignored. A query states the whole view: a missing line means no find, filter or sort, and an empty query clears them.
- Values are parsed as their column types with the same rules as conditions from the menus. An error names the line and character column and the reason, such as ``line 1, column 8: this `(` is not closed``.
- Writing a query and reading it back gives the same conditions, sort and find value.

### Query input and log

A query replaces the active Parquet or SQLite tab's filter, sort and find state. Parsing errors preserve the existing state and show the line, column and reason. Find submitted with new conditions starts after those conditions are applied.

On Windows, choose `Query → Write Query…` to edit the current query. `Ctrl+Enter` or `Apply` applies it and keeps the window open; `Esc` or `Close` closes it. Reopening the window reloads the active tab's query.

External programs can choose `Copy Message File Path` and write the complete query as UTF-8 to that file. UTF-8 BOM and CRLF line endings are accepted. The query is applied after the file settles; the file is removed when the viewer exits.

| Platform | App storage directory |
|---|---|
| Windows | `%LOCALAPPDATA%\TPUI` |
| Linux | `$XDG_STATE_HOME/tpui`, or `~/.local/state/tpui` when unset |

Message files use `inbox/<process id>.txt` under this directory. Queries and parse errors are appended to `query-log/<date>.jsonl` with the source, query and result or error. A row-view query is logged when it completes; superseded computations and clicks that leave the query unchanged add no entry.

`Open Query Log Folder` opens the log location. Logs contain query text, including any cell values in the query, and remain until the user deletes them.

## Theme and usage

### Theme

- Colors come in a light and a dark set. At startup the app follows the Windows app color setting and keeps following its changes. The toolbar's `Light Mode` / `Dark Mode` button switches the theme, after which the app no longer follows the setting until restart.

### Memory and CPU use

- On Windows, press `Usage` in the status bar for a new reading. Its values remain until the next press.
- Memory is the viewer and its code/PDF processes' combined working set, excluding GPU memory. CPU is their share of the whole machine since the previous reading.
- Daily readings are stored as `usage-log/<date>.jsonl` in the [app storage directory](#query-input-and-log). Open them as JSON Lines and delete old logs when desired. `not logged` indicates that a reading could not be written.
