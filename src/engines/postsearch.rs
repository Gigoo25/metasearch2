//! These search engines are requested after we've built the main search
//! results. They can only show stuff in infoboxes and don't get requested if
//! an infobox was added by another earlier engine.

/// How many top results are scanned for a postsearch trigger. Higher means
/// infoboxes are found more often, at the cost of matching less relevant
/// results further down the page.
pub const RESULT_WINDOW: usize = 30;

pub mod docs_rs;
pub mod github;
pub mod mdn;
pub mod minecraft_wiki;
pub mod stackexchange;
