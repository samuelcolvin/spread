use std::{path::PathBuf, sync::Arc};

use anyhow::{Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use gpui::{
    App, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, prelude::*,
    px, size,
};

use crate::{
    view::{CopySelection, SpreadsheetViewer, WINDOW_HEIGHT, WINDOW_WIDTH},
    workbook::load_workbook,
};

mod view;
mod workbook;

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(&cli) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<()> {
    validate_output_mode(cli)?;

    let workbook = Arc::new(load_workbook(&cli.path)?);

    if cli.list_sheets {
        print_sheet_list(&workbook);
        return Ok(());
    }

    let sheet_ix = resolve_sheet(&workbook, cli.sheet.as_deref())?;

    #[cfg(feature = "debug")]
    if cli.debug {
        serde_json::to_writer_pretty(std::io::stdout(), &workbook.sheet(sheet_ix).inspect())?;
        println!();
        return Ok(());
    }

    match cli.display {
        DisplayMode::Gui => {}
        DisplayMode::Xml => {
            print!("{}", workbook.sheet(sheet_ix).to_pretty_xml());
            println!();
            return Ok(());
        }
        DisplayMode::Table => {
            print_terminal_table(workbook.sheet(sheet_ix));
            return Ok(());
        }
        DisplayMode::Audit => {
            let audits = workbook.formula_audits(cli.sheet.as_ref().map(|_| sheet_ix))?;
            print_formula_audits(&audits);
            let exit_code = formula_audit_exit_code(&audits);
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            return Ok(());
        }
    }

    let title = format!("spread - {}", workbook.display_name());

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        cx.bind_keys([KeyBinding::new(
            "cmd-c",
            CopySelection,
            Some("SpreadsheetViewer"),
        )]);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.clone().into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            {
                let workbook = Arc::clone(&workbook);
                move |window, cx| {
                    cx.new(|cx| SpreadsheetViewer::new(Arc::clone(&workbook), sheet_ix, window, cx))
                }
            },
        )
        .unwrap_or_else(|error| panic!("failed to open window for {title}: {error}"));

        cx.activate(true);
    });

    Ok(())
}

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(name = "spread", about = "View CSV and XLSX spreadsheets")]
struct Cli {
    /// Sheet name or 1-based sheet index to open/export.
    #[arg(long)]
    sheet: Option<String>,

    /// Print available sheets and exit.
    #[arg(long)]
    list_sheets: bool,

    /// Display mode.
    #[arg(long, value_enum, default_value_t = DisplayMode::Gui)]
    display: DisplayMode,

    /// Print internal debug JSON and exit.
    #[cfg(feature = "debug")]
    #[arg(long)]
    debug: bool,

    /// Spreadsheet file to open.
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum DisplayMode {
    #[default]
    Gui,
    Xml,
    Table,
    Audit,
}

fn validate_output_mode(cli: &Cli) -> Result<()> {
    let output_modes = usize::from(cli.list_sheets)
        + usize::from(cli.display != DisplayMode::Gui)
        + debug_output_mode_count(cli);

    if output_modes > 1 {
        bail!("choose only one output mode: --list-sheets, --display, or --debug");
    }

    Ok(())
}

#[cfg(feature = "debug")]
fn debug_output_mode_count(cli: &Cli) -> usize {
    usize::from(cli.debug)
}

#[cfg(not(feature = "debug"))]
fn debug_output_mode_count(_: &Cli) -> usize {
    0
}

fn print_sheet_list(workbook: &workbook::WorkbookData) {
    for (sheet_ix, sheet_name) in workbook.sheet_names().enumerate() {
        println!("{}\t{sheet_name}", sheet_ix + 1);
    }
}

fn print_terminal_table(sheet: &workbook::SheetData) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    let header = std::iter::once(String::new())
        .chain((0..sheet.col_count()).map(workbook::column_label))
        .collect::<Vec<_>>();
    table.set_header(header);

    for row_ix in 0..sheet.row_count() {
        let row = std::iter::once((row_ix + 1).to_string())
            .chain((0..sheet.col_count()).map(|col_ix| sheet.cell_data(row_ix, col_ix).value))
            .collect::<Vec<_>>();
        table.add_row(row);
    }

    println!("{table}");
}

fn print_formula_audits(audits: &[workbook::FormulaAudit]) {
    for audit in audits {
        println!("<sheet name=\"{}\">", xml_attr(&audit.sheet));
        println!(
            "  <uncached_values>{}</uncached_values>",
            audit.uncached_values
        );
        println!(
            "  <cached_matches>{}</cached_matches>",
            audit.cached_matches
        );
        if audit.inconsistencies.is_empty() {
            println!("</sheet>");
            continue;
        }

        println!(
            "  <inconsistencies count=\"{}\">",
            audit.inconsistencies.len()
        );
        for inconsistency in &audit.inconsistencies {
            println!(
                "    <inconsistency cell=\"{}\">",
                xml_attr(&inconsistency.cell)
            );
            println!(
                "      <formula>{}</formula>",
                xml_text(&inconsistency.formula)
            );
            println!(
                "      <cached_value>{}</cached_value>",
                xml_text(&inconsistency.cached_value)
            );
            println!(
                "      <calculated_value>{}</calculated_value>",
                xml_text(&inconsistency.calculated_value)
            );
            println!("    </inconsistency>");
        }
        println!("  </inconsistencies>");
        println!("</sheet>");
    }
}

fn xml_text(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(ch),
        }
    }
    output
}

fn xml_attr(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(ch),
        }
    }
    output
}

fn formula_audit_exit_code(audits: &[workbook::FormulaAudit]) -> i32 {
    i32::from(audits.iter().any(|audit| !audit.inconsistencies.is_empty()))
}

fn resolve_sheet(workbook: &workbook::WorkbookData, sheet: Option<&str>) -> Result<usize> {
    let Some(sheet) = sheet else {
        return Ok(0);
    };

    workbook.sheet_index(sheet).ok_or_else(|| {
        let available = workbook.sheet_names().collect::<Vec<_>>().join(", ");
        anyhow!("sheet '{sheet}' not found; available sheets: {available}")
    })
}

#[cfg(test)]
fn parse_args<I, T>(args: I) -> Result<Cli>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Ok(Cli::try_parse_from(args)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_path() {
        let cli = parse_args(["spread", "book.csv"]).unwrap();
        assert_eq!(cli.path, PathBuf::from("book.csv"));
        assert_eq!(cli.sheet, None);
        assert!(!cli.list_sheets);
        assert_eq!(cli.display, DisplayMode::Gui);
    }

    #[test]
    fn parses_display_xml_sheet_output_mode() {
        let cli = parse_args([
            "spread",
            "--display",
            "xml",
            "--sheet",
            "Summary",
            "book.xlsx",
        ])
        .unwrap();
        assert_eq!(cli.path, PathBuf::from("book.xlsx"));
        assert_eq!(cli.sheet.as_deref(), Some("Summary"));
        assert_eq!(cli.display, DisplayMode::Xml);
    }

    #[test]
    fn parses_display_table_output_mode() {
        let cli = parse_args(["spread", "--display", "table", "book.xlsx"]).unwrap();
        assert_eq!(cli.display, DisplayMode::Table);
    }

    #[test]
    fn parses_audit_output_mode() {
        let cli = parse_args(["spread", "--display", "audit", "book.xlsx"]).unwrap();
        assert_eq!(cli.display, DisplayMode::Audit);
    }

    #[test]
    fn rejects_old_formula_errors_output_mode() {
        let error = parse_args(["spread", "--display", "formula-errors", "book.xlsx"]).unwrap_err();
        assert!(error.to_string().contains("invalid value"));
    }

    #[test]
    fn rejects_old_xml_flag() {
        let error = parse_args(["spread", "--xml", "book.xlsx"]).unwrap_err();
        assert!(error.to_string().contains("unexpected argument"));
    }

    #[cfg(feature = "debug")]
    #[test]
    fn parses_debug_output_mode() {
        let cli = parse_args(["spread", "--debug", "book.xlsx"]).unwrap();
        assert!(cli.debug);
    }

    #[cfg(not(feature = "debug"))]
    #[test]
    fn rejects_debug_without_feature() {
        let error = parse_args(["spread", "--debug", "book.xlsx"]).unwrap_err();
        assert!(error.to_string().contains("unexpected argument"));
    }

    #[test]
    fn rejects_missing_cli_path() {
        let error = parse_args(["spread"]).unwrap_err();
        assert!(error.to_string().contains("required"));
    }

    #[test]
    fn audit_reports_exit_code_for_inconsistencies() {
        let audit = workbook::FormulaAudit {
            sheet: "Sheet1".to_owned(),
            inconsistencies: vec![workbook::FormulaInconsistency {
                cell: "A1".to_owned(),
                formula: "1+1".to_owned(),
                cached_value: "3".to_owned(),
                calculated_value: "2".to_owned(),
            }],
            ..Default::default()
        };

        assert_eq!(formula_audit_exit_code(&[audit]), 1);
        assert_eq!(
            formula_audit_exit_code(&[workbook::FormulaAudit::default()]),
            0
        );
    }

    #[test]
    fn audit_xml_escapes_text_and_attributes() {
        assert_eq!(xml_attr("A&B\"C"), "A&amp;B&quot;C");
        assert_eq!(xml_text("1 < 2 && 3 > 2"), "1 &lt; 2 &amp;&amp; 3 &gt; 2");
    }
}
