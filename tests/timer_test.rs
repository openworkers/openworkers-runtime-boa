use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

#[tokio::test]
async fn test_settimeout_basic() {
    let script = Script::new(
        r#"
        addEventListener('fetch', async (event) => {
            var result = 'before';

            await new Promise(function(resolve) {
                setTimeout(function() {
                    result = 'after';
                    resolve();
                }, 10);
            });

            event.respondWith(new Response(result));
        });
    "#,
    );

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
    let body = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body), "after");
}

#[tokio::test]
async fn test_settimeout_zero_delay() {
    let script = Script::new(
        r#"
        addEventListener('fetch', async (event) => {
            var order = [];

            order.push('sync');

            await new Promise(function(resolve) {
                setTimeout(function() {
                    order.push('timeout');
                    resolve();
                }, 0);
            });

            order.push('done');
            event.respondWith(new Response(order.join(',')));
        });
    "#,
    );

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
    let body = response.body.collect().await.unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("timeout"), "Got: {}", body_str);
    assert!(body_str.contains("done"), "Got: {}", body_str);
}

#[tokio::test]
async fn test_clear_timeout() {
    let script = Script::new(
        r#"
        addEventListener('fetch', async (event) => {
            var fired = false;
            var id = setTimeout(function() { fired = true; }, 10);
            clearTimeout(id);

            // Wait a bit to make sure it doesn't fire
            await new Promise(function(resolve) {
                setTimeout(resolve, 20);
            });

            event.respondWith(new Response('fired: ' + fired));
        });
    "#,
    );

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
    let body = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body), "fired: false");
}

#[tokio::test]
async fn test_settimeout_with_args() {
    let script = Script::new(
        r#"
        addEventListener('fetch', async (event) => {
            var result = await new Promise(function(resolve) {
                setTimeout(function(a, b) {
                    resolve(a + ':' + b);
                }, 5, 'hello', 'world');
            });

            event.respondWith(new Response(result));
        });
    "#,
    );

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
    let body = response.body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body), "hello:world");
}
