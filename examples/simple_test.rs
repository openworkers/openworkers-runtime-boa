use openworkers_runtime_boa::{HttpRequest, Script, Task, Worker};
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/test".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    println!("Status: {}", response.status);
    if let Some(body) = response.body {
        println!("Body: {}", String::from_utf8_lossy(&body));
    }
}
