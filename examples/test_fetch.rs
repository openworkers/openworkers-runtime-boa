use openworkers_runtime_boa::{Script, Worker};

#[tokio::main]
async fn main() {
    env_logger::init();

    let code = r#"
        addEventListener('fetch', async (event) => {
            console.log('Making HTTP request...');
            
            const response = await fetch('https://httpbin.org/json');
            const data = await response.json();
            
            console.log('Received data:', JSON.stringify(data));
            
            event.respondWith(new Response(JSON.stringify({
                message: 'Fetched successfully!',
                slideshow: data.slideshow
            }), {
                status: 200,
                headers: { 'Content-Type': 'application/json' }
            }));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    println!("=== Testing fetch() API in Boa ===\n");

    use openworkers_runtime_boa::{HttpRequest, Task};
    use std::collections::HashMap;

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/test".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);

    println!("Executing worker with fetch...");
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();

    println!("\n✅ Response status: {}", response.status);
    if let Some(body) = response.body {
        let body_str = String::from_utf8_lossy(&body);
        println!(
            "✅ Response body preview: {}...",
            &body_str[..body_str.len().min(100)]
        );
    }
    println!("\n🎉 fetch() API works in Boa!");
}
