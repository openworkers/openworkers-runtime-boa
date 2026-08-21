use openworkers_core::{
    Event, HttpMethod, HttpRequest, RequestBody, RuntimeLimits, Script, TerminationReason,
};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;
use std::time::{Duration, Instant};

async fn get(worker: &mut Worker, url: &str) -> String {
    let req = HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let body = rx
        .await
        .unwrap()
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap_or_default();
    String::from_utf8_lossy(&body).into_owned()
}

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
    let body = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
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
    let body = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
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
    let body = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
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
    let body = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&body), "hello:world");
}

#[tokio::test]
async fn test_pending_timer_does_not_hold_the_response() {
    let script = Script::new(
        r#"
        globalThis.late = 'no';

        addEventListener('fetch', (event) => {
            if (event.request.url.endsWith('/late')) {
                event.respondWith(new Response(globalThis.late));
                return;
            }

            setTimeout(function() { globalThis.late = 'yes'; }, 1500);
            setInterval(function() { globalThis.late = 'yes'; }, 10);
            event.respondWith(new Response('now'));
        });
    "#,
    );

    let mut worker = Worker::new(script, None).await.unwrap();

    let start = Instant::now();
    assert_eq!(get(&mut worker, "http://localhost/").await, "now");
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_millis(500), "took {:?}", elapsed);
    assert_eq!(get(&mut worker, "http://localhost/late").await, "no");
}

#[tokio::test]
async fn test_wall_clock_budget_ends_a_stalled_handler() {
    let script = Script::new(
        r#"
        addEventListener('fetch', async (event) => {
            await new Promise(function(resolve) { setTimeout(resolve, 5000); });
            event.respondWith(new Response('late'));
        });
    "#,
    );

    let limits = RuntimeLimits {
        max_wall_clock_time_ms: 100,
        ..RuntimeLimits::default()
    };

    let mut worker = Worker::new(script, Some(limits)).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, _rx) = Event::fetch(req);
    let start = Instant::now();

    assert_eq!(
        worker.exec(event).await,
        Err(TerminationReason::WallClockTimeout)
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}
