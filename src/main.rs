use std::{env, ffi::OsString, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};

use crate::{
    view::{SpreadsheetViewer, WINDOW_HEIGHT, WINDOW_WIDTH},
    workbook::load_workbook,
};

mod view;
mod workbook;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = parse_args(env::args_os())?;
    let path = cli.path;
    let workbook = Arc::new(load_workbook(&path)?);

    if cli.output == OutputMode::Json {
        serde_json::to_writer_pretty(std::io::stdout(), &workbook.inspect())?;
        println!();
        return Ok(());
    }

    let title = format!("spread - {}", workbook.display_name());

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);

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
                move |_, cx| cx.new(|_| SpreadsheetViewer::new(Arc::clone(&workbook)))
            },
        )
        .unwrap_or_else(|error| panic!("failed to open window for {title}: {error}"));

        cx.activate(true);
    });

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct Cli {
    output: OutputMode,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Window,
    Json,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Cli> {
    let mut args = args.into_iter();
    let _binary = args.next();
    let mut output = OutputMode::Window;
    let mut path = None;

    for arg in args {
        if arg == "--json" {
            output = OutputMode::Json;
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            bail!("Usage: spread [--json] <file.csv|file.xlsx>");
        }
    }

    let Some(path) = path else {
        bail!("Usage: spread [--json] <file.csv|file.xlsx>");
    };

    Ok(Cli { output, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_path() {
        let cli = parse_args(["spread", "book.csv"].map(OsString::from)).unwrap();
        assert_eq!(cli.path, PathBuf::from("book.csv"));
        assert_eq!(cli.output, OutputMode::Window);
    }

    #[test]
    fn parses_json_output_mode() {
        let cli = parse_args(["spread", "--json", "book.xlsx"].map(OsString::from)).unwrap();
        assert_eq!(cli.path, PathBuf::from("book.xlsx"));
        assert_eq!(cli.output, OutputMode::Json);
    }

    #[test]
    fn rejects_missing_cli_path() {
        let error = parse_args(["spread"].map(OsString::from)).unwrap_err();
        assert!(error.to_string().contains("Usage"));
    }
}
