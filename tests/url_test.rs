use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

/// Run `expr` inside a fetch handler and return what it evaluated to.
async fn eval(expr: &str) -> String {
    let script = format!(
        r#"addEventListener('fetch', (event) => {{
            let out;

            try {{
                out = String((function() {{ {} }})());
            }} catch (e) {{
                out = 'threw ' + (e.name || e);
            }}

            event.respondWith(new Response(out));
        }});"#,
        expr
    );

    let mut worker = Worker::new(Script::new(script.as_str()), None)
        .await
        .expect("worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "https://example.com/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("task should execute");

    let response = rx.await.expect("should receive response");
    let body = response.body.collect().await.expect("should have body");

    String::from_utf8_lossy(&body).into_owned()
}

#[tokio::test]
async fn test_url_parts() {
    let out = eval(
        "const u = new URL('https://user@example.com:8443/a/b?x=1#frag');
         return [u.protocol, u.hostname, u.port, u.pathname, u.search, u.hash, u.origin].join('|');",
    )
    .await;

    assert_eq!(
        out,
        "https:|example.com|8443|/a/b|?x=1|#frag|https://example.com:8443"
    );
}

#[tokio::test]
async fn test_url_from_url_object() {
    let out = eval(
        "const u = new URL('https://example.com/a?x=1');
         return new URL(u).href;",
    )
    .await;

    assert_eq!(out, "https://example.com/a?x=1");
}

#[tokio::test]
async fn test_url_relative_to_base() {
    let out = eval("return new URL('../c', 'https://example.com/a/b/d').href;").await;
    assert_eq!(out, "https://example.com/a/c");
}

#[tokio::test]
async fn test_url_rejects_relative_without_base() {
    let out = eval("return new URL('/a').href;").await;
    assert_eq!(out, "threw TypeError");
}

#[tokio::test]
async fn test_search_params_reflect_url() {
    let out = eval(
        "const u = new URL('https://example.com/?a=1&b=2');
         return [u.searchParams.get('a'), String(u.searchParams.get('missing')), u.searchParams.size].join('|');",
    )
    .await;

    assert_eq!(out, "1|null|2");
}

#[tokio::test]
async fn test_search_params_write_back_to_url() {
    let out = eval(
        "const u = new URL('https://example.com/');
         u.searchParams.set('q', 'a b');
         u.searchParams.append('q2', 'c');
         return u.href;",
    )
    .await;

    assert_eq!(out, "https://example.com/?q=a+b&q2=c");
}

#[tokio::test]
async fn test_search_params_follow_url_mutation() {
    let out = eval(
        "const u = new URL('https://example.com/?a=1');
         const p = u.searchParams;
         u.search = '?a=2';
         return p.get('a');",
    )
    .await;

    assert_eq!(out, "2");
}

#[tokio::test]
async fn test_search_params_decoding() {
    let out = eval(
        "const p = new URLSearchParams('token=aGk%3D&text=a+b&flag');
         return [p.get('token'), p.get('text'), p.get('flag')].join('|');",
    )
    .await;

    assert_eq!(out, "aGk=|a b|");
}

#[tokio::test]
async fn test_search_params_set_replaces_duplicates() {
    let out = eval(
        "const p = new URLSearchParams('a=1&a=2&b=3');
         p.set('a', '9');
         p.delete('b');
         return p.toString();",
    )
    .await;

    assert_eq!(out, "a=9");
}
