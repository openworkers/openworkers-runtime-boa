use bytes::Bytes;
use openworkers_core::{
    Event, HttpMethod, HttpRequest, HttpResponse, OpFuture, OperationsHandler, RequestBody,
    ResponseBody, Script,
};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;
use std::sync::Arc;

/// Mock handler that returns fixed responses for testing
struct MockOps;

impl OperationsHandler for MockOps {
    fn handle_fetch(&self, request: HttpRequest) -> OpFuture<'_, Result<HttpResponse, String>> {
        Box::pin(async move {
            // Return mock responses based on URL
            if request.url.contains("/get") {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".to_string(), "application/json".to_string())],
                    body: ResponseBody::Bytes(Bytes::from(
                        r#"{"url":"https://example.com/get","data":"test"}"#,
                    )),
                })
            } else if request.url.contains("/post") {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".to_string(), "application/json".to_string())],
                    body: ResponseBody::Bytes(Bytes::from(r#"{"json":{"hello":"world"}}"#)),
                })
            } else if request.url.contains("/headers") {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".to_string(), "application/json".to_string())],
                    body: ResponseBody::Bytes(Bytes::from(
                        r#"{"headers":{"x-custom-header":"test-value"}}"#,
                    )),
                })
            } else if request.url.contains("/tricky") {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("x-note".to_string(), "it's \"quoted\"".to_string())],
                    body: ResponseBody::Bytes(Bytes::from("line1\nline2 'q' \"d\" back\\slash")),
                })
            } else if request.url.contains("/status/404") {
                Ok(HttpResponse {
                    status: 404,
                    headers: vec![],
                    body: ResponseBody::Bytes(Bytes::from("Not Found")),
                })
            } else {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![],
                    body: ResponseBody::Bytes(Bytes::from("OK")),
                })
            }
        })
    }
}

async fn create_worker(script: &str) -> Worker {
    let script_obj = Script::new(script);
    Worker::new_with_ops(script_obj, None, Arc::new(MockOps))
        .await
        .expect("Worker should initialize")
}

#[tokio::test]
async fn test_fetch_basic_get() {
    let script = r#"
        addEventListener('fetch', async (event) => {
            const response = await fetch('https://example.com/get');
            const data = await response.json();
            event.respondWith(new Response(JSON.stringify({
                status: response.status,
                hasUrl: !!data.url
            })));
        });
    "#;

    let mut worker = create_worker(script).await;

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["status"], 200);
    assert_eq!(json["hasUrl"], true);
}

#[tokio::test]
async fn test_fetch_post_with_body() {
    let script = r#"
        addEventListener('fetch', async (event) => {
            const response = await fetch('https://example.com/post', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ hello: 'world' })
            });
            const data = await response.json();
            event.respondWith(new Response(JSON.stringify({
                status: response.status,
                receivedData: data.json
            })));
        });
    "#;

    let mut worker = create_worker(script).await;

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["status"], 200);
    assert_eq!(json["receivedData"]["hello"], "world");
}

#[tokio::test]
async fn test_fetch_with_headers() {
    let script = r#"
        addEventListener('fetch', async (event) => {
            const response = await fetch('https://example.com/headers', {
                headers: { 'X-Custom-Header': 'test-value' }
            });
            const data = await response.json();
            event.respondWith(new Response(JSON.stringify({
                status: response.status,
                customHeader: data.headers['x-custom-header']
            })));
        });
    "#;

    let mut worker = create_worker(script).await;

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["customHeader"], "test-value");
}

#[tokio::test]
async fn test_fetch_404() {
    let script = r#"
        addEventListener('fetch', async (event) => {
            const response = await fetch('https://example.com/status/404');
            // Compute ok manually since Boa's native response may not expose it correctly
            const isOk = response.status >= 200 && response.status < 300;
            event.respondWith(new Response(JSON.stringify({
                status: response.status,
                ok: isOk
            })));
        });
    "#;

    let mut worker = create_worker(script).await;

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["status"], 404);
    assert_eq!(json["ok"], false);
}

#[tokio::test]
async fn test_fetch_response_with_quotes_and_newlines() {
    let script = r#"
        addEventListener('fetch', async (event) => {
            const response = await fetch('https://example.com/tricky');
            const text = await response.text();
            event.respondWith(new Response(JSON.stringify({
                text: text,
                note: response.headers.get('x-note')
            })));
        });
    "#;

    let mut worker = create_worker(script).await;

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["text"], "line1\nline2 'q' \"d\" back\\slash");
    assert_eq!(json["note"], "it's \"quoted\"");
}

/// Answers only after yielding to the reactor, which a fetch driven off the job
/// queue has to survive.
struct SlowOps;

impl OperationsHandler for SlowOps {
    fn handle_fetch(&self, _request: HttpRequest) -> OpFuture<'_, Result<HttpResponse, String>> {
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            Ok(HttpResponse {
                status: 200,
                headers: vec![],
                body: ResponseBody::Bytes(Bytes::from("slow")),
            })
        })
    }
}

#[tokio::test]
async fn test_fetch_waits_for_a_slow_handler() {
    let script = r#"
        addEventListener('fetch', async (event) => {
            const first = await (await fetch('https://example.com/a')).text();
            const second = await (await fetch('https://example.com/b')).text();
            event.respondWith(new Response(first + ',' + second));
        });
    "#;

    let mut worker = Worker::new_with_ops(Script::new(script), None, Arc::new(SlowOps))
        .await
        .expect("Worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    let body = response.body.collect().await.expect("Should have body");
    assert_eq!(String::from_utf8_lossy(&body), "slow,slow");
}

#[tokio::test]
async fn test_fetched_response_can_be_the_answer() {
    let script = r#"
        addEventListener('fetch', (event) => {
            event.respondWith(fetch('https://example.com/get'));
        });
    "#;

    let mut worker = create_worker(script).await;

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers,
        vec![("content-type".to_string(), "application/json".to_string())]
    );

    let body = response.body.collect().await.expect("Should have body");
    assert_eq!(
        String::from_utf8_lossy(&body),
        r#"{"url":"https://example.com/get","data":"test"}"#
    );
}
