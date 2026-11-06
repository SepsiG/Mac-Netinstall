//! `app/src/lib/codes.json` (read by the UI's translation tests) must be
//! the current code catalog. Regenerate it with
//! `UPDATE_CODES=1 cargo test -p mac-netinstall --test codes`.

use mac_netinstall_app::codes::catalog_json;
use std::path::Path;

#[test]
fn the_ui_code_list_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/codes.json");
    let want = catalog_json();
    if std::env::var_os("UPDATE_CODES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is out of date: run UPDATE_CODES=1 cargo test -p mac-netinstall --test codes",
        path.display()
    );
}
