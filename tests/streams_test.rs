use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

#[tokio::test]
async fn test_readable_stream_creation() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            const stream = new ReadableStream({
                start(controller) {
                    controller.enqueue('Hello ');
                    controller.enqueue('World');
                    controller.close();
                }
            });

            // Read from stream manually
            const reader = stream.getReader();
            let result = '';
            while (true) {
                const { done, value } = await reader.read();
                if (done) break;
                result += value;
            }
            event.respondWith(new Response(result));
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
    assert_eq!(String::from_utf8_lossy(&body_bytes), "Hello World");
}

#[tokio::test]
async fn test_readable_stream_locked() {
    let code = r#"
        addEventListener('fetch', (event) => {
            const stream = new ReadableStream({
                start(controller) {
                    controller.enqueue('test');
                    controller.close();
                }
            });

            const reader = stream.getReader();
            const isLocked = stream.locked;

            event.respondWith(new Response('locked: ' + isLocked));
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
    let body_bytes = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "locked: true");
}

#[tokio::test]
async fn test_response_body_is_readable_stream() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            const response = new Response('Hello');
            const body = response.body;
            const isStream = body instanceof ReadableStream;
            event.respondWith(new Response('isStream: ' + isStream));
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
    let body_bytes = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "isStream: true");
}

#[tokio::test]
async fn test_readable_stream_read_chunks() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            const stream = new ReadableStream({
                start(controller) {
                    controller.enqueue('chunk1');
                    controller.enqueue('chunk2');
                    controller.enqueue('chunk3');
                    controller.close();
                }
            });

            const reader = stream.getReader();
            let chunks = 0;

            while (true) {
                const { done, value } = await reader.read();
                if (done) break;
                chunks++;
            }

            event.respondWith(new Response('chunks: ' + chunks));
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
    let body_bytes = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "chunks: 3");
}
