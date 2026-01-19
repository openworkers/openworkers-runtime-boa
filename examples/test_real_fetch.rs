use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

#[tokio::main]
async fn main() {
    env_logger::init();

    let code = r#"
addEventListener('fetch', async (event) => {
    console.log('Fetching from httpbin.org...');

    try {
        const response = await fetch('https://httpbin.org/json');
        const data = await response.text();

        console.log('Fetch successful!');
        console.log('Status:', response.status);
        console.log('Data length:', data.length);

        event.respondWith(
            new Response(data, { status: response.status })
        );
    } catch (err) {
        console.error('Fetch error:', err.message);
        event.respondWith(
            new Response(`Fetch failed: ${err.message}`, { status: 500 })
        );
    }
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

    println!("=== Testing real fetch() ===\n");

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let response = rx.await.unwrap();
    println!("\n=== Response ===");
    println!("Status: {}", response.status);

    if let Some(body) = response.body.collect().await {
        let body_str = String::from_utf8_lossy(&body);
        println!("Body length: {}", body_str.len());

        if body_str.contains("slideshow") {
            println!("Real fetch worked! Got JSON from httpbin");
        } else {
            println!("Body preview: {}", &body_str[..body_str.len().min(200)]);
        }
    }
}
