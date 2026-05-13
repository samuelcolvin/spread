use std::{env, fs, io, path::Path};

const INFO_PLIST: &str = "packaging/macos/Info.plist";
const VERSION_KEYS: &[&str] = &["CFBundleVersion", "CFBundleShortVersionString"];

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed={INFO_PLIST}");

    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set by Cargo");
    update_info_plist(Path::new(INFO_PLIST), &version)
}

fn update_info_plist(path: &Path, version: &str) -> io::Result<()> {
    let plist = fs::read_to_string(path)?;
    let updated = update_plist_versions(&plist, version);

    if updated != plist {
        fs::write(path, updated)?;
    }

    Ok(())
}

fn update_plist_versions(plist: &str, version: &str) -> String {
    let mut updated = plist.to_owned();

    for key in VERSION_KEYS {
        let marker = format!("<key>{key}</key>");
        let Some(marker_start) = updated.find(&marker) else {
            continue;
        };
        let after_marker = marker_start + marker.len();
        let Some(string_start_offset) = updated[after_marker..].find("<string>") else {
            continue;
        };
        let value_start = after_marker + string_start_offset + "<string>".len();
        let Some(string_end_offset) = updated[value_start..].find("</string>") else {
            continue;
        };
        let value_end = value_start + string_end_offset;

        updated.replace_range(value_start..value_end, version);
    }

    updated
}
