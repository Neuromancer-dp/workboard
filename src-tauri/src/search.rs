//! DuckDuckGo HTML scraping (port of the prototype's serve.py). Runs in Rust so the webview needs no network access.

use reqwest::Url;
use scraper::{Html, Selector};
use serde::Serialize;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";
const ENDPOINT: &str = "https://html.duckduckgo.com/html/";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// DDG wraps links as //duckduckgo.com/l/?uddg=<real url>; unwrap them.
pub fn clean_url(href: &str) -> String {
    let abs = if let Some(rest) = href.strip_prefix("//") { format!("https://{rest}") } else { href.to_string() };
    let base = Url::parse("https://duckduckgo.com/").expect("static url");
    let Ok(u) = base.join(&abs) else { return href.to_string() };
    match u.query_pairs().find(|(k, _)| k == "uddg") {
        Some((_, v)) => v.into_owned(),
        None => {
            if abs.starts_with("http") {
                abs
            } else {
                href.to_string()
            }
        }
    }
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pull (title, url, snippet) out of result markup. Walks `a.result__a` and `.result__snippet`
/// in document order; a snippet belongs to the most recent title, exactly like the prototype.
pub fn parse(html: &str, n: usize) -> Vec<Hit> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("a.result__a, .result__snippet").expect("static selector");
    let mut raw: Vec<(String, String, String)> = Vec::new();
    for el in doc.select(&sel) {
        let text: String = el.text().collect();
        if el.value().name() == "a" && el.value().has_class("result__a", scraper::CaseSensitivity::CaseSensitive) {
            raw.push((text, clean_url(el.value().attr("href").unwrap_or("")), String::new()));
        } else if let Some(cur) = raw.last_mut() {
            cur.2.push_str(&text);
        }
    }
    raw.into_iter()
        .map(|(t, u, s)| Hit { title: squash(&t), url: squash(&u), snippet: squash(&s) })
        .filter(|h| !h.title.is_empty() && h.url.starts_with("http") && !h.url.contains("duckduckgo.com/y.js"))
        .take(n)
        .collect()
}

pub fn looks_blocked(html: &str) -> bool {
    html.contains("anomaly") || html.to_lowercase().contains("captcha")
}

pub async fn search(client: &reqwest::Client, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let q: String = q.chars().take(200).collect();
    let body = Url::parse_with_params("https://x.invalid/", [("q", q.as_str())]).expect("static url").query().unwrap_or("").to_string();
    let resp = client
        .post(ENDPOINT)
        .header("User-Agent", UA)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .timeout(Duration::from_secs(12))
        .body(body)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "DuckDuckGo timed out".to_string() } else { "could not reach DuckDuckGo (offline?)".to_string() })?;
    let status = resp.status();
    let html = resp.text().await.map_err(|_| "DuckDuckGo response was cut off".to_string())?;
    let hits = parse(&html, n);
    if hits.is_empty() && looks_blocked(&html) {
        return Err("DuckDuckGo asked for a captcha (rate-limited). Try again in a bit.".into());
    }
    if !status.is_success() {
        return Err(format!("DuckDuckGo returned HTTP {}", status.as_u16()));
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = include_str!("../tests/fixtures/ddg_results.html");
    const ANOMALY: &str = include_str!("../tests/fixtures/ddg_anomaly.html");
    const EMPTY: &str = include_str!("../tests/fixtures/ddg_empty.html");

    #[test]
    fn parses_titles_urls_snippets() {
        let hits = parse(RESULTS, 10);
        let titles: Vec<&str> = hits.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Rust Programming Language",
                "Rust (programming language) – Wikipedia",
                "The Rust Programming Language - The Book",
                "Result without snippet",
                "crates.io: Rust Package Registry",
            ],
            "ads (y.js) and non-http links are dropped"
        );
        assert_eq!(hits[0].url, "https://rust-lang.org/");
        assert_eq!(hits[0].snippet, "A language empowering everyone to build reliable and efficient software.");
    }

    #[test]
    fn unwraps_redirect_links_and_normalizes_text() {
        let hits = parse(RESULTS, 10);
        assert_eq!(hits[1].url, "https://en.wikipedia.org/wiki/Rust_(programming_language)?a=1&b=2");
        assert_eq!(hits[1].snippet, "Rust is a general-purpose programming language & it emphasizes performance, type safety, and concurrency.");
        assert_eq!(hits[2].url, "https://doc.rust-lang.org/book/", "relative /l/?uddg= links work");
        assert_eq!(hits[2].snippet, "Affectionately nicknamed \"the book,\" this text will give you an overview.", "div snippets work");
        assert_eq!(hits[3].snippet, "");
        assert_eq!(hits[4].snippet, "The Rust community's crate registry.");
    }

    #[test]
    fn respects_limit() {
        assert_eq!(parse(RESULTS, 2).len(), 2);
        assert_eq!(parse(RESULTS, 0).len(), 0);
    }

    #[test]
    fn ad_snippet_does_not_leak_into_next_result() {
        let hits = parse(RESULTS, 10);
        assert!(hits.iter().all(|h| !h.snippet.contains("Sponsored")));
    }

    #[test]
    fn detects_captcha_and_empty_pages() {
        assert!(parse(ANOMALY, 5).is_empty());
        assert!(looks_blocked(ANOMALY));
        assert!(parse(EMPTY, 5).is_empty());
        assert!(!looks_blocked(EMPTY));
        assert!(!looks_blocked(RESULTS));
    }

    #[test]
    fn clean_url_cases() {
        assert_eq!(clean_url("https://a.example/x"), "https://a.example/x");
        assert_eq!(clean_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.example%2F%3Fq%3D1&rut=x"), "https://a.example/?q=1");
        assert_eq!(clean_url("//cdn.example/x"), "https://cdn.example/x");
        assert_eq!(clean_url(""), "");
        assert_eq!(clean_url("javascript:alert(1)"), "javascript:alert(1)");
    }
}
