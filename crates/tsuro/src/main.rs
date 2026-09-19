use std::path::PathBuf;

use tsuro::{boot, Session};

fn main() -> iced::Result {
    let session = match std::env::args().nth(1) {
        Some(path) => Session::open_path(PathBuf::from(path)),
        None => Session::empty(),
    };
    iced::application("TsuroPDF", Session::update, Session::view)
        .subscription(Session::subscription)
        .run_with(move || boot(session))
}
