//! utop — a lightweight htop clone. Thin binary entry point.

fn main() {
    let config = utop::cli::parse();
    if let Err(err) = utop::run(&config) {
        eprintln!("utop: {err}");
        std::process::exit(1);
    }
}
