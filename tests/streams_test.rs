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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "isStream: true");
}

#[tokio::test]
async fn test_readable_stream_read_chunks() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            var stream = new ReadableStream({
                start: function(controller) {
                    controller.enqueue('chunk1');
                    controller.enqueue('chunk2');
                    controller.enqueue('chunk3');
                    controller.close();
                }
            });

            var reader = stream.getReader();
            var chunks = 0;

            while (true) {
                var r = await reader.read();
                if (r.done) break;
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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "chunks: 3");
}

#[tokio::test]
async fn test_readable_stream_cancel() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            var cancelled = false;

            var stream = new ReadableStream({
                start: function(controller) {
                    controller.enqueue('data');
                },
                cancel: function(reason) {
                    cancelled = true;
                }
            });

            var reader = stream.getReader();
            await reader.cancel('done');
            event.respondWith(new Response('cancelled: ' + cancelled));
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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "cancelled: true");
}

#[tokio::test]
async fn test_readable_stream_error() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            var stream = new ReadableStream({
                start: function(controller) {
                    controller.error(new Error('test error'));
                }
            });

            var reader = stream.getReader();
            var caught = false;

            try {
                await reader.read();
            } catch (e) {
                caught = true;
            }

            event.respondWith(new Response('caught: ' + caught));
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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "caught: true");
}

#[tokio::test]
async fn test_readable_stream_as_response_body() {
    // Response constructed with a ReadableStream body
    let code = r#"
        addEventListener('fetch', async (event) => {
            var stream = new ReadableStream({
                start: function(controller) {
                    var enc = new TextEncoder();
                    controller.enqueue(enc.encode('Hello '));
                    controller.enqueue(enc.encode('Stream'));
                    controller.close();
                }
            });

            var resp = new Response(stream);

            // Read the response body back
            var reader = resp.body.getReader();
            var result = '';

            while (true) {
                var r = await reader.read();
                if (r.done) break;
                result += new TextDecoder().decode(r.value);
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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&body_bytes), "Hello Stream");
}

#[tokio::test]
async fn test_readable_stream_controller_desired_size() {
    let code = r#"
        addEventListener('fetch', (event) => {
            var sizes = [];
            var stream = new ReadableStream({
                start: function(controller) {
                    sizes.push(controller.desiredSize);
                    controller.enqueue('a');
                    sizes.push(controller.desiredSize);
                    controller.enqueue('b');
                    sizes.push(controller.desiredSize);
                    controller.close();
                }
            });
            event.respondWith(new Response(sizes.join(',')));
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
    let body_bytes = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
    // desiredSize: 1 (empty), 0 (1 item), 0 (2 items, clamped)
    assert_eq!(String::from_utf8_lossy(&body_bytes), "1,0,0");
}

#[tokio::test]
async fn test_streaming_request_body_is_rejected() {
    let code = r#"
        addEventListener('fetch', (event) => {
            event.respondWith(new Response('unreachable'));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(Ok(bytes::Bytes::from_static(b"chunk")))
        .await
        .unwrap();
    drop(tx);

    let req = HttpRequest {
        method: HttpMethod::Post,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::Stream(rx),
    };

    let (event, _res_rx) = Event::fetch(req);
    let err = worker
        .exec(event)
        .await
        .expect_err("should refuse a stream");

    assert!(
        err.to_string().contains("Streaming request bodies"),
        "unexpected error: {}",
        err
    );
}
