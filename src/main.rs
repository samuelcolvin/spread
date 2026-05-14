use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, LazyLock},
};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use gpui::{
    App, Application, AsyncApp, Bounds, Context, Div, FontWeight, Image, ImageFormat, IntoElement,
    KeyBinding, Menu, MenuItem, MouseButton, PathPromptOptions, Render, SystemMenuType,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, img, point, prelude::*, px,
    rgb, size,
};

use crate::{
    view::{CopySelection, SpreadsheetViewer, WINDOW_HEIGHT, WINDOW_WIDTH},
    workbook::load_workbook,
};

mod csv_source;
mod parquet_source;
mod view;
mod workbook;

actions!(spread_app, [OpenDocument, CloseFile, QuitSpread]);

const APP_NAME: &str = "Spread";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const SPLASH_WIDTH: f32 = 520.0;
const SPLASH_HEIGHT: f32 = 360.0;
const SPLASH_BG: u32 = 0xf8_f9_fa;
const SPLASH_TEXT: u32 = 0x20_21_24;
const SPLASH_MUTED_TEXT: u32 = 0x5f_63_68;
const SPLASH_ERROR_TEXT: u32 = 0xb3_26_1e;
const SPLASH_BUTTON_BG: u32 = 0x1a_73_e8;
const SPLASH_BUTTON_HOVER_BG: u32 = 0x18_64_c9;
const SPLASH_BUTTON_TEXT: u32 = 0xff_ff_ff;
#[cfg(target_os = "macos")]
const APP_ICON_BYTES: &[u8] = include_bytes!("../packaging/macos/Spread.icns");
static SPREAD_LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../packaging/macos/Spread.png").to_vec(),
    ))
});

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(&cli) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<()> {
    validate_output_mode(cli)?;

    if cli.list_sheets {
        let workbook = Arc::new(load_workbook(required_path(cli)?)?);
        print_sheet_list(&workbook);
        return Ok(());
    }

    match cli.display {
        DisplayMode::Gui => {}
        DisplayMode::Json => {
            let workbook = Arc::new(load_workbook(required_path(cli)?)?);
            let sheet_ix = resolve_sheet(&workbook, cli.sheet.as_deref())?;
            serde_json::to_writer_pretty(std::io::stdout(), &workbook.sheet(sheet_ix).inspect())?;
            println!();
            return Ok(());
        }
        DisplayMode::Xml => {
            let workbook = Arc::new(load_workbook(required_path(cli)?)?);
            let sheet_ix = resolve_sheet(&workbook, cli.sheet.as_deref())?;
            print!("{}", workbook.sheet(sheet_ix).to_pretty_xml());
            println!();
            return Ok(());
        }
        DisplayMode::Table => {
            let workbook = Arc::new(load_workbook(required_path(cli)?)?);
            let sheet_ix = resolve_sheet(&workbook, cli.sheet.as_deref())?;
            print_terminal_table(workbook.sheet(sheet_ix));
            return Ok(());
        }
        DisplayMode::Audit => {
            let workbook = Arc::new(load_workbook(required_path(cli)?)?);
            let sheet_ix = resolve_sheet(&workbook, cli.sheet.as_deref())?;
            let audits = workbook.formula_audits(cli.sheet.as_ref().map(|_| sheet_ix))?;
            print_formula_audits(&audits);
            let exit_code = formula_audit_exit_code(&audits);
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            return Ok(());
        }
    }

    let initial_path = cli.path.clone();
    let initial_sheet = cli.sheet.clone();
    let async_app: Rc<RefCell<Option<AsyncApp>>> = Rc::new(RefCell::new(None));
    let pending_urls = Rc::new(RefCell::new(Vec::new()));
    let splash_error = Rc::new(RefCell::new(None));
    // Set by a document window just before it closes itself via File > Close File.
    // The app shell consumes it in on_window_closed so opening the splash happens
    // after GPUI has actually removed the document window from its window list.
    let show_splash_after_document_close = Rc::new(Cell::new(false));
    let open_urls_app = Rc::clone(&async_app);
    let open_urls_pending = Rc::clone(&pending_urls);
    let open_urls_show_splash_after_document_close = Rc::clone(&show_splash_after_document_close);
    let open_urls_splash_error = Rc::clone(&splash_error);
    let application = Application::new();

    application.on_open_urls(move |urls| {
        let Some(async_app) = open_urls_app.borrow().as_ref().cloned() else {
            open_urls_pending.borrow_mut().extend(urls);
            return;
        };
        let executor = async_app.foreground_executor().clone();
        let show_splash_after_document_close =
            Rc::clone(&open_urls_show_splash_after_document_close);
        let splash_error = Rc::clone(&open_urls_splash_error);
        executor
            .spawn(async move {
                if let Err(error) = async_app.update(|cx| {
                    open_urls(urls, &show_splash_after_document_close, &splash_error, cx)
                }) {
                    eprintln!("{error:#}");
                }
            })
            .detach();
    });

    application.run(move |cx: &mut App| {
        set_app_icon();
        *async_app.borrow_mut() = Some(cx.to_async());
        let pending_urls = pending_urls.borrow_mut().drain(..).collect::<Vec<_>>();
        let opened_pending_urls = if pending_urls.is_empty() {
            0
        } else {
            open_urls(
                pending_urls,
                &show_splash_after_document_close,
                &splash_error,
                cx,
            )
        };
        let should_open_splash = initial_path.is_none() && opened_pending_urls == 0;

        if should_open_splash
            && let Err(error) = open_splash_window(
                cx,
                Rc::clone(&show_splash_after_document_close),
                Rc::clone(&splash_error),
            )
        {
            eprintln!("{error:#}");
            cx.quit();
            return;
        }

        cx.bind_keys([KeyBinding::new(
            "cmd-c",
            CopySelection,
            Some("SpreadsheetViewer"),
        )]);
        cx.bind_keys([KeyBinding::new("cmd-o", OpenDocument, None)]);
        cx.bind_keys([KeyBinding::new("cmd-q", CloseFile, None)]);
        let open_dialog_show_splash_after_document_close =
            Rc::clone(&show_splash_after_document_close);
        let open_dialog_splash_error = Rc::clone(&splash_error);
        cx.on_action(move |_: &OpenDocument, cx| {
            open_document_from_dialog(
                Rc::clone(&open_dialog_show_splash_after_document_close),
                Rc::clone(&open_dialog_splash_error),
                cx,
            );
        });
        cx.on_action(quit_spread);
        cx.set_menus(vec![
            Menu {
                name: "Spread".into(),
                items: vec![
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit Spread", QuitSpread),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Open...", OpenDocument),
                    MenuItem::separator(),
                    MenuItem::action("Close File", CloseFile),
                ],
            },
        ]);

        let closed_window_show_splash_after_document_close =
            Rc::clone(&show_splash_after_document_close);
        cx.on_window_closed(move |cx| {
            if closed_window_show_splash_after_document_close.replace(false) {
                // Close File removes the document window first. Only after GPUI has
                // reported that close do we create the splash window, otherwise the
                // app can briefly have zero windows and quit or leave stale windows.
                if document_window_count(cx) == 0
                    && !has_splash_window(cx)
                    && let Err(error) = open_splash_window(
                        cx,
                        Rc::clone(&closed_window_show_splash_after_document_close),
                        Rc::clone(&splash_error),
                    )
                {
                    eprintln!("{error:#}");
                }
            } else if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        if let Some(path) = initial_path.as_ref()
            && let Err(error) = open_workbook_window(
                path,
                initial_sheet.as_deref(),
                Rc::clone(&show_splash_after_document_close),
                cx,
            )
        {
            eprintln!("{error:#}");
            cx.quit();
        }

        cx.activate(true);
    });

    Ok(())
}

#[cfg(target_os = "macos")]
fn set_app_icon() {
    use cocoa::{
        appkit::{NSApp, NSApplication, NSImage},
        base::{id, nil},
        foundation::NSData,
    };
    use std::ffi::c_void;

    // GPUI also supports launching the CLI binary directly, so set the Dock icon
    // from embedded data instead of relying only on the app bundle's Info.plist.
    unsafe {
        let data = NSData::dataWithBytes_length_(
            nil,
            APP_ICON_BYTES.as_ptr().cast::<c_void>(),
            APP_ICON_BYTES.len() as _,
        );
        let image = cocoa::appkit::NSImage::initWithData_(NSImage::alloc(nil), data);
        if image != nil {
            NSApp().setApplicationIconImage_(image as id);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn set_app_icon() {}

fn open_document_from_dialog(
    show_splash_after_document_close: Rc<Cell<bool>>,
    splash_error: Rc<RefCell<Option<String>>>,
    cx: &mut App,
) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Open file".into()),
    });

    cx.spawn(async move |cx| match paths.await {
        Ok(Ok(Some(paths))) => {
            if let Err(error) = cx.update(|cx| {
                open_paths(paths, &show_splash_after_document_close, &splash_error, cx);
            }) {
                eprintln!("{error:#}");
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => eprintln!("{error:#}"),
        Err(error) => eprintln!("failed to receive selected file paths: {error}"),
    })
    .detach();
}

fn quit_spread(_: &QuitSpread, cx: &mut App) {
    cx.quit();
}

fn open_paths(
    paths: Vec<PathBuf>,
    show_splash_after_document_close: &Rc<Cell<bool>>,
    splash_error: &Rc<RefCell<Option<String>>>,
    cx: &mut App,
) -> usize {
    let mut opened = 0;
    for path in paths {
        if let Err(error) =
            open_workbook_window(&path, None, Rc::clone(show_splash_after_document_close), cx)
        {
            eprintln!("{error:#}");
            *splash_error.borrow_mut() = Some(error.to_string());
            notify_splash_windows(cx);
        } else {
            opened += 1;
        }
    }
    if opened > 0 {
        splash_error.borrow_mut().take();
        close_splash_windows(cx);
    }
    opened
}

fn open_urls(
    urls: Vec<String>,
    show_splash_after_document_close: &Rc<Cell<bool>>,
    splash_error: &Rc<RefCell<Option<String>>>,
    cx: &mut App,
) -> usize {
    open_paths(
        urls.into_iter()
            .filter_map(|url| {
                let path = file_url_to_path(&url);
                if path.is_none() {
                    eprintln!("unsupported URL from platform: {url}");
                }
                path
            })
            .collect(),
        show_splash_after_document_close,
        splash_error,
        cx,
    )
}

fn open_workbook_window(
    path: &Path,
    sheet: Option<&str>,
    show_splash_after_document_close: Rc<Cell<bool>>,
    cx: &mut App,
) -> Result<()> {
    let workbook = Arc::new(load_workbook(path)?);
    let sheet_ix = resolve_sheet(&workbook, sheet)?;
    let title = format!("spread - {}", workbook.display_name());
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);

    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.0), px(13.0))),
            }),
            ..Default::default()
        },
        {
            let workbook = Arc::clone(&workbook);
            move |window, cx| {
                cx.new(|cx| {
                    SpreadsheetViewer::new(
                        Arc::clone(&workbook),
                        sheet_ix,
                        Rc::clone(&show_splash_after_document_close),
                        window,
                        cx,
                    )
                })
            }
        },
    )
    .with_context(|| format!("failed to open window for {title}"))?;

    cx.activate(true);
    Ok(())
}

fn open_splash_window(
    cx: &mut App,
    show_splash_after_document_close: Rc<Cell<bool>>,
    splash_error: Rc<RefCell<Option<String>>>,
) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(SPLASH_WIDTH), px(SPLASH_HEIGHT)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_NAME.into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.0), px(13.0))),
            }),
            ..Default::default()
        },
        |_, cx| {
            cx.new(|_| SplashScreen {
                show_splash_after_document_close,
                error: splash_error,
            })
        },
    )
    .context("failed to open splash window")?;

    cx.activate(true);
    Ok(())
}

fn close_splash_windows(cx: &mut App) {
    for window_handle in cx.windows() {
        if window_handle.downcast::<SplashScreen>().is_none() {
            continue;
        }
        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
    }
}

fn notify_splash_windows(cx: &mut App) {
    for window_handle in cx.windows() {
        if let Some(splash_window) = window_handle.downcast::<SplashScreen>() {
            let _ = splash_window.update(cx, |_, _, cx| cx.notify());
        }
    }
}

fn document_window_count(cx: &App) -> usize {
    cx.windows()
        .into_iter()
        .filter(|window| window.downcast::<SpreadsheetViewer>().is_some())
        .count()
}

fn has_splash_window(cx: &App) -> bool {
    cx.windows()
        .into_iter()
        .any(|window| window.downcast::<SplashScreen>().is_some())
}

struct SplashScreen {
    show_splash_after_document_close: Rc<Cell<bool>>,
    error: Rc<RefCell<Option<String>>>,
}

impl Render for SplashScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let show_splash_after_document_close = Rc::clone(&self.show_splash_after_document_close);
        let error = self.error.borrow().clone();

        div()
            .id("splash-screen")
            .size_full()
            .on_action(cx.listener(
                |view: &mut SplashScreen, _: &OpenDocument, _: &mut Window, cx| {
                    open_document_from_dialog(
                        Rc::clone(&view.show_splash_after_document_close),
                        Rc::clone(&view.error),
                        cx,
                    );
                },
            ))
            .bg(rgb(SPLASH_BG))
            .text_color(rgb(SPLASH_TEXT))
            .font_family("Arial")
            .flex()
            .flex_col()
            .child(splash_title_bar())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .px(px(32.0))
                    .pb(px(32.0))
                    .child(
                        img(Arc::clone(&SPREAD_LOGO))
                            .w(px(96.0))
                            .h(px(96.0))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .text_size(px(34.0))
                            .font_weight(FontWeight::BOLD)
                            .child(APP_NAME),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(rgb(SPLASH_MUTED_TEXT))
                            .child(format!("Version {APP_VERSION}")),
                    )
                    .child(open_button(
                        show_splash_after_document_close,
                        Rc::clone(&self.error),
                    ))
                    .child(error_line(error)),
            )
    }
}

fn splash_title_bar() -> Div {
    div()
        .h(px(44.0))
        .w_full()
        .flex_none()
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
}

fn open_button(
    show_splash_after_document_close: Rc<Cell<bool>>,
    splash_error: Rc<RefCell<Option<String>>>,
) -> Div {
    div()
        .mt(px(12.0))
        .px(px(20.0))
        .h(px(34.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(SPLASH_BUTTON_BG))
        .text_color(rgb(SPLASH_BUTTON_TEXT))
        .text_size(px(13.0))
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .hover(|button| button.bg(rgb(SPLASH_BUTTON_HOVER_BG)))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            open_document_from_dialog(
                Rc::clone(&show_splash_after_document_close),
                Rc::clone(&splash_error),
                cx,
            );
        })
        .child("Open file")
}

fn error_line(error: Option<String>) -> Div {
    div()
        .h(px(18.0))
        .max_w(px(420.0))
        .text_size(px(12.0))
        .text_color(rgb(SPLASH_ERROR_TEXT))
        .child(error.unwrap_or_default())
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

    /// Spreadsheet file to open.
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum DisplayMode {
    #[default]
    Gui,
    Json,
    Xml,
    Table,
    Audit,
}

fn validate_output_mode(cli: &Cli) -> Result<()> {
    let output_modes = usize::from(cli.list_sheets) + usize::from(cli.display != DisplayMode::Gui);

    if output_modes > 1 {
        bail!("choose only one output mode: --list-sheets or --display");
    }

    if cli.path.is_none()
        && (cli.display != DisplayMode::Gui || cli.list_sheets || cli.sheet.is_some())
    {
        bail!("missing spreadsheet file path");
    }

    Ok(())
}

fn required_path(cli: &Cli) -> Result<&Path> {
    cli.path
        .as_deref()
        .ok_or_else(|| anyhow!("missing spreadsheet file path"))
}

fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let path = url
        .strip_prefix("file://localhost/")
        .or_else(|| url.strip_prefix("file:///"))?;
    Some(PathBuf::from(format!("/{}", percent_decode(path)?)))
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut ix = 0;

    while ix < bytes.len() {
        if bytes[ix] == b'%' {
            let hi = *bytes.get(ix + 1)?;
            let lo = *bytes.get(ix + 2)?;
            output.push((hex_value(hi)? << 4) | hex_value(lo)?);
            ix += 3;
        } else {
            output.push(bytes[ix]);
            ix += 1;
        }
    }

    String::from_utf8(output).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
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
        assert_eq!(cli.path.as_deref(), Some(Path::new("book.csv")));
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
        assert_eq!(cli.path.as_deref(), Some(Path::new("book.xlsx")));
        assert_eq!(cli.sheet.as_deref(), Some("Summary"));
        assert_eq!(cli.display, DisplayMode::Xml);
    }

    #[test]
    fn parses_display_table_output_mode() {
        let cli = parse_args(["spread", "--display", "table", "book.xlsx"]).unwrap();
        assert_eq!(cli.display, DisplayMode::Table);
    }

    #[test]
    fn parses_display_json_output_mode() {
        let cli = parse_args(["spread", "--display", "json", "book.xlsx"]).unwrap();
        assert_eq!(cli.display, DisplayMode::Json);
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

    #[test]
    fn rejects_old_debug_flag() {
        let error = parse_args(["spread", "--debug", "book.xlsx"]).unwrap_err();
        assert!(error.to_string().contains("unexpected argument"));
    }

    #[test]
    fn allows_gui_without_path() {
        let cli = parse_args(["spread"]).unwrap();
        validate_output_mode(&cli).unwrap();
        assert_eq!(cli.display, DisplayMode::Gui);
        assert_eq!(cli.path, None);
    }

    #[test]
    fn rejects_missing_cli_path_for_output_modes() {
        let cli = parse_args(["spread", "--display", "table"]).unwrap();
        let error = validate_output_mode(&cli).unwrap_err();
        assert!(error.to_string().contains("missing spreadsheet file path"));
    }

    #[test]
    fn rejects_missing_cli_path_with_sheet() {
        let cli = parse_args(["spread", "--sheet", "Summary"]).unwrap();
        let error = validate_output_mode(&cli).unwrap_err();
        assert!(error.to_string().contains("missing spreadsheet file path"));
    }

    #[test]
    fn converts_file_urls_to_paths() {
        assert_eq!(
            file_url_to_path("file:///Users/samuel/My%20File.xlsx").as_deref(),
            Some(Path::new("/Users/samuel/My File.xlsx"))
        );
        assert_eq!(
            file_url_to_path("file://localhost/Users/samuel/book.csv").as_deref(),
            Some(Path::new("/Users/samuel/book.csv"))
        );
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
