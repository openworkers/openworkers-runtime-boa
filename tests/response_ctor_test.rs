//! Response has to be constructible from every place a handler can reach it.

use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

async fn respond(code: &str) -> (u16, String) {
    let mut worker = Worker::new(Script::new(code), None).await.unwrap();

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("task should execute");

    let response = rx.await.expect("should receive response");
    let status = response.status;
    let body = response.body.collect().await.unwrap_or_default();

    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn test_handler_without_response() {
    let (status, body) = respond(
        r#"
        addEventListener('fetch', function(event) {
            globalThis.handlerCalled = true;
        });
    "#,
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body, "");
}

#[tokio::test]
async fn test_response_in_user_script() {
    let (status, body) = respond(
        r#"
        var greeting = new Response('hello from script');

        addEventListener('fetch', function(event) {
            event.respondWith(greeting);
        });
    "#,
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body, "hello from script");
}

#[tokio::test]
async fn test_response_without_body() {
    let (status, body) = respond(
        r#"
        addEventListener('fetch', function(event) {
            event.respondWith(new Response(null, { status: 204 }));
        });
    "#,
    )
    .await;

    assert_eq!(status, 204);
    assert_eq!(body, "");
}

#[tokio::test]
async fn test_response_built_in_the_handler() {
    let (status, body) = respond(
        r#"
        addEventListener('fetch', function(event) {
            event.respondWith(new Response('built here', { status: 201 }));
        });
    "#,
    )
    .await;

    assert_eq!(status, 201);
    assert_eq!(body, "built here");
}
