//! A worker can be driven using only what this crate re-exports.

use openworkers_runtime_boa::snapshot::create_runtime_snapshot;
use openworkers_runtime_boa::{
    Event, HttpMethod, HttpRequest, HttpResponse, OpFuture, OperationsHandler, RequestBody,
    ResponseBody, Script, Worker,
};
use std::collections::HashMap;
use std::sync::Arc;

struct EchoOps;

impl OperationsHandler for EchoOps {
    fn handle_fetch(&self, _request: HttpRequest) -> OpFuture<'_, Result<HttpResponse, String>> {
        Box::pin(async move {
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: ResponseBody::Bytes(bytes::Bytes::from("upstream")),
            })
        })
    }
}

#[tokio::test]
async fn worker_runs_through_crate_reexports() {
    let script = Script::new(
        r#"
        addEventListener('fetch', async (event) => {
            const upstream = await fetch('https://example.com/');
            event.respondWith(new Response(await upstream.text()));
        });
    "#,
    );

    let mut worker = Worker::new_with_ops(script, None, Arc::new(EchoOps))
        .await
        .expect("Worker should initialize");

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(req);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    assert_eq!(String::from_utf8_lossy(&body), "upstream");
}

#[test]
fn snapshots_are_reported_as_unsupported() {
    assert!(create_runtime_snapshot().is_err());
}
