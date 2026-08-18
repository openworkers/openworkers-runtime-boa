use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

/// Run `body` inside a fetch handler and return what it returned, awaited, or
/// the error it threw.
pub async fn eval(body: &str) -> String {
    let script = format!(
        r#"addEventListener('fetch', (event) => {{
            event.respondWith((async function() {{
                let out;

                try {{
                    out = String(await (async function() {{ {} }})());
                }} catch (e) {{
                    out = 'threw ' + e;
                }}

                return new Response(out);
            }})());
        }});"#,
        body
    );

    let mut worker = Worker::new(Script::new(script.as_str()), None)
        .await
        .expect("worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "https://example.com/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("task should execute");

    let response = rx.await.expect("should receive response");
    let body = response.body.collect().await.expect("should have body");

    String::from_utf8_lossy(&body).into_owned()
}
