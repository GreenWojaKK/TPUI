# TPUI Usage

This guide covers opening data, navigating the viewer, inspecting values, applying find, filter and sort queries, and reading Markdown and JSON documents. For building and running the app, see [README.md](README.md#build-and-run). For implementation details and data contracts, see [architecture.md](architecture.md).

## Screen overview

```text
┌─────────────────────────────────────────────────────────────────────┐
│ Open File    Open Folder               Active file name   Light Mode │
├────────────────┬──────────────────────────────────┬─────────────────┤
│ Explorer       │ [a.parquet ×][b.parquet ×] [Data|File Structure] │ Schema │
│                │ Find bar · Filter and sort bar    │ Fields · types  │
│ Expand folders │ Data Grid                        │ nullable        │
│ Select files   │ Fixed headers · sort/filter marks │ Nested fields   │
│                │ Source row numbers / visible cells├─────────────────┤
│                │ Vertical/horizontal scroll · cell │                 │
│                │ selection                        │ Value           │
│                │                                  │ Raw value       │
│                │                                  │ Nested value tree│
├────────────────┴──────────────────────────────────┴─────────────────┤
│ Active tab's path · total rows · columns · Row Groups · read state  │
└─────────────────────────────────────────────────────────────────────┘
```

## Explorer

- `Open Folder` selects the root directory. Expanding a folder lists only its direct children.
- Directories, files with the `.parquet`, `.md`, `.markdown` or `.json` extension, and SQLite databases are shown. A database is a file with the `.sqlite`, `.sqlite3`, `.db`, `.db3`, `.s3db` or `.sl3` extension that starts with the SQLite header; other `.db` files are hidden. Extensions are case-insensitive.
- Expanding a database lists its tables, views and virtual tables by name. Internal `sqlite_` tables and the shadow tables of virtual tables are left out. Views are tagged `view`.
- Directories come first, then names in order within each kind. Symbolic links and directory junctions are not followed automatically.
- Double-clicking a Parquet file, a document or a table, or selecting it and pressing Enter, opens it in a tab. A single click expands a folder or database.
- A failed folder listing shows an error and retry on that node. Grids of files already open are kept.

## Opening files

- `Open File` or `Ctrl+O` opens the native file dialog. Cancelling keeps the current state.
- A selected file with the `.md`, `.markdown` or `.json` extension opens as a [document](#markdown-documents). A selected file with the SQLite header opens its first table or view by name, and the explorer expands the database to show the others; a database without tables shows a notice in the status bar. Any other file is validated by the Parquet reader regardless of extension.
- Opening a new source creates a new tab, activates it and starts in the `Opening` state. A source is a Parquet file, a document, or a SQLite database and a table name. If the source is already open in a tab, that tab is activated. A SQLite tab is titled `table · file name`.
- For Parquet, the footer and metadata provide the row count and schema before the first screen of data is read. Opening a SQLite table also inspects its rows to determine stored types and row positions.
- When a file is opened directly and the explorer has no root, its parent directory becomes the explorer root. An existing root is kept.

## Tabs

- Each source opens in one tab. Reopening the same Parquet file or the same SQLite table activates its existing tab; different tables of one database have separate tabs.
- The tab bar above the grid shows file names and close buttons. Clicking a tab switches to it; `Ctrl+Tab` and `Ctrl+Shift+Tab` go to the next and previous tab, and `Ctrl+W` closes the active tab.
- With many tabs, widths shrink evenly down to a minimum; beyond that the tab bar scrolls horizontally. It scrolls with the mouse wheel, and scrolls to keep the active tab visible when it changes. When tabs overflow, a scroll position indicator is shown below the bar.
- Hovering a tab shows the file's full path as a tooltip, so files stay distinguishable when shrunk tabs truncate their names.
- Dropping a dragged tab on another tab moves it to that tab's slot; dropping it on empty space moves it to the last slot. While dragging, the tab at the drop slot is highlighted. Reordering keeps the active tab. Holding a dragged tab near either end of the bar keeps scrolling the bar that way, so it can be moved to slots out of view.
- The grid, schema, Value panel and status bar show the active tab's content.
- Each tab remembers its scroll position, selected cell and schema expansion. Returning to a tab requests cell data again from that state, and blocks still in the cache are shown without I/O.
- Closing the active tab activates the tab to its right, or else to its left. Closing all tabs returns to the empty state.

## Schema view

- Shows the Arrow schema's field names, data types and nullability.
- Expand `Struct`, `List` and `Map` fields to inspect their child types.
- Grid columns are top-level fields. Expanding nested fields does not change the grid's column or row count.
- The file summary shows the total rows, top-level columns, Row Groups and file size.
- A file with zero rows still shows its schema and column headers, with an empty data state in the body.

## Data grid

- Uses fixed-height rows, fixed-width columns, and fixed header and row number areas.
- Provides vertical and horizontal scrollbars, mouse wheel, and Shift+wheel for horizontal movement.
- A click selects one cell and highlights it. The selected cell is the target of value detail.
- With the grid focused, arrow keys move the selected cell by rows and columns, Page Up/Down move by a screen, and Ctrl+Home/End go to the first and last rows.
- Cells being read show a placeholder and are updated when data arrives.
- A cell is a one-line preview of its value. Newlines, tabs and control characters become display symbols, and long values get an ellipsis at the preview length. The full raw value is shown in value detail.

## Value detail

- The Value panel shows the selected cell's value exactly as stored. Without a selected cell, it shows an empty state.
- The top of the panel shows the row number, column name, type and value size. The value size is the byte length and character count for strings, the byte length for Binary, and the element or entry count for List and Map.
- Strings are shown in full with their newlines, and Binary as a hex dump of all bytes. Any value can be scrolled to its end regardless of length.
- `Struct`, `List` and `Map` values expand into a value tree. Selecting a node in the tree shows that node's raw value in the body.
- `Ctrl+C` copies the raw value of the selected cell or node.
- The Value panel keeps its value even when the selected cell scrolls out of the grid.

## Row view

- Each tab has filter conditions and a sort key. Together they decide which rows the grid shows and in what order; this is the row view. Without filter or sort, the row view is the source file's row order.
- Grid row positions, scrolling, selection and key movement follow the row view's order. The row number area shows the source file's row numbers, and so does the Value panel.
- Changing the filter or sort makes the worker build a new row view. While it is being built, the grid keeps showing the previous row view, and the filter and sort bar above the grid shows progress and `Cancel`. When complete, the grid moves to the new row view's first row and clears the selection; the horizontal scroll position is kept. `Cancel` restores the conditions from before the change.
- The filter and sort bar is shown when there are conditions and reads as the query text: each condition and the sort is a chip removed with `×`, operators are boxes with `▾`, and conditions joined by `or` are in parentheses. `Copy Query` copies the whole query text, including the find value while the find bar is open, and `Clear All` returns to source order. The status bar shows the row view's row count together with the total.
- Each tab remembers its row view. Conditions and results survive tab switches.

## Find

- `Ctrl+F` opens the query window with the active tab's conditions and a `find` line, the cursor after `find` (on systems without the query window, the message file is pointed to). The value can also come from `Find` in a cell's context menu or from a `find` line in any query.
- The find bar above the grid shows the value being found, `Change…`, which opens the query window again, `Previous`, `Next`, the status and `×`. `Esc` in the grid closes the find bar.
- Find locates cells whose whole value exactly equals the input. Matching is case-sensitive.
- The comparison target is the full text shown for the cell: the original text for strings, the text of the cell display contract for numbers, times and Boolean, and the same lowercase hexadecimal as the copy text for Binary. Hexadecimal input is case-insensitive. Null cells and `Struct`, `List` and `Map` cells are not find targets.
- `F3` and `Next` move to the next match, `Shift+F3` and `Previous` to the previous one. The order is the row view's row order, then column order within a row. The search starts from the selected cell and wraps around at the end.
- The found cell is selected and scrolled into view. The find bar shows the number of matching cells in the whole row view, and the grid highlights matching cells.
- `Find` in a cell's context menu puts the cell's full value into the find bar and moves to its next match.
- The first search for a new value reads the file to compute match positions. During the computation, the find bar shows progress, and grid scrolling, cell selection and tab switching keep working.
- A find sent together with new conditions runs once they are applied. `F3` while conditions are being applied shows a notice in the find bar. After a tab switch, find results are recomputed for the new tab.

## Context menus

- Right-clicking a column header opens its menu: `Sort Ascending`, `Sort Descending`, `Clear Sort` on the sorted column, `Only Nulls`, `Exclude Nulls`, and `Remove Conditions on This Column` when the column has conditions.
- Right-clicking a cell selects it and opens its menu once the cell's full value is read. A cell with a value offers conditions comparing the column with that value (`=`, `≠`, and `>`, `≥`, `<`, `≤` where the column type allows them), `Find` and `Copy Value`. A Null cell offers `Only Nulls` and `Exclude Nulls`. Nested values offer no conditions.
- Condition items act with modifier keys: Shift+click joins the column's conditions with `or`, and Alt+click adds the opposite condition. While the menu is open, the labels follow the keys held, so they show what a click adds. A condition already in the filter is marked `✓`.
- Conditions use the cell's full value. The value text is the copy text of the cell: strings as is, Binary as lowercase hexadecimal, other values as displayed, which parses back to the same value.
- Choosing an item applies it and closes the menu. Clicking outside or `Esc` closes the menu without an action.

## Filter

- Conditions come from the context menus and from query text (see below).
- A filter has the shape of a spreadsheet filter: clauses that must all hold, joined by `and`. A clause is one or more conditions on one column of which at least one must hold, joined by `or` and drawn in parentheses when there are several: `(OBE_ID == "366" or OBE_ID == "977") and id >= 5 and id < 20`. So `or` joins conditions on one column only, and parentheses never nest.
- Clauses on the same column sit next to each other, in the order their columns first appear, so each column's conditions read together.
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

## Sort

- Each header click cycles ascending, descending, then no sort. The header's context menu sets a direction or clears the sort directly. The sort key is a single column; clicking another column's header switches to ascending on that column. The header shows the direction with `▲` or `▼`.
- With a filter, only rows that pass the filter are sorted.
- Null goes last regardless of direction. Rows with equal values keep the source file's order.
- Strings sort by UTF-8 byte order, and floating-point NaN sorts as the largest value.
- Columns of `Struct`, `List`, `Map` and types without display support are not sorted. Clicking such a header shows a notice in the filter and sort bar that the column cannot be sorted.

## Query text

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

## Query input and log

Queries reach a tab in three ways, all applied the same way and logged the same way.

| Way | For |
|---|---|
| Context menus and header clicks | Quick changes with the mouse |
| Query window (Windows) | A person writing a query, such as `contains` or a value not on screen |
| Message file | Agents and other programs, and systems without the query window |

- A query text states the whole view of the active tab: its conditions, sort and find value replace the current ones. A text that does not parse changes nothing; the reason, with line and column, is shown on a line of its own in the filter and sort bar and in the query window's status.
- A find sent with new conditions runs once their row view is applied, since find works on finished row views.
- The toolbar's `Query` menu has `Write Query…`, `Copy Query`, `Copy Message File Path` and `Open Query Log Folder`. The context menus of headers and cells also have `Write Query…`.

On Windows, the query window uses the operating system's text input control.

- It holds the system's multi-line edit control, so typing, Korean IME composition, clipboard and undo are the system's. It opens over the viewer, owned by it, filled with the active tab's current query; opening it again refills it and brings it to the front.
- `Ctrl+Enter` or `Apply` applies the text and keeps the window open; `Esc` or `Close` closes it. The status line shows the row count once the row view is applied, or the error.

Agents and other programs can apply a query by writing its complete text to the message file.

- Each running viewer has its own file, `%LOCALAPPDATA%\TPUI\inbox\<process id>.txt` (`~/.local/state/tpui/inbox/` elsewhere), created empty at start. `Copy Message File Path` copies its path.
- The file is checked every 0.5 seconds. A change is applied once the file has stayed the same for one more check, which skips the empty file a writer leaves between truncating and writing. A UTF-8 byte order mark is dropped and `\r\n` line ends are read as `\n`.
- The file is removed when the viewer quits. Files of viewers that ended without quitting are removed when the next viewer starts.

Every applied query goes to the query log, `%LOCALAPPDATA%\TPUI\query-log\<date>.jsonl`, one JSON line per query.

- A line holds the local time, the source file and SQLite table, the origin (`click`, `window` or `file`), the whole query text, and the row count of the resulting row view or the error. A text that does not parse is logged as received, with its error.
- A query that starts a row view computation is logged when the computation ends. A computation replaced by a newer query before it ends is not logged, and a click that leaves the query unchanged is not logged again.
- Lines are only appended; the log is never rewritten or trimmed. The log stays in the app's own folder. Queries can contain cell values, so the log can hold personal data from the files viewed; `Open Query Log Folder` shows where it is, and the user deletes it when they want to.

## File structure

- `Data` and `File Structure` at the right of the tab bar switch the tab body. Each tab remembers its last choice. `Ctrl+F` in the file structure returns to the data and opens find.
- The file structure shows stored Parquet metadata without scanning data pages.
- The top shows the file summary: Parquet format version, the program that wrote the file, compressed and uncompressed data size and ratio, and the keys and value sizes of the key-value metadata.
- The middle table shows, per leaf column, the path, physical and logical type, codec, compressed and uncompressed size, ratio, null count, min, max, and whether a page index and Bloom filter exist. Hovering a cell shows truncated content in full and the list of encodings as a tooltip.
- The bottom table shows, per Row Group, the row count, compressed and uncompressed size, and ratio. The first row, `All`, is the total over all Row Groups. Selecting a Row Group switches the middle table to that Row Group's information.
- Min and max use the same display format as data cells. Statistics truncated in the file get a `≈` prefix. Without statistics, or for leaves of nested columns, `-` is shown. Null counts are summed only when every Row Group records them.

## Table structure

- For a SQLite tab, the structure button reads `Table Structure`. It shows the schema and pragmas of the table; no rows are read.
- The top shows the kind (table, view or virtual table, with `WITHOUT ROWID` and `STRICT`), the row count, the row order, and the database: file size, page count and size, text encoding and journal mode.
- The column table shows, per column, the name, declared type, the Arrow type the grid shows it as, the storage classes found in it, `NOT NULL`, the default and the primary key position.
- The bottom shows the indexes (name, columns, uniqueness, origin and whether partial, with the `CREATE INDEX` statement as a tooltip) next to the table's `CREATE` statement.

## Markdown documents

- A file with the `.md` or `.markdown` extension opens as a document, in a tab of its own. The text fills a centered column, and the side panels make way for it.
- Text wraps at spaces and line breaks, so Korean words stay whole; a word wider than the whole line is broken between characters. Every document reads this way.
- Headings, paragraphs, bold, italic, strikethrough, inline code, bullet, numbered and task lists, quotes, tables with their column alignment, code blocks, rules, footnotes and a metadata block at the top are drawn.
- Code blocks keep their spaces and lines. A long line continues on the next line, tabs are drawn up to the next stop of four columns, and copying gives the original text.
- HTML, math and text in double brackets are shown as written.
- An image alone in its paragraph is drawn at up to the column's width, from a path relative to the document or an absolute path. A PNG, JPEG, WebP or GIF that can be read is shown; otherwise its alt text is. An image at a web address shows its alt text and address.
- Clicking a `http`, `https` or `mailto` link opens it in the browser. Other link destinations are shown as links and stay where they are.
- Dragging selects text across blocks, a double click selects a word and a triple click a whole text. `Ctrl+A` selects the document and `Ctrl+C` copies the selection as plain text, with table cells separated by tabs.
- Arrow keys, `Page Up`, `Page Down`, `Ctrl+Home` and `Ctrl+End` scroll. A vertical scrollbar follows the document.

## JSON documents

- A file with the `.json` extension opens as a tree. Objects and arrays expand on a click, or with the arrow keys and `Enter`. Arrays and objects with more than 1,000 members group them into ranges.
- Keys stay in the file's order, duplicates included, and numbers stay as written: `1.10` reads `1.10`.
- The Value panel shows the selected value's place in the document, such as `$.routes[3].name`, its type and its full text. `Ctrl+C` copies strings without their quotes and escapes, and other values as they stand in the file.
- The file must be JSON as defined by RFC 8259. A file with comments or trailing commas shows the reason with its line and character column.

## How documents are read

- A document's file is read when its tab is shown and released when another tab is shown; the tab remembers its scroll position, or its expanded values and selection. Nothing about a document is stored on disk.
- While a document is shown, its file is checked every 0.5 seconds. A change is shown once the file has stayed the same for one more check, at the same place in the document. A file that can no longer be read shows the reason and `Retry`, and a later change is picked up by itself.
- Files must be UTF-8; a UTF-8 byte order mark is dropped. Other encodings show the reason.
- The viewer never writes to these files.
- Find, filter, sort and queries work on Parquet and SQLite tabs. On a document tab, `Ctrl+F` and queries show a notice in the status bar.

## Theme

- Colors come in a light and a dark set. At startup the app follows the Windows app color setting and keeps following its changes. The toolbar's `Light Mode` / `Dark Mode` button switches the theme, after which the app no longer follows the setting until restart.
