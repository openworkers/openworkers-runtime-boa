use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

#[tokio::main]
async fn main() {
    let code = r#"
        addEventListener('fetch', (event) => {
            const { pathname } = new URL(event.request.url);
            const message = `Hello from Boa! Path: ${pathname}`;
            event.respondWith(new Response(message, { status: 200 }));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/test".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    println!("Status: {}", response.status);

    if let Some(body) = response.body.collect().await {
        println!("Body: {}", String::from_utf8_lossy(&body));
    }
}
