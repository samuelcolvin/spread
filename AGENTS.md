# Repository Guidelines

## Project Structure & Module Organization

This is a small Rust GPUI spreadsheet viewer.

- `src/main.rs` handles CLI parsing, window startup, and app lifecycle.
- `src/workbook.rs` loads CSV/XLSX files, parses workbook metadata, formats displayed values, and exposes the in-memory sheet model.
- `src/view.rs` renders the spreadsheet with GPUI, including headers, cells, scrolling, and scrollbars.
- `scripts/xlsx_inspect.py` is an openpyxl-based reference inspector for comparing XLSX display output.
- `cloud-usage.xlsx` and `pydantic-by-numbers.xlsx` are local fixture workbooks used by tests and manual verification.

Unit tests live inline in the Rust modules under `#[cfg(test)]`.

## Build, Test, and Development Commands

- `make help`: list supported Make recipes.
- `make install`: install the `spread` binary from this checkout with `cargo install --path . --locked`.
- `make format`: run `cargo fmt --all`.
- `make check`: run `cargo clippy --all-targets -- -D warnings`.
- `make test`: run all Rust unit tests.
- `cargo run -- pydantic-by-numbers.xlsx`: open a workbook in the GPUI viewer.
- `cargo run -- --display json pydantic-by-numbers.xlsx`: print the Rust sheet inspection JSON.
- `uv run scripts/xlsx_inspect.py --json pydantic-by-numbers.xlsx`: print the openpyxl reference JSON.

## Coding Style & Naming Conventions

Use standard Rust formatting via `cargo fmt`. Keep code modular: parsing and formatting belong in `workbook.rs`; rendering and input handling belong in `view.rs`; CLI concerns stay in `main.rs`.

Prefer descriptive snake_case names for functions and variables. Types use UpperCamelCase. Keep comments sparse and only add them when they explain non-obvious behavior or API constraints.

## Testing Guidelines

Use Rust unit tests with `#[test]`, colocated with the module being tested. Name tests after the behavior, for example `formats_percentage_values` or `loads_pydantic_by_numbers_dollar_formats`.

At the end of every run, you MUST run `make check` and fix any issues. If the change is testable or could affect behavior, you MUST also run `make test` and fix any failures.

```sh
make check
make test
```

For XLSX display behavior, compare Rust JSON output against the Python reference script when practical.

## Commit & Pull Request Guidelines

This repository currently has no commit history, so there is no established commit convention. Use short, imperative commit messages such as `Add percentage formatting` or `Fix horizontal scroll sync`.

Pull requests should include a concise description, the files or behavior changed, and verification commands run. Include screenshots or short screen recordings for GPUI rendering or scrolling changes.
