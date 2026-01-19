//! Additional integration tests for boa runtime
//! Basic tests are generated from openworkers_core::generate_worker_tests!

use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

#[tokio::test]
async fn test_large_body() {
    let code = r#"
        addEventListener('fetch', (event) => {
            const largeText = 'x'.repeat(10000);
            event.respondWith(new Response(largeText));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body.collect().await.unwrap().len(), 10000);
}

#[tokio::test]
async fn test_no_handler_registered() {
    let code = r#"
        // No addEventListener call
        const x = 42;
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, _rx) = Event::fetch(req);
    let result = worker.exec(event).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_promise_response() {
    let code = r#"
        addEventListener('fetch', (event) => {
            // Simulate responding with a Promise (like fetch())
            const responsePromise = Promise.resolve(
                new Response('Promise Response', { status: 200 })
            );
            event.respondWith(responsePromise);
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    let body_bytes = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "Promise Response");
}

#[tokio::test]
async fn test_url_parsing() {
    let code = r#"
addEventListener("fetch", (event) => {
  const { pathname, hostname } = new URL(event.request.url);
  const message = `Path: ${pathname}, Host: ${hostname}`;
  event.respondWith(new Response(message, { status: 200 }));
});
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://example.com/test/path".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    let body_bytes = response.body.collect().await.unwrap();
    assert_eq!(
        String::from_utf8_lossy(&body_bytes),
        "Path: /test/path, Host: example.com"
    );
}

#[tokio::test]
async fn test_error_handling() {
    let code = r#"
addEventListener("fetch", (event) => {
  event.respondWith(
    handleRequest().catch(
      (err) => new Response(err.message, { status: 500 })
    )
  );
});

async function handleRequest() {
  throw new Error("Test error");
}
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 500);
    let body_bytes = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "Test error");
}

#[tokio::test]
async fn test_fetch_available() {
    // Just verify fetch() is available and returns a Promise
    let code = r#"
addEventListener("fetch", async (event) => {
  const hasFetch = typeof fetch === 'function';
  event.respondWith(new Response(`fetch available: ${hasFetch}`, { status: 200 }));
});
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    let body_bytes = response.body.collect().await.unwrap();
    let body = String::from_utf8_lossy(&body_bytes);
    assert_eq!(body, "fetch available: true");
}

#[tokio::test]
async fn test_request_headers() {
    let code = r#"
addEventListener("fetch", (event) => {
  const request = event.request;
  const authHeader = request.headers["authorization"] || "none";
  const contentType = request.headers["content-type"] || "none";

  const message = `Auth: ${authHeader}, Content-Type: ${contentType}`;
  event.respondWith(new Response(message, { status: 200 }));
});
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let mut headers = HashMap::new();
    headers.insert("authorization".to_string(), "Bearer token123".to_string());
    headers.insert("content-type".to_string(), "application/json".to_string());

    let req = HttpRequest {
        method: HttpMethod::Post,
        url: "http://localhost/api".to_string(),
        headers,
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    let body_bytes = response.body.collect().await.unwrap();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("Bearer token123"));
    assert!(body.contains("application/json"));
}

#[tokio::test]
async fn test_request_method_and_url() {
    let code = r#"
addEventListener("fetch", (event) => {
  const request = event.request;
  const message = `Method: ${request.method}, URL: ${request.url}`;
  event.respondWith(new Response(message, { status: 200 }));
});
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Post,
        url: "http://example.com/api/users".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    let body_bytes = response.body.collect().await.unwrap();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("Method: POST"));
    assert!(body.contains("URL: http://example.com/api/users"));
}

#[tokio::test]
async fn test_response_headers() {
    let code = r#"
addEventListener("fetch", (event) => {
  const response = new Response("Hello", {
    status: 200,
    headers: {
      "content-type": "text/plain",
      "x-custom-header": "custom-value"
    }
  });
  event.respondWith(response);
});
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    // Check headers are forwarded
    let headers_map: HashMap<String, String> = response.headers.into_iter().collect();
    assert_eq!(
        headers_map.get("content-type"),
        Some(&"text/plain".to_string())
    );
    assert_eq!(
        headers_map.get("x-custom-header"),
        Some(&"custom-value".to_string())
    );
}
