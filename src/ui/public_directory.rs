//! Public app directory for a hosted board.
//!
//! `/apps/explore` and `/apps/catalog/<id>` are answered before the board
//! session gate. The browser talks only to this board. The board reads the
//! catalog itself, with no cookies and no caller-chosen URL. Installing stays
//! on the existing sign-in path.
//!
//! The catalog is the board's own API, or a catalog the operator names. It is
//! never the signed-in web app: this directory is public, so the app origin the
//! worker hands the board for its own callbacks is deliberately not read here.

use serde_json::Value;
use tiny_http::{Header, Response, StatusCode};

use super::write_path::HttpResp;

fn clean_origin(raw: Option<&str>) -> Option<String> {
    let raw = raw.filter(|v| !v.trim().is_empty())?;
    let raw = raw.trim().trim_end_matches('/');
    if raw.contains(['?', '#', '@', ' ', '\\']) || raw.matches("//").count() != 1 {
        return None;
    }
    Some(raw.to_string())
}

fn reachable_catalog(raw: &str) -> bool {
    raw.starts_with("https://")
        || raw.starts_with("http://127.0.0.1")
        || raw.starts_with("http://localhost")
}

/// A hosted board has no internet: when the issuer is exactly
/// `http://api.internal`, that outbound handler is the catalog. Otherwise the
/// catalog is whatever `AGENTICOS_BOARD_CATALOG_ORIGIN` names — an operator's
/// own API, or a loopback rig. `AGENTICOS_BOARD_API_ORIGIN` is the signed-in
/// web app the worker hands the board for its own callbacks; it is not a
/// catalog, and reading it here is what once pointed the public directory at
/// the private app.
pub(super) fn catalog_origin_from(catalog: Option<&str>, issuer: Option<&str>) -> Option<String> {
    let issuer = clean_origin(issuer);
    if issuer.as_deref() == Some("http://api.internal") {
        return issuer;
    }
    clean_origin(catalog).filter(|raw| reachable_catalog(raw))
}

fn catalog_origin() -> Option<String> {
    catalog_origin_from(
        std::env::var("AGENTICOS_BOARD_CATALOG_ORIGIN")
            .ok()
            .as_deref(),
        std::env::var("AGENTICOS_BOARD_ISSUER").ok().as_deref(),
    )
}

enum Route {
    List,
    Detail(String),
}

fn catalog_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && !id.contains("..")
        && !id.contains('/')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn route(path: &str) -> Option<Route> {
    // The catalogue's public home is `/apps/explore`. `/` and `/apps` are the
    // board's own front door and keep the session gate.
    if path == "/apps/explore" {
        return Some(Route::List);
    }
    let id = path.strip_prefix("/apps/catalog/")?;
    catalog_id(id).then(|| Route::Detail(id.to_string()))
}

fn query_value(raw: &str, key: &str) -> String {
    let Some(value) = raw.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then_some(v)
    }) else {
        return String::new();
    };
    let spaced = value.replace('+', " ");
    super::serve::pct_decode(&spaced)
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect()
}

fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn pct_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn sign_in_href(page_url: &str) -> String {
    format!("/__platform/login?to={}", pct_encode(page_url))
}

fn text(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}

fn listing(app: &Value) -> Option<&Value> {
    app.get("listing").filter(|v| v.is_object())
}

fn publisher(app: &Value) -> String {
    let Some(listing) = listing(app) else {
        return String::new();
    };
    match listing.get("publisher") {
        Some(Value::String(name)) => name.clone(),
        Some(Value::Object(obj)) => obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn category(app: &Value) -> &str {
    listing(app)
        .and_then(|row| row.get("category"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn tagline(app: &Value) -> &str {
    listing(app)
        .and_then(|row| row.get("tagline"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(app.get("summary")))
}

fn apps_of(body: &Value) -> Vec<&Value> {
    body.get("data")
        .and_then(|data| data.get("apps"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().take(100).collect())
        .unwrap_or_default()
}

fn matches_query(app: &Value, q: &str, cat: &str) -> bool {
    if !cat.is_empty() && !category(app).eq_ignore_ascii_case(cat) {
        return false;
    }
    if q.is_empty() {
        return true;
    }
    let needle = q.to_lowercase();
    let publisher = publisher(app);
    let mut hay = String::new();
    for part in [
        text(app.get("title")),
        text(app.get("summary")),
        tagline(app),
        category(app),
        publisher.as_str(),
    ] {
        hay.push(' ');
        hay.push_str(part);
    }
    if let Some(tags) = listing(app)
        .and_then(|row| row.get("tags"))
        .and_then(Value::as_array)
    {
        for tag in tags {
            if let Some(tag) = tag.as_str() {
                hay.push(' ');
                hay.push_str(tag);
            }
        }
    }
    hay.to_lowercase().contains(&needle)
}

const PAGE_STYLE: &str = r#"
body{margin:0;font:16px/1.45 system-ui,sans-serif;background:#f6f7f9;color:#1c1e21}
a{color:#1a4fad;text-decoration:none} a:hover{text-decoration:underline}
header,main{max-width:960px;margin:0 auto;padding:20px 16px}
header{display:flex;align-items:center;gap:16px}
header .grow{flex:1}
h1{font-size:28px;margin:0 0 8px}
.lede{color:#4b5160;margin:0 0 16px}
form{display:flex;gap:8px;margin:0 0 12px}
input[type=search]{flex:1;padding:10px 12px;border:1px solid #c8ced8;border-radius:8px;font:inherit}
button,.btn{display:inline-block;padding:8px 12px;border-radius:8px;border:1px solid #c8ced8;background:#fff;font:inherit;cursor:pointer}
.cats{display:flex;flex-wrap:wrap;gap:8px;margin:0 0 16px}
.cats a{padding:4px 10px;border-radius:999px;background:#fff;border:1px solid #d5dbe3}
.cats a.on{background:#1c1e21;color:#fff;border-color:#1c1e21}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:12px}
.card{display:block;background:#fff;border:1px solid #e1e5eb;border-radius:12px;padding:16px}
.card h2{font-size:18px;margin:0 0 4px;color:#1c1e21}
.meta{color:#5c6570;font-size:13px;margin:0 0 8px}
.summary{margin:0;color:#333}
.flag{display:inline-block;margin-top:10px;font-size:13px;color:#5c6570}
ul{padding-left:18px} .note{background:#fff;border:1px solid #e1e5eb;border-radius:12px;padding:16px}
"#;

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{style}</style>
</head>
<body>
{body}
</body>
</html>
"#,
        title = esc(title),
        style = PAGE_STYLE,
        body = body,
    )
}

/// Who is reading the directory. Both see the same catalog; only the action
/// differs, because installing happens on the board's own apps page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Viewer {
    Anonymous,
    SignedIn,
}

impl Viewer {
    /// The action: sign in for a visitor, their own apps for a live session.
    fn cta(self, page_url: &str) -> String {
        match self {
            Viewer::Anonymous => format!(
                r#"<a class="btn" href="{sign}">Sign in to install</a>"#,
                sign = esc(&sign_in_href(page_url)),
            ),
            Viewer::SignedIn => {
                r#"<a class="btn" href="/apps">Install from your apps</a>"#.to_string()
            }
        }
    }

    fn lede(self) -> &'static str {
        match self {
            Viewer::Anonymous => {
                "Published apps. Browse with no account. Installing needs a sign-in on this board."
            }
            Viewer::SignedIn => {
                "Published apps. Browse the catalog; installing happens from your apps page."
            }
        }
    }
}

fn header(page_url: &str, viewer: Viewer) -> String {
    format!(
        r#"<header>
<a href="/apps/explore"><strong>Apps</strong></a>
<span class="grow"></span>
{cta}
</header>"#,
        cta = viewer.cta(page_url),
    )
}

fn render_list(body: &Value, q: &str, cat: &str, page_url: &str, viewer: Viewer) -> String {
    let apps = apps_of(body);
    let mut categories: Vec<&str> = apps
        .iter()
        .map(|app| category(app))
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort_unstable();
    categories.dedup();
    let shown: Vec<&&Value> = apps
        .iter()
        .filter(|app| matches_query(app, q, cat))
        .collect();
    let mut cats = String::from(r#"<div class="cats">"#);
    let all_on = if cat.is_empty() { " on" } else { "" };
    cats.push_str(&format!(
        r#"<a class="{all_on}" href="/apps/explore{qmark}">All</a>"#,
        all_on = all_on,
        qmark = if q.is_empty() {
            String::new()
        } else {
            format!("?q={}", pct_encode(q))
        },
    ));
    for name in categories {
        let on = if name.eq_ignore_ascii_case(cat) {
            " on"
        } else {
            ""
        };
        let mut href = format!("/apps/explore?cat={}", pct_encode(name));
        if !q.is_empty() {
            href.push_str(&format!("&q={}", pct_encode(q)));
        }
        cats.push_str(&format!(
            r#"<a class="{on}" href="{href}">{name}</a>"#,
            on = on,
            href = esc(&href),
            name = esc(name),
        ));
    }
    cats.push_str("</div>");
    let mut cards = String::from(r#"<div class="grid">"#);
    if shown.is_empty() {
        cards.push_str(r#"<p class="lede">No apps match.</p>"#);
    }
    for app in shown {
        let id = text(app.get("id"));
        if !catalog_id(id) {
            continue;
        }
        let installable = app.get("installable").and_then(Value::as_bool) == Some(true);
        let flag = if installable {
            "Can be installed"
        } else {
            "Listed, not installable yet"
        };
        let pub_name = publisher(app);
        let meta = if pub_name.is_empty() {
            format!("{} · {}", text(app.get("version")), category(app))
        } else {
            format!(
                "{} · {} · {}",
                pub_name,
                text(app.get("version")),
                category(app)
            )
        };
        cards.push_str(&format!(
            r#"<a class="card" href="/apps/catalog/{id}"><h2>{title}</h2><p class="meta">{meta}</p><p class="summary">{summary}</p><span class="flag">{flag}</span></a>"#,
            id = esc(id),
            title = esc(text(app.get("title"))),
            meta = esc(&meta),
            summary = esc(tagline(app)),
            flag = flag,
        ));
    }
    cards.push_str("</div>");
    page(
        "Apps",
        &format!(
            r#"{header}<main>
<h1>App directory</h1>
<p class="lede">{lede}</p>
<form method="get" action="/apps/explore">
<input type="search" name="q" value="{q}" placeholder="Search apps" aria-label="Search apps">
{hidden}
<button type="submit">Search</button>
</form>
{cats}
{cards}
</main>"#,
            header = header(page_url, viewer),
            lede = viewer.lede(),
            q = esc(q),
            hidden = if cat.is_empty() {
                String::new()
            } else {
                format!(r#"<input type="hidden" name="cat" value="{}">"#, esc(cat))
            },
            cats = cats,
            cards = cards,
        ),
    )
}

fn strings(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn render_detail(app: &Value, page_url: &str, viewer: Viewer) -> String {
    let installable = app.get("installable").and_then(Value::as_bool) == Some(true);
    let action = if installable {
        viewer.cta(page_url)
    } else {
        let reason = text(app.get("unavailableReason"));
        if reason.is_empty() {
            "<p class=\"flag\">Listed, not installable yet.</p>".to_string()
        } else {
            format!("<p class=\"flag\">{}</p>", esc(reason))
        }
    };
    let can = strings(listing(app).and_then(|row| row.get("can")));
    let mut can_html = String::new();
    if !can.is_empty() {
        can_html.push_str("<h2>What it can do</h2><ul>");
        for item in can {
            can_html.push_str(&format!("<li>{}</li>", esc(item)));
        }
        can_html.push_str("</ul>");
    }
    let about = listing(app)
        .and_then(|row| row.get("about"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let about_html = if about.is_empty() {
        String::new()
    } else {
        format!("<p>{}</p>", esc(about))
    };
    page(
        text(app.get("title")),
        &format!(
            r#"{header}<main>
<p><a href="/apps/explore">All apps</a></p>
<h1>{title}</h1>
<p class="meta">{meta}</p>
<p class="lede">{tagline}</p>
{about}
{can}
<p>{action}</p>
</main>"#,
            header = header(page_url, viewer),
            title = esc(text(app.get("title"))),
            meta = esc(&format!(
                "{} · {}",
                publisher(app),
                text(app.get("version"))
            )),
            tagline = esc(tagline(app)),
            about = about_html,
            can = can_html,
            action = action,
        ),
    )
}

fn html_response(status: u16, body: String) -> HttpResp {
    let mut resp = Response::from_data(body.into_bytes()).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

const BODY_CAP: u64 = 1024 * 1024;

const TRANSIENT: &str = "The app directory didn't load. Try again in a moment.";

/// One catalog read. The catalog answering 404 means that app does not exist,
/// which is a different page from a catalog that is unreachable or unwell.
enum Fetched {
    Ok(Value),
    NotFound,
    Failed,
}

fn fetch_catalog(url: &str) -> Fetched {
    use std::time::Duration;
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let Ok(mut response) = agent.get(url).call() else {
        return Fetched::Failed;
    };
    match response.status().as_u16() {
        200 => {}
        404 => return Fetched::NotFound,
        _ => return Fetched::Failed,
    }
    let Ok(bytes) = response
        .body_mut()
        .with_config()
        .limit(BODY_CAP)
        .read_to_vec()
    else {
        return Fetched::Failed;
    };
    match serde_json::from_slice(&bytes) {
        Ok(body) => Fetched::Ok(body),
        Err(_) => Fetched::Failed,
    }
}

fn unavailable_html(page_url: &str, message: &str, viewer: Viewer) -> String {
    page(
        "Apps",
        &format!(
            r#"{header}<main><h1>App directory</h1><p class="lede">{message}</p></main>"#,
            header = header(page_url, viewer),
            message = esc(message),
        ),
    )
}

fn no_such_app_html(page_url: &str, viewer: Viewer) -> String {
    page(
        "App",
        &format!(
            r#"{header}<main><h1>No such app</h1><p><a href="/apps/explore">All apps</a></p></main>"#,
            header = header(page_url, viewer),
        ),
    )
}

/// Status and body for one directory read. Pure: the fetch already happened.
fn page_for(
    route: &Route,
    fetched: Fetched,
    raw_query: &str,
    page_url: &str,
    viewer: Viewer,
) -> (u16, String) {
    let body = match fetched {
        Fetched::Ok(body) => body,
        Fetched::NotFound => {
            return match route {
                Route::Detail(_) => (404, no_such_app_html(page_url, viewer)),
                Route::List => (200, unavailable_html(page_url, TRANSIENT, viewer)),
            };
        }
        Fetched::Failed => return (200, unavailable_html(page_url, TRANSIENT, viewer)),
    };
    match route {
        Route::List => (
            200,
            render_list(
                &body,
                &query_value(raw_query, "q"),
                &query_value(raw_query, "cat"),
                page_url,
                viewer,
            ),
        ),
        Route::Detail(id) => {
            let app = body.get("data").and_then(|data| data.get("app"));
            match app.filter(|app| text(app.get("id")) == id) {
                Some(app) => (200, render_detail(app, page_url, viewer)),
                None => (404, no_such_app_html(page_url, viewer)),
            }
        }
    }
}

pub(super) fn is_public_route(path: &str) -> bool {
    route(path).is_some()
}

/// `Some` when this path is the public directory. Other board routes stay gated.
/// The catalog is the same for a visitor and for a live session.
pub(super) fn response(
    path: &str,
    raw_query: &str,
    page_url: &str,
    viewer: Viewer,
) -> Option<HttpResp> {
    let route = route(path)?;
    let Some(origin) = catalog_origin() else {
        return Some(html_response(
            200,
            unavailable_html(
                page_url,
                "This board has no public catalog configured.",
                viewer,
            ),
        ));
    };
    let upstream = match &route {
        Route::List => format!("{origin}/public/apps"),
        Route::Detail(id) => format!("{origin}/public/apps/{id}"),
    };
    let (status, html) = page_for(&route, fetch_catalog(&upstream), raw_query, page_url, viewer);
    Some(html_response(status, html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_directory_routes_are_public() {
        assert!(route("/apps/explore").is_some());
        assert!(route("/apps/catalog/social-content").is_some());
        // The board front door is not the catalogue.
        assert!(route("/").is_none());
        assert!(route("/apps").is_none());
        assert!(route("/api/health").is_none());
        assert!(route("/apps/catalog/../admin").is_none());
        assert!(route("/apps/catalog/a/b").is_none());
        assert!(route("/apps/manage/inst").is_none());
    }

    #[test]
    fn the_catalog_is_the_board_api_or_a_named_catalog_never_the_web_app() {
        // A hosted board reads its own API, whatever else is set.
        assert_eq!(
            catalog_origin_from(None, Some("http://api.internal")).as_deref(),
            Some("http://api.internal")
        );
        assert_eq!(
            catalog_origin_from(
                Some("https://api-v2.agenticos.hk"),
                Some("http://api.internal"),
            )
            .as_deref(),
            Some("http://api.internal")
        );
        // A rig or a self-hosted board names its own catalog.
        assert_eq!(
            catalog_origin_from(Some("https://api-v2.agenticos.hk/"), None).as_deref(),
            Some("https://api-v2.agenticos.hk")
        );
        assert_eq!(
            catalog_origin_from(Some("http://127.0.0.1:8812"), None).as_deref(),
            Some("http://127.0.0.1:8812")
        );
        // The signed-in web app is not a catalog, and an issuer alone is not one.
        assert!(catalog_origin_from(None, Some("https://app-v2.agenticos.hk")).is_none());
        assert!(catalog_origin_from(None, Some("https://app-v2.agenticos.hk/")).is_none());
        // Still refused: plain http off loopback, query strings, lookalike hosts.
        assert!(catalog_origin_from(Some("http://evil.example"), None).is_none());
        assert!(catalog_origin_from(Some("https://api.example/?x=1"), None).is_none());
        assert!(catalog_origin_from(None, Some("http://api.internal.evil")).is_none());
        assert!(catalog_origin_from(None, Some("http://api.internal/public/apps")).is_none());
    }

    #[test]
    fn list_shows_apps_that_are_not_installable_and_escapes_titles() {
        let body = json!({"ok": true, "data": {"apps": [
            {"id": "social-content", "title": "Social <Content>", "summary": "Drafts", "version": "0.9.7",
             "installable": true, "listing": {"tagline": "Draft a caption", "category": "marketing", "publisher": {"name": "Cadence"}}},
            {"id": "crm", "title": "CRM", "summary": "Customers", "version": "0.1.0",
             "installable": false, "listing": {"category": "sales", "publisher": {"name": "Cadence"}}}
        ]}});
        let html = render_list(
            &body,
            "",
            "",
            "https://board.example/apps/explore",
            Viewer::Anonymous,
        );
        assert!(html.contains("Social &lt;Content&gt;"));
        assert!(html.contains("Listed, not installable yet"));
        assert!(html.contains("Can be installed"));
        assert!(html.contains("/apps/catalog/crm"));
        assert!(!html.contains("app-v2.agenticos.hk/apps"));
        assert!(html.contains("/__platform/login?to="));
        let filtered = render_list(
            &body,
            "caption",
            "marketing",
            "https://board.example/apps/explore",
            Viewer::Anonymous,
        );
        assert!(filtered.contains("social-content"));
        assert!(!filtered.contains("/apps/catalog/crm"));
    }

    /// One canned HTTP response on loopback, so the catalog read is exercised
    /// against a real socket instead of a mock.
    fn catalog_stub(status_line: &str, body: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (status_line, body) = (status_line.to_string(), body.to_string());
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{addr}/public/apps")
    }

    #[test]
    fn the_catalog_status_decides_between_missing_and_broken() {
        assert!(matches!(
            fetch_catalog(&catalog_stub("404 Not Found", r#"{"ok":false}"#)),
            Fetched::NotFound
        ));
        assert!(matches!(
            fetch_catalog(&catalog_stub("500 Internal Server Error", "{}")),
            Fetched::Failed
        ));
        assert!(matches!(
            fetch_catalog(&catalog_stub("200 OK", "not json")),
            Fetched::Failed
        ));
        assert!(matches!(
            fetch_catalog(&catalog_stub("200 OK", r#"{"ok":true}"#)),
            Fetched::Ok(_)
        ));
    }

    #[test]
    fn a_missing_app_is_a_404_page_while_a_broken_catalog_is_not() {
        let page_url = "https://board.example/apps/catalog/nope";
        let (status, html) = page_for(
            &Route::Detail("nope".to_string()),
            Fetched::NotFound,
            "",
            page_url,
            Viewer::Anonymous,
        );
        assert_eq!(status, 404);
        assert!(html.contains("No such app"));

        // A catalog that never answered says nothing about the app.
        let (status, html) = page_for(
            &Route::Detail("nope".to_string()),
            Fetched::Failed,
            "",
            page_url,
            Viewer::Anonymous,
        );
        assert_eq!(status, 200);
        assert!(html.contains("Try again"));

        // A 404 from the list endpoint is not a claim about any app either.
        let (status, html) = page_for(
            &Route::List,
            Fetched::NotFound,
            "",
            "https://board.example/apps/explore",
            Viewer::Anonymous,
        );
        assert_eq!(status, 200);
        assert!(html.contains("Try again"));
    }

    #[test]
    fn a_listed_app_renders_its_detail_and_an_unknown_id_stays_not_found() {
        let body = json!({"ok": true, "data": {"app": {
            "id": "social-content",
            "title": "Social Content",
            "version": "0.9.7",
            "listing": {"tagline": "Draft a caption", "publisher": {"name": "Cadence"}}
        }}});
        let (status, html) = page_for(
            &Route::Detail("social-content".to_string()),
            Fetched::Ok(body.clone()),
            "",
            "https://board.example/apps/catalog/social-content",
            Viewer::Anonymous,
        );
        assert_eq!(status, 200);
        assert!(html.contains("Social Content"));
        assert!(html.contains("/__platform/login?to="));

        let (status, html) = page_for(
            &Route::Detail("crm".to_string()),
            Fetched::Ok(body),
            "",
            "https://board.example/apps/catalog/crm",
            Viewer::Anonymous,
        );
        assert_eq!(status, 404);
        assert!(html.contains("No such app"));
    }

    #[test]
    fn a_live_session_browses_the_same_catalog_with_its_own_action() {
        let body = json!({"ok": true, "data": {"apps": [
            {"id": "social-content", "title": "Social Content", "summary": "Drafts",
             "version": "0.9.7", "installable": true,
             "listing": {"tagline": "Draft a caption", "category": "marketing",
                         "publisher": {"name": "Cadence"}}}
        ]}});
        let html = render_list(
            &body,
            "",
            "",
            "https://board.example/apps/explore",
            Viewer::SignedIn,
        );
        // The catalog is the same; installing points at the board's own page.
        assert!(html.contains("social-content"));
        assert!(html.contains(r#"<a class="btn" href="/apps">"#));
        assert!(!html.contains("/__platform/login?to="));
        assert!(!html.contains("Installing needs a sign-in"));

        let (status, html) = page_for(
            &Route::Detail("social-content".to_string()),
            Fetched::Ok(json!({"ok": true, "data": {"app": {
                "id": "social-content", "title": "Social Content", "version": "0.9.7",
                "installable": true,
                "listing": {"publisher": {"name": "Cadence"}}
            }}})),
            "",
            "https://board.example/apps/catalog/social-content",
            Viewer::SignedIn,
        );
        assert_eq!(status, 200);
        assert!(html.contains(r#"<a class="btn" href="/apps">"#));
        assert!(!html.contains("/__platform/login?to="));
    }
}
