//! Safe search levels, engine parameters and the local blocklist.
//!
//! Engine-side parameters catch most explicit results, but they don't cover
//! every engine (the local index and kiwix have no safe-search parameter) and
//! some engines ignore or partially apply them. The lists below are a local
//! backstop applied to every result after merging.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SafeSearch {
    #[default]
    Off,
    Moderate,
    Strict,
}

impl SafeSearch {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Moderate => "moderate",
            Self::Strict => "strict",
        }
    }

    /// The value of bing's `adlt` parameter.
    #[must_use]
    pub fn bing(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Moderate => "demote",
            Self::Strict => "strict",
        }
    }

    /// The value of brave's `safesearch` parameter.
    #[must_use]
    pub fn brave(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Moderate => "moderate",
            Self::Strict => "strict",
        }
    }

    /// The value of duckduckgo's `kp` parameter.
    #[must_use]
    pub fn duckduckgo(self) -> &'static str {
        match self {
            Self::Off => "-2",
            Self::Moderate => "-1",
            Self::Strict => "1",
        }
    }
}

/// Adult site domains, blocked in moderate and strict mode. Subdomains are
/// blocked too, so `www.pornhub.com` matches `pornhub.com`.
const ADULT_HOSTS: &[&str] = &[
    "brazzers.com",
    "cam4.com",
    "chaturbate.com",
    "e-hentai.org",
    "hanime.tv",
    "literotica.com",
    "livejasmin.com",
    "nhentai.net",
    "onlyfans.com",
    "pornhub.com",
    "redtube.com",
    "rule34.xxx",
    "spankbang.com",
    "stripchat.com",
    "xhamster.com",
    "xnxx.com",
    "xvideos.com",
    "youporn.com",
];

/// Domains only blocked in strict mode: mixed-content sites that carry a
/// significant amount of adult material.
const STRICT_HOSTS: &[&str] = &[
    "adultfriendfinder.com",
    "bongacams.com",
    "camwhores.tv",
    "erome.com",
    "fansly.com",
    "hentaihaven.xxx",
    "motherless.com",
    "nudevista.com",
    "porn.com",
    "sex.com",
];

/// Unambiguous explicit terms, blocked in moderate and strict mode.
static MODERATE_KEYWORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(porn|porno|pornographic|pornstar|xxx|nsfw|hentai|onlyfans|chaturbate|camgirl|camgirls|blowjob|handjob|creampie|cumshot|gangbang|deepthroat|doggystyle|milf|squirting|bdsm|sex cam|sex cams|sex video|sex videos|nude pics|nude photos)\b",
    )
    .unwrap()
});

/// Terms that are explicit but also have innocent uses, so they are only
/// blocked in strict mode.
static STRICT_KEYWORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(nude|nudity|naked|erotic|erotica|anal|bondage|masturbation|orgasm|orgasms|threesome|hardcore|escort|escorts|fetish|fetishes|adult dating|adult video|adult videos|sex chat|live sex|cam sex)\b",
    )
    .unwrap()
});

/// Whether a result should be dropped for the given safe search level. The
/// url, title and description are all checked; `url` may be relative (kiwix),
/// in which case only the keyword lists apply.
#[must_use]
pub fn blocked(level: SafeSearch, url: &str, title: &str, description: &str) -> bool {
    if level == SafeSearch::Off {
        return false;
    }

    if let Some(host) = Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
    {
        if host_matches(&host, ADULT_HOSTS)
            || (level == SafeSearch::Strict && host_matches(&host, STRICT_HOSTS))
        {
            return true;
        }
    }

    let text = format!("{url} {title} {description}");
    if MODERATE_KEYWORDS.is_match(&text) {
        return true;
    }

    level == SafeSearch::Strict && STRICT_KEYWORDS.is_match(&text)
}

fn host_matches(host: &str, blocked_hosts: &[&str]) -> bool {
    blocked_hosts
        .iter()
        .any(|blocked| host == *blocked || host.ends_with(&format!(".{blocked}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_never_blocks() {
        assert!(!blocked(
            SafeSearch::Off,
            "https://www.pornhub.com/view",
            "Porn",
            "explicit porn video"
        ));
    }

    #[test]
    fn moderate_blocks_adult_hosts_and_keywords() {
        assert!(blocked(
            SafeSearch::Moderate,
            "https://www.pornhub.com/view",
            "",
            ""
        ));
        assert!(blocked(
            SafeSearch::Moderate,
            "https://example.com/a",
            "some hentai collection",
            ""
        ));
        // subdomains of the strict list are not blocked in moderate mode
        assert!(!blocked(SafeSearch::Moderate, "https://sex.com/", "", ""));
    }

    #[test]
    fn strict_blocks_ambiguous_terms_too() {
        assert!(blocked(SafeSearch::Strict, "https://sex.com/", "", ""));
        assert!(blocked(
            SafeSearch::Strict,
            "https://example.com/a",
            "nude photography",
            ""
        ));
        assert!(!blocked(
            SafeSearch::Moderate,
            "https://example.com/a",
            "nude photography",
            ""
        ));
    }

    #[test]
    fn innocent_words_are_not_blocked() {
        for (title, description) in [
            ("Sussex county council", "sexism in the workplace"),
            ("Torvalds' analysis of the market", "analog computing"),
            ("Essex, England", "a historic county"),
        ] {
            assert!(
                !blocked(
                    SafeSearch::Moderate,
                    "https://example.com/a",
                    title,
                    description
                ),
                "{title} should not be blocked"
            );
            assert!(
                !blocked(
                    SafeSearch::Strict,
                    "https://example.com/a",
                    title,
                    description
                ),
                "{title} should not be blocked in strict mode"
            );
        }
    }

    #[test]
    fn engine_parameter_values() {
        assert_eq!(SafeSearch::Off.bing(), "off");
        assert_eq!(SafeSearch::Moderate.bing(), "demote");
        assert_eq!(SafeSearch::Strict.bing(), "strict");
        assert_eq!(SafeSearch::Off.brave(), "off");
        assert_eq!(SafeSearch::Moderate.brave(), "moderate");
        assert_eq!(SafeSearch::Strict.brave(), "strict");
        assert_eq!(SafeSearch::Off.duckduckgo(), "-2");
        assert_eq!(SafeSearch::Moderate.duckduckgo(), "-1");
        assert_eq!(SafeSearch::Strict.duckduckgo(), "1");
    }
}
