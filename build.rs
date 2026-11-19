//! The web UI is built separately (`npm run build` in `web/`) and embedded from `web/dist`.
//! So that `cargo build` and `cargo test` work without Node, a placeholder page is put there
//! when no UI has been built yet.
use std::{fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    let index = Path::new("web/dist/index.html");
    if !index.exists() {
        fs::create_dir_all("web/dist").unwrap();
        fs::write(
            index,
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>agent</title></head>\
             <body><p>The web UI is not built. Run <code>npm install &amp;&amp; npm run build</code> \
             in <code>web/</code>, then rebuild the agent.</p></body></html>",
        )
        .unwrap();
    }
}
