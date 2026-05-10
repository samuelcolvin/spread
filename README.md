# spread

A simple, fast spreadsheet viewer written in Rust using [GPUI](https://www.gpui.rs/).

It currently opens CSV and XLSX files for viewing only. Editing is intentionally not supported. XLSX rendering includes basic display formatting such as dates, currency, percentages, bold text, colors, and column/row dimensions.

## Usage

Install the binary locally:

```sh
make install
```

Then open a file with:

```sh
spread path/to/file.xlsx
```

or:

```sh
spread path/to/file.csv
```

Useful CLI modes:

```sh
spread --list-sheets path/to/file.xlsx
spread --sheet Summary path/to/file.xlsx
spread --sheet 2 --display xml path/to/file.xlsx
spread --sheet 2 --display table path/to/file.xlsx
```

`--sheet` accepts a sheet name or 1-based sheet index. `--display` can be `gui`, `xml`, or `table`.
