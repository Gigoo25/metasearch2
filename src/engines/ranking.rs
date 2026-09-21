use std::{collections::HashMap, sync::Arc};

use url::Url;

use crate::{
    config::Config,
    urls::{apply_url_replacements, get_url_weight},
};

use super::{
    Answer, AutocompleteResult, Engine, EngineImageResult, EngineImagesResponse, EngineResponse,
    EngineSearchResult, FeaturedSnippet, ImagesResponse, Infobox, Response, SearchResult,
};

pub fn merge_engine_responses(
    config: Arc<Config>,
    query: &str,
    responses: HashMap<Engine, EngineResponse>,
) -> Response {
    let mut search_results: Vec<SearchResult<EngineSearchResult>> = Vec::new();
    let mut featured_snippet: Option<FeaturedSnippet> = None;
    let mut answer: Option<Answer> = None;
    let mut infobox: Option<Infobox> = None;

    for (engine, response) in responses {
        let engine_config = config.engines.get(engine);

        for (result_index, mut search_result) in response.search_results.into_iter().enumerate() {
            // position 1 has a score of 1, position 2 has a score of 0.5, position 3 has a
            // score of 0.33, etc.
            let base_result_score = 1. / (result_index + 1) as f64;
            let result_score = base_result_score * engine_config.weight;

            // apply url config here
            search_result.url = apply_url_replacements(&search_result.url, &config.urls);
            let url_weight = get_url_weight(&search_result.url, &config.urls);
            if url_weight <= 0. {
                continue;
            }
            let result_score = result_score * url_weight;
            // prefer results that actually mention the query terms
            let result_score = result_score
                * relevance_multiplier(query, &search_result.title, &search_result.description);

            if let Some(existing_result) = search_results
                .iter_mut()
                .find(|r| is_duplicate(&r.result, &search_result))
            {
                // if the weight of this engine is higher than every other one then replace the
                // title and description
                if engine_config.weight
                    > existing_result
                        .engines
                        .iter()
                        .map(|&other_engine| {
                            let other_engine_config = config.engines.get(other_engine);
                            other_engine_config.weight
                        })
                        .max_by(|a, b| a.total_cmp(b))
                        .unwrap_or(0.)
                {
                    existing_result.result.title = search_result.title;
                    existing_result.result.description = search_result.description;
                }

                existing_result.engines.insert(engine);
                existing_result.score += result_score;
            } else {
                search_results.push(SearchResult {
                    result: search_result,
                    engines: [engine].iter().copied().collect(),
                    score: result_score,
                });
            }
        }

        if let Some(mut engine_featured_snippet) = response.featured_snippet {
            // if it has a higher weight than the current featured snippet
            let featured_snippet_weight = featured_snippet.as_ref().map_or(0., |s| {
                let other_engine_config = config.engines.get(s.engine);
                other_engine_config.weight
            });

            // url config applies to featured snippets too
            engine_featured_snippet.url =
                apply_url_replacements(&engine_featured_snippet.url, &config.urls);
            let url_weight = get_url_weight(&engine_featured_snippet.url, &config.urls);
            if url_weight <= 0. {
                continue;
            }
            let featured_snippet_weight = featured_snippet_weight * url_weight;

            if engine_config.weight > featured_snippet_weight {
                featured_snippet = Some(FeaturedSnippet {
                    url: engine_featured_snippet.url,
                    title: engine_featured_snippet.title,
                    description: engine_featured_snippet.description,
                    engine,
                });
            }
        }

        if let Some(engine_answer_html) = response.answer_html {
            // if it has a higher weight than the current answer
            let answer_weight = answer.as_ref().map_or(0., |s| {
                let other_engine_config = config.engines.get(s.engine);
                other_engine_config.weight
            });
            if engine_config.weight > answer_weight {
                answer = Some(Answer {
                    html: engine_answer_html,
                    engine,
                });
            }
        }

        if let Some(engine_infobox_html) = response.infobox_html {
            // if it has a higher weight than the current infobox
            let infobox_weight = infobox.as_ref().map_or(0., |s| {
                let other_engine_config = config.engines.get(s.engine);
                other_engine_config.weight
            });
            if engine_config.weight > infobox_weight {
                infobox = Some(Infobox {
                    html: engine_infobox_html,
                    engine,
                });
            }
        }
    }

    search_results.sort_by(|a, b| b.score.total_cmp(&a.score));
    // don't let one domain fill the result list
    apply_host_diversity(&mut search_results);
    search_results.sort_by(|a, b| b.score.total_cmp(&a.score));

    Response {
        search_results,
        featured_snippet,
        answer,
        infobox,
        config,
    }
}

/// Returns the lowercased host of an absolute url. Relative urls (like local
/// kiwix content) have no host.
fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
}

/// Like [`host_of`], but relative kiwix urls get a synthetic key per book so
/// that duplicate titles inside one ZIM can be merged.
fn dedupe_host_of(url: &str) -> Option<String> {
    if let Some(host) = host_of(url) {
        return Some(host);
    }

    let path = url.strip_prefix('/').unwrap_or(url);
    let path = path.strip_prefix("kiwix/").unwrap_or(path);
    let book = path.strip_prefix("content/")?.split('/').next()?;
    (!book.is_empty()).then(|| format!("kiwix:{book}"))
}

fn is_duplicate(existing: &EngineSearchResult, candidate: &EngineSearchResult) -> bool {
    if existing.url == candidate.url {
        return true;
    }

    let (Some(existing_host), Some(candidate_host)) = (
        dedupe_host_of(&existing.url),
        dedupe_host_of(&candidate.url),
    ) else {
        return false;
    };

    existing_host == candidate_host
        && !existing.title.trim().is_empty()
        && existing
            .title
            .trim()
            .eq_ignore_ascii_case(candidate.title.trim())
}

/// Scores how well a result mentions the query terms. Results that match the
/// whole query in the title rank highest, results that match nothing anywhere
/// are pushed down, multi-word queries where only some words match are
/// weakened, and a title that is exactly the query gets a large boost.
fn relevance_multiplier(query: &str, title: &str, description: &str) -> f64 {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| term.chars().count() >= 2)
        .collect();
    if terms.is_empty() {
        return 1.0;
    }

    let title = title.to_lowercase();
    let title_hits = terms
        .iter()
        .filter(|term| title.contains(term.as_str()))
        .count();

    let haystack = format!("{title} {}", description.to_lowercase());
    let hits = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    if hits == 0 {
        return 0.6;
    }

    let mut multiplier = if title_hits == terms.len() {
        1.3
    } else if title_hits > 0 {
        1.15
    } else {
        1.0
    };

    // matching only some words of a multi-word query is weaker
    if terms.len() >= 2 && hits < terms.len() {
        multiplier *= 0.6 + 0.4 * (hits as f64 / terms.len() as f64);
    }

    // an exact title match is what the user was probably looking for
    let normalize = |value: &str| {
        value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    if normalize(title.as_str()) == normalize(query) {
        multiplier *= 2.5;
    }

    multiplier
}

/// Demotes repeat results from the same host so a single site can't fill the
/// top of the list.
fn apply_host_diversity(results: &mut [SearchResult<EngineSearchResult>]) {
    let mut host_counts: HashMap<String, u32> = HashMap::new();
    for result in results {
        let Some(host) = host_of(&result.result.url) else {
            continue;
        };
        let count = host_counts.entry(host).or_insert(0);
        if *count > 0 {
            result.score *= 0.7_f64.powi(*count as i32);
        }
        *count += 1;
    }
}

pub fn merge_autocomplete_responses(
    config: &Config,
    responses: HashMap<Engine, Vec<String>>,
) -> Vec<String> {
    let mut autocomplete_results: Vec<AutocompleteResult> = Vec::new();

    for (engine, response) in responses {
        let engine_config = config.engines.get(engine);

        for (result_index, autocomplete_result) in response.into_iter().enumerate() {
            // position 1 has a score of 1, position 2 has a score of 0.5, position 3 has a
            // score of 0.33, etc.
            let base_result_score = 1. / (result_index + 1) as f64;
            let result_score = base_result_score * engine_config.weight;

            if let Some(existing_result) = autocomplete_results
                .iter_mut()
                .find(|r| r.query == autocomplete_result)
            {
                existing_result.score += result_score;
            } else {
                autocomplete_results.push(AutocompleteResult {
                    query: autocomplete_result,
                    score: result_score,
                });
            }
        }
    }

    autocomplete_results.sort_by(|a, b| b.score.total_cmp(&a.score));

    autocomplete_results.into_iter().map(|r| r.query).collect()
}

pub fn merge_images_responses(
    config: Arc<Config>,
    responses: HashMap<Engine, EngineImagesResponse>,
) -> ImagesResponse {
    let mut image_results: Vec<SearchResult<EngineImageResult>> = Vec::new();

    for (engine, response) in responses {
        let engine_config = config.engines.get(engine);

        for (result_index, image_result) in response.image_results.into_iter().enumerate() {
            // position 1 has a score of 1, position 2 has a score of 0.5, position 3 has a
            // score of 0.33, etc.
            let base_result_score = 1. / (result_index + 1) as f64;
            let result_score = base_result_score * engine_config.weight;

            if let Some(existing_result) = image_results
                .iter_mut()
                .find(|r| r.result.image_url == image_result.image_url)
            {
                // if the weight of this engine is higher than every other one then replace the
                // title and page url
                if engine_config.weight
                    > existing_result
                        .engines
                        .iter()
                        .map(|&other_engine| {
                            let other_engine_config = config.engines.get(other_engine);
                            other_engine_config.weight
                        })
                        .max_by(|a, b| a.partial_cmp(b).unwrap())
                        .unwrap_or(0.)
                {
                    existing_result.result.title = image_result.title;
                    existing_result.result.page_url = image_result.page_url;
                }

                existing_result.engines.insert(engine);
                existing_result.score += result_score;
            } else {
                image_results.push(SearchResult {
                    result: image_result,
                    engines: [engine].iter().copied().collect(),
                    score: result_score,
                });
            }
        }
    }

    image_results.sort_by(|a, b| b.score.total_cmp(&a.score));

    ImagesResponse {
        image_results,
        config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_result(url: &str, title: &str, description: &str) -> EngineSearchResult {
        EngineSearchResult {
            url: url.to_string(),
            title: title.to_string(),
            description: description.to_string(),
        }
    }

    fn result(url: &str, title: &str, description: &str) -> SearchResult<EngineSearchResult> {
        SearchResult {
            result: engine_result(url, title, description),
            engines: [Engine::Google].into_iter().collect(),
            score: 1.0,
        }
    }

    #[test]
    fn relevance_prefers_title_matches() {
        assert_eq!(
            relevance_multiplier("python threading", "Threading in Python", ""),
            1.3
        );
        let partial = relevance_multiplier(
            "python threading",
            "Welcome to Python.org",
            "the official site",
        );
        assert!((partial - 0.92).abs() < 1e-9, "{partial}");
        assert_eq!(
            relevance_multiplier("python threading", "Unrelated", "nothing here"),
            0.6
        );
        assert_eq!(relevance_multiplier("", "anything", ""), 1.0);
        // single-character terms are ignored
        assert_eq!(relevance_multiplier("a", "anything", ""), 1.0);
        // an exact title match gets a large boost
        assert!((relevance_multiplier("youtube", "YouTube", "") - 1.3 * 2.5).abs() < 1e-9);
        assert!(
            (relevance_multiplier("youtube", "YouTube (YouTube channel)", "") - 1.3).abs() < 1e-9
        );
    }

    #[test]
    fn duplicates_are_same_url_or_same_host_and_title() {
        let a = engine_result("https://example.com/x", "Introduction", "");
        assert!(is_duplicate(
            &a,
            &engine_result("https://example.com/x", "Other", "")
        ));
        assert!(is_duplicate(
            &a,
            &engine_result("https://example.com/y", "introduction", "")
        ));
        assert!(!is_duplicate(
            &a,
            &engine_result("https://other.com/y", "Introduction", "")
        ));
        assert!(!is_duplicate(
            &a,
            &engine_result("https://example.com/y", "Something else", "")
        ));

        // relative urls (kiwix) dedupe on book + title
        let kiwix = engine_result("/kiwix/content/book_a/A/One", "Same", "");
        assert!(is_duplicate(
            &kiwix,
            &engine_result("/kiwix/content/book_a/A/Two", "same", "")
        ));
        assert!(!is_duplicate(
            &kiwix,
            &engine_result("/kiwix/content/book_b/A/One", "Same", "")
        ));
        assert!(!is_duplicate(
            &kiwix,
            &engine_result("/kiwix/content/book_a/A/Two", "Other", "")
        ));
    }

    #[test]
    fn host_diversity_demotes_repeats() {
        let mut results = vec![
            result("https://example.com/a", "a", ""),
            result("https://example.com/b", "b", ""),
            result("https://other.com/c", "c", ""),
        ];
        apply_host_diversity(&mut results);
        assert!((results[0].score - 1.0).abs() < f64::EPSILON);
        assert!((results[1].score - 0.7).abs() < 1e-9);
        assert!((results[2].score - 1.0).abs() < f64::EPSILON);
    }
}
