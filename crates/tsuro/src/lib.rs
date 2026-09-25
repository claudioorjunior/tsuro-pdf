pub mod browse;
pub mod kiri;
pub mod page;
pub mod positions;
pub mod prefs;
pub mod search;
pub mod session;
pub mod view;

pub(crate) mod engine;
pub(crate) mod print;
pub(crate) mod spool;

pub use page::{Bitmap, MediaBox, Outline, OutlineItem, PageNo, Quad, Scale, TextLayer, Viewport};
pub use session::{Message, OpenSource, Session};

pub fn boot(session: Session) -> (Session, iced::Task<Message>) {
    session.boot()
}
