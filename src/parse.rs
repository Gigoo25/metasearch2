//! Helper functions for parsing search engine responses.

use crate::{
    engines::{EngineFeaturedSnippet, EngineResponse, EngineSearchResult},
    urls::normalize_url,
};

use scraper::{Html, Selector};
use tracing::trace;

pub struct ParseOpts {
    result: Option<Selector>,
    title: QueryMethod,
    href: QueryMethod,
    description: QueryMethod,

    featured_snippet: Option<Selector>,
    featured_snippet_title: QueryMethod,
    featured_snippet_href: QueryMethod,
    featured_snippet_description: QueryMethod,
}

impl Default for ParseOpts {
    fn default() -> Self {
        Self {
            result: None,
            title: QueryMethod::default(),
            href: QueryMethod::default(),
            description: QueryMethod::default(),
            featured_snippet: None,
            featured_snippet_title: QueryMethod::default(),
            featured_snippet_href: QueryMethod::default(),
            featured_snippet_description: QueryMethod::default(),
        }
    }
}

impl ParseOpts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn result(mut self, result: Selector) -> Self {
        self.result = Some(result);
        self
    }

    #[must_use]
    pub fn title(mut self, title: impl Into<QueryMethod>) -> Self {
        self.title = title.into();
        self
    }

    #[must_use]
    pub fn href(mut self, href: impl Into<QueryMethod>) -> Self {
        self.href = href.into();
        self
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<QueryMethod>) -> Self {
        self.description = description.into();
        self
    }

    #[must_use]
    pub fn featured_snippet(mut self, featured_snippet: Selector) -> Self {
        self.featured_snippet = Some(featured_snippet);
        self
    }

    #[must_use]
    pub fn featured_snippet_title(
        mut self,
        featured_snippet_title: impl Into<QueryMethod>,
    ) -> Self {
        self.featured_snippet_title = featured_snippet_title.into();
        self
    }

    #[must_use]
    pub fn featured_snippet_href(mut self, featured_snippet_href: impl Into<QueryMethod>) -> Self {
        self.featured_snippet_href = featured_snippet_href.into();
        self
    }

    #[must_use]
    pub fn featured_snippet_description(
        mut self,
        featured_snippet_description: impl Into<QueryMethod>,
    ) -> Self {
        self.featured_snippet_description = featured_snippet_description.into();
        self
    }
}

type ManualQueryMethod = Box<dyn Fn(&scraper::ElementRef) -> eyre::Result<String>>;

#[derive(Default)]
pub enum QueryMethod {
    #[default]
    None,
    CssSelector(Selector),
    Manual(ManualQueryMethod),
}

impl From<&'static str> for QueryMethod {
    fn from(s: &'static str) -> Self {
        QueryMethod::CssSelector(Selector::parse(s).unwrap())
    }
}

impl From<Selector> for QueryMethod {
    fn from(s: Selector) -> Self {
        QueryMethod::CssSelector(s)
    }
}

impl QueryMethod {
    pub fn call_with_css_selector_override(
        &self,
        el: &scraper::ElementRef,
        with_css_selector: impl Fn(&scraper::ElementRef, &Selector) -> Option<String>,
    ) -> eyre::Result<String> {
        match self {
            QueryMethod::None => Ok(String::new()),
            QueryMethod::CssSelector(ref sel) => Ok(with_css_selector(el, sel).unwrap_or_default()),
            QueryMethod::Manual(f) => f(el),
        }
    }

    pub fn call(&self, el: &scraper::ElementRef) -> eyre::Result<String> {
        self.call_with_css_selector_override(el, |el, sel| {
            el.select(sel).next().map(|n| n.text().collect::<String>())
        })
    }
}

pub(super) fn parse_html_response_with_opts(
    body: &str,
    opts: ParseOpts,
) -> eyre::Result<EngineResponse> {
    let dom = Html::parse_document(body);

    let mut search_results = Vec::new();

    let ParseOpts {
        result,
        title: title_query_method,
        href: href_query_method,
        description: description_query_method,
        featured_snippet: featured_snippet_query,
        featured_snippet_title: featured_snippet_title_query_method,
        featured_snippet_href: featured_snippet_href_query_method,
        featured_snippet_description: featured_snippet_description_query_method,
    } = opts;

    let result = result.as_ref().expect("result selector must be set");
    let results = dom.select(result);

    for result in results {
        let title = title_query_method.call(&result)?;
        let url = href_query_method.call_with_css_selector_override(&result, |el, sel| {
            el.select(sel).next().map(|n| {
                n.value()
                    .attr("href")
                    .map_or_else(|| n.text().collect::<String>(), str::to_string)
            })
        })?;
        let description = description_query_method.call(&result)?;
        trace!("url: {url}, title: {title}, description: {description}");
        trace!("result: {:?}", result.value().classes().collect::<Vec<_>>());

        // this can happen on google if you search "roll d6"
        let is_empty = description.is_empty() && title.is_empty();
        if is_empty {
            trace!("empty content for {url} ({title}), skipping");
            continue;
        }

        // this can happen on google if it gives you a featured snippet
        if description.is_empty() {
            trace!("empty description for {url} ({title}), skipping");
            continue;
        }

        let url = normalize_url(&url);

        search_results.push(EngineSearchResult {
            url,
            title,
            description,
        });
    }

    let featured_snippet = if let Some(ref featured_snippet_sel) = featured_snippet_query {
        if let Some(featured_snippet) = dom.select(featured_snippet_sel).next() {
            let title = featured_snippet_title_query_method.call(&featured_snippet)?;
            let url = featured_snippet_href_query_method.call(&featured_snippet)?;
            let url = normalize_url(&url);
            let description = featured_snippet_description_query_method.call(&featured_snippet)?;

            // this can happen on google if you search "what's my user agent"
            let is_empty = description.is_empty() && title.is_empty();
            if is_empty {
                None
            } else {
                Some(EngineFeaturedSnippet {
                    url,
                    title,
                    description,
                })
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(EngineResponse {
        search_results,
        featured_snippet,
        // these fields are used by instant answers, not normal search engines
        answer_html: None,
        infobox_html: None,
    })
}
