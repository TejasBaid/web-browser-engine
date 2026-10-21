//! Styling: stylesheets, the user-agent sheet, the cascade and computed values.

pub mod cascade;
pub mod computed;
pub mod grid;
pub mod hints;
pub mod properties;
pub mod stylesheet;

pub use cascade::{StyledDocument, Viewport, compute_styles};
pub use computed::ComputedStyle;
pub use stylesheet::{Origin, Stylesheet};

/// The built-in user-agent stylesheet source.
pub const UA_CSS: &str = include_str!("ua.css");

/// Extra user-agent rules applied to quirks-mode documents.
pub const UA_QUIRKS_CSS: &str = include_str!("ua-quirks.css");

thread_local! {
    static UA_QUIRKS_SHEET: std::rc::Rc<Stylesheet> =
        std::rc::Rc::new(Stylesheet::parse(UA_QUIRKS_CSS, Origin::UserAgent, None));
    static UA_SHEET: std::rc::Rc<Stylesheet> = std::rc::Rc::new(Stylesheet::parse(UA_CSS, Origin::UserAgent, None));
}

/// The parsed user-agent stylesheet (parsed once per thread).
pub fn user_agent_stylesheet() -> std::rc::Rc<Stylesheet> {
    UA_SHEET.with(std::rc::Rc::clone)
}

/// The quirks-mode additions to the user-agent stylesheet.
pub fn quirks_stylesheet() -> std::rc::Rc<Stylesheet> {
    UA_QUIRKS_SHEET.with(std::rc::Rc::clone)
}
