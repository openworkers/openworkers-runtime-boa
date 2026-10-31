//! The wall-clock limit ends a request whose promises never stop queuing more.

use openworkers_core::Event;
use openworkers_core::HttpMethod;
use openworkers_core::HttpRequest;
use openworkers_core::RequestBody;
use openworkers_core::RuntimeLimits;
use openworkers_core::Script;
use openworkers_core::TerminationReason;
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

#[tokio::test]
async fn a_chain_of_promises_that_never_ends_meets_the_limit() {
    let script = "addEventListener('fetch', (event) => {
        const spin = () => Promise.resolve().then(spin);

        spin();
        event.respondWith(new Promise(() => {}));
    });";
    let limits = RuntimeLimits {
        max_wall_clock_time_ms: 200,
        ..Default::default()
    };
    let mut worker = Worker::new(Script::new(script), Some(limits))
        .await
        .expect("worker should initialize");

    let (event, _receiver) = Event::fetch(HttpRequest {
        method: HttpMethod::Get,
        url: "https://example.com/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    });

    let started = Instant::now();
    let result = worker.exec(event).await;

    assert!(
        matches!(result, Err(TerminationReason::WallClockTimeout)),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}
