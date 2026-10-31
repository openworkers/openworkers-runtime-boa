use bytes::Bytes;
use openworkers_core::BindingInfo;
use openworkers_core::BindingType;
use openworkers_core::Event;
use openworkers_core::HttpMethod;
use openworkers_core::HttpRequest;
use openworkers_core::HttpResponse;
use openworkers_core::LogLevel;
use openworkers_core::OpFuture;
use openworkers_core::OperationsHandler;
use openworkers_core::RequestBody;
use openworkers_core::ResponseBody;
use openworkers_core::Script;
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;
use std::sync::Arc;

/// Serves ASSETS: /bytes is three bytes that are not UTF-8, /missing is a 404,
/// and any other path echoes the request it got.
struct Assets;

impl OperationsHandler for Assets {
    fn handle_log(&self, _level: LogLevel, _message: String) {}

    fn handle_binding_fetch(
        &self,
        binding: &str,
        request: HttpRequest,
    ) -> OpFuture<'_, Result<HttpResponse, String>> {
        let answer = if binding != "ASSETS" {
            Err(format!("no binding {binding}"))
        } else if request.url.ends_with("/bytes") {
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "image/x-test".into())],
                body: ResponseBody::Bytes(Bytes::from_static(&[0, 255, 128])),
            })
        } else if request.url.ends_with("/missing") {
            Ok(HttpResponse {
                status: 404,
                headers: vec![],
                body: ResponseBody::None,
            })
        } else {
            let echo = format!(
                "{} {} {}",
                request.method,
                request.url,
                request.headers.get("x-a").cloned().unwrap_or_default()
            );

            Ok(HttpResponse {
                status: 200,
                headers: vec![
                    ("content-type".into(), "text/plain".into()),
                    ("set-cookie".into(), "a=1".into()),
                    ("set-cookie".into(), "b=2".into()),
                ],
                body: ResponseBody::Bytes(Bytes::from(echo)),
            })
        };

        Box::pin(async move { answer })
    }
}

/// Run `body` against a worker holding two assets bindings: ASSETS, and OTHER,
/// which the host does not serve.
async fn eval(body: &str) -> String {
    let script = format!(
        r#"addEventListener('fetch', (event) => {{
            event.respondWith((async function() {{
                let out;

                try {{
                    out = String(await (async function() {{ {body} }})());
                }} catch (e) {{
                    out = 'threw ' + e;
                }}

                return new Response(out);
            }})());
        }});"#
    );

    let mut script = Script::new(script.as_str());

    script.bindings = vec![
        BindingInfo::new("ASSETS", BindingType::Assets),
        BindingInfo::new("OTHER", BindingType::Assets),
    ];

    let mut worker = Worker::new_with_ops(script, None, Arc::new(Assets))
        .await
        .expect("worker should initialize");

    let (event, receiver) = Event::fetch(HttpRequest {
        method: HttpMethod::Get,
        url: "https://example.com/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    });

    worker.exec(event).await.expect("worker should run");

    let response = receiver.await.expect("a response");
    let body = response
        .body
        .collect()
        .await
        .expect("a readable body")
        .unwrap_or_default();

    String::from_utf8_lossy(&body).to_string()
}

#[tokio::test]
async fn a_path_is_resolved_on_the_host_of_the_binding() {
    let out = eval("return (await env.ASSETS.fetch('/a/../b.txt')).text();").await;

    assert_eq!(out, "GET http://localhost/b.txt ");
}

#[tokio::test]
async fn a_request_keeps_its_url_method_and_headers() {
    let out = eval(
        "const request = new Request('https://app.test/x', { method: 'POST', headers: { 'x-a': '1' } });
         return (await env.ASSETS.fetch(request)).text();",
    )
    .await;

    assert_eq!(out, "POST https://app.test/x 1");
}

#[tokio::test]
async fn init_gives_the_method_and_headers_of_a_path() {
    let out = eval(
        "return (await env.ASSETS.fetch('/x', { method: 'HEAD', headers: { 'x-a': '2' } })).text();",
    )
    .await;

    assert_eq!(out, "HEAD http://localhost/x 2");
}

#[tokio::test]
async fn the_status_and_the_headers_come_back() {
    let out = eval(
        "const found = await env.ASSETS.fetch('/x');
         const missing = await env.ASSETS.fetch('/missing');
         return [found.status, found.headers.get('content-type'), missing.status, missing.ok].join(' ');",
    )
    .await;

    assert_eq!(out, "200 text/plain 404 false");
}

#[tokio::test]
async fn bytes_come_back_unchanged() {
    let out = eval(
        "const response = await env.ASSETS.fetch('/bytes');
         return Array.from(new Uint8Array(await response.arrayBuffer())).join(',');",
    )
    .await;

    assert_eq!(out, "0,255,128");
}

#[tokio::test]
async fn an_error_of_the_host_rejects() {
    let out = eval("return (await env.OTHER.fetch('/x')).text();").await;

    assert_eq!(out, "threw TypeError: no binding OTHER");
}

#[tokio::test]
async fn a_method_the_host_does_not_know_throws() {
    let out = eval("return (await env.ASSETS.fetch('/x', { method: 'NOPE' })).text();").await;

    assert!(out.starts_with("threw TypeError"), "{out}");
}

#[test]
fn assets_is_supported() {
    assert!(openworkers_runtime_boa::SUPPORTED_BINDINGS.contains(&BindingType::Assets));
}
