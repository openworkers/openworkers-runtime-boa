//! The body of a response leaves the worker as the bytes the handler gave,
//! whatever its source.

use openworkers_core::Event;
use openworkers_core::HttpMethod;
use openworkers_core::HttpRequest;
use openworkers_core::HttpResponse;
use openworkers_core::RequestBody;
use openworkers_core::ResponseBody;
use openworkers_core::RuntimeLimits;
use openworkers_core::Script;
use openworkers_core::TerminationReason;
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

async fn respond(response: &str) -> Result<(HttpResponse, Vec<u8>), TerminationReason> {
    let script = format!("addEventListener('fetch', (event) => event.respondWith({response}));");
    let limits = RuntimeLimits {
        max_wall_clock_time_ms: 1000,
        ..Default::default()
    };
    let mut worker = Worker::new(Script::new(script.as_str()), Some(limits))
        .await
        .expect("worker should initialize");

    let (event, receiver) = Event::fetch(HttpRequest {
        method: HttpMethod::Get,
        url: "https://example.com/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    });

    worker.exec(event).await?;

    let mut response = receiver.await.expect("a response");
    let body = std::mem::replace(&mut response.body, ResponseBody::None)
        .collect()
        .await
        .expect("a readable body")
        .unwrap_or_default();

    Ok((response, body.to_vec()))
}

#[tokio::test]
async fn a_stream_with_a_pull_source_arrives_whole() {
    let (_, body) = respond(
        "(() => {
            let n = 0;

            return new Response(new ReadableStream({
                async pull(controller) {
                    await null;

                    if (n++ < 500) {
                        controller.enqueue(new TextEncoder().encode('x'));
                    } else {
                        controller.close();
                    }
                },
            }));
        })()",
    )
    .await
    .unwrap();

    assert_eq!(body, vec![b'x'; 500]);
}

#[tokio::test]
async fn bytes_that_are_not_utf8_arrive_unchanged() {
    let (_, body) = respond("new Response(new Uint8Array([0, 255, 128]))")
        .await
        .unwrap();

    assert_eq!(body, [0, 255, 128]);
}

#[tokio::test]
async fn the_status_and_the_headers_stay_with_the_body() {
    let (response, body) =
        respond("new Response('gone', { status: 410, headers: { 'x-a': '1' } })")
            .await
            .unwrap();

    assert_eq!(response.status, 410);
    assert!(
        response
            .headers
            .contains(&("x-a".to_string(), "1".to_string()))
    );
    assert_eq!(body, b"gone");
}

#[tokio::test]
async fn no_body_is_none() {
    let (response, body) = respond("new Response(null, { status: 204 })")
        .await
        .unwrap();

    assert_eq!(response.status, 204);
    assert!(body.is_empty());
}

#[tokio::test]
async fn a_stream_that_errors_is_a_500() {
    let (response, _) =
        respond("new Response(new ReadableStream({ pull() { throw new Error('broken'); } }))")
            .await
            .unwrap();

    assert_eq!(response.status, 500);
}

#[tokio::test]
async fn a_stream_that_never_ends_meets_the_wall_clock_limit() {
    let result = respond(
        "new Response(new ReadableStream({
            async pull(controller) {
                await null;
                controller.enqueue(new TextEncoder().encode('x'));
            },
        }))",
    )
    .await;

    assert!(
        matches!(result, Err(TerminationReason::WallClockTimeout)),
        "{:?}",
        result.map(|(response, body)| (response.status, body.len()))
    );
}
