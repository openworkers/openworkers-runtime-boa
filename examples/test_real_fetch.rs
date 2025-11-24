use openworkers_runtime_boa::{HttpRequest, Script, Task, Worker};
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/test".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    println!("=== Testing real fetch() ===\n");

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    println!("\n=== Response ===");
    println!("Status: {}", response.status);
    if let Some(body) = response.body {
        let body_str = String::from_utf8_lossy(&body);
        println!("Body length: {}", body_str.len());
        if body_str.contains("slideshow") {
            println!("✅ Real fetch worked! Got JSON from httpbin");
        } else {
            println!("Body preview: {}", &body_str[..body_str.len().min(200)]);
        }
    }
}
