//! HTML parsing: the WHATWG tokenizer and tree builder.
//!
//! ```
//! let doc = browser_engine::html::parse_document("<p>Hello <b>world</b>");
//! assert_eq!(doc.text_content(doc.body().unwrap()), "Hello world");
//! ```

pub mod entities;
pub mod tokenizer;
pub mod tree_builder;

pub use tokenizer::{Doctype, State, Tag, Token, Tokenizer};
pub use tree_builder::{ParseOptions, parse_document, parse_document_with, parse_fragment};
