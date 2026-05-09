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
