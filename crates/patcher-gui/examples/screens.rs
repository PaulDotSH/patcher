//! Renders every screen of a theme to PNG: `cargo run -p patcher-gui --example screens -- OUT_DIR [THEME.toml]`.

fn main() {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().expect("OUT_DIR"));
    let theme = match args.next() {
        Some(t) => patcher_gui::Theme::load(std::path::Path::new(&t)).unwrap_or_else(|e| panic!("{e}")),
        None => patcher_gui::Theme::builtin(),
    };
    let work = std::env::temp_dir().join("patcher-gui-screens");
    for p in patcher_gui::snapshot::render_all(&theme, &patcher_gui::snapshot::demo_meta(), &work, &out).unwrap() {
        println!("{}", p.display());
    }
}
