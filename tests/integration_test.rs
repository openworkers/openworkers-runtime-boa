use openworkers_runtime_boa::{HttpRequest, Script, Task, TerminationReason, Worker};
use std::collections::HashMap;

#[tokio::test]
async fn test_simple_response() {
    let code = r#"
        addEventListener('fetch', (event) => {
            event.respondWith(new Response('Hello World'));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    let result = worker.exec(task).await.unwrap();

    assert_eq!(result, TerminationReason::Success);

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        String::from_utf8_lossy(response.body.as_bytes().unwrap()),
        "Hello World"
    );
}

#[tokio::test]
async fn test_custom_status() {
    let code = r#"
        addEventListener('fetch', (event) => {
            event.respondWith(new Response('Not Found', { status: 404 }));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 404);
    assert_eq!(
        String::from_utf8_lossy(response.body.as_bytes().unwrap()),
        "Not Found"
    );
}

#[tokio::test]
async fn test_async_handler() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            const message = 'Async Response';
            event.respondWith(new Response(message));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        String::from_utf8_lossy(response.body.as_bytes().unwrap()),
        "Async Response"
    );
}

#[tokio::test]
async fn test_console_log() {
    let code = r#"
        addEventListener('fetch', (event) => {
            console.log('Test log');
            console.warn('Test warning');
            console.error('Test error');
            event.respondWith(new Response('OK'));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn test_empty_body() {
    let code = r#"
        addEventListener('fetch', (event) => {
            event.respondWith(new Response(''));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        String::from_utf8_lossy(response.body.as_bytes().unwrap()),
        ""
    );
}

#[tokio::test]
async fn test_multiple_requests() {
    let code = r#"
        let counter = 0;
        addEventListener('fetch', (event) => {
            counter++;
            event.respondWith(new Response('Request ' + counter));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    // First request
    let req1 = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };
    let (task1, rx1) = Task::fetch(req1);
    worker.exec(task1).await.unwrap();
    let response1 = rx1.await.unwrap();
    assert_eq!(
        String::from_utf8_lossy(response1.body.as_bytes().unwrap()),
        "Request 1"
    );

    // Second request
    let req2 = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };
    let (task2, rx2) = Task::fetch(req2);
    worker.exec(task2).await.unwrap();
    let response2 = rx2.await.unwrap();
    assert_eq!(
        String::from_utf8_lossy(response2.body.as_bytes().unwrap()),
        "Request 2"
    );
}

#[tokio::test]
async fn test_large_body() {
    let code = r#"
        addEventListener('fetch', (event) => {
            const largeText = 'x'.repeat(10000);
            event.respondWith(new Response(largeText));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_bytes().unwrap().len(), 10000);
}

#[tokio::test]
async fn test_json_response() {
    let code = r#"
        addEventListener('fetch', (event) => {
            const data = JSON.stringify({ message: 'Hello', status: 'ok' });
            event.respondWith(new Response(data, { status: 200 }));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    let body = String::from_utf8_lossy(response.body.as_bytes().unwrap());
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["message"], "Hello");
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn test_worker_creation_error() {
    let code = r#"
        this is invalid javascript syntax
    "#;

    let script = Script::new(code);
    let result = Worker::new(script, None, None).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_no_handler_registered() {
    let code = r#"
        // No addEventListener call
        const x = 42;
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, _rx) = Task::fetch(req);
    let result = worker.exec(task).await;

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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        String::from_utf8_lossy(response.body.as_bytes().unwrap()),
        "Promise Response"
    );
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://example.com/test/path".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        String::from_utf8_lossy(response.body.as_bytes().unwrap()),
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 500);
    assert_eq!(
        String::from_utf8_lossy(response.body.as_bytes().unwrap()),
        "Test error"
    );
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    let body = String::from_utf8_lossy(response.body.as_bytes().unwrap());
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let mut headers = HashMap::new();
    headers.insert("authorization".to_string(), "Bearer token123".to_string());
    headers.insert("content-type".to_string(), "application/json".to_string());

    let req = HttpRequest {
        method: "POST".to_string(),
        url: "http://localhost/api".to_string(),
        headers,
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    let body = String::from_utf8_lossy(response.body.as_bytes().unwrap());
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "POST".to_string(),
        url: "http://example.com/api/users".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    assert_eq!(response.status, 200);

    let body = String::from_utf8_lossy(response.body.as_bytes().unwrap());
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

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
