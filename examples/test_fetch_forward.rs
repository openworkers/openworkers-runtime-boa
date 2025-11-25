use openworkers_runtime_boa::{HttpRequest, Script, Task, Worker};
use std::collections::HashMap;

#[tokio::main]
async fn main() {
    env_logger::init();

    // Worker code from openworkers runner
    let code = r#"
addEventListener("fetch", (event) => {
  event.respondWith(
    handleRequest(event.request).catch(
      (err) => new Response(err.message || String(err), { status: 500 })
    )
  );
});

async function handleRequest(request) {
  const { pathname } = new URL(request.url);

  console.log('Pathname:', pathname);

  if (pathname.startsWith("/test")) {
    console.log('Fetching from NPM registry...');
    return fetch('https://registry.npmjs.org/@openworkers%2fapi-types');
  }

  return new Response("<h3>Hello world!</h3>", {
    headers: { "Content-Type": "text/html" },
  });
}
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    println!("=== Testing /test route with fetch forward ===\n");

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/test".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);

    match worker.exec(task).await {
        Ok(_) => match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(response)) => {
                println!("Status: {}", response.status);
                if let Some(body) = response.body.as_bytes() {
                    let body_str = String::from_utf8_lossy(body);
                    println!("Body length: {}", body_str.len());
                    println!("Body preview: {}", &body_str[..body_str.len().min(100)]);
                }
            }
            Ok(Err(e)) => {
                println!("Error receiving response: {:?}", e);
            }
            Err(_) => {
                println!("❌ TIMEOUT waiting for response!");
            }
        },
        Err(e) => {
            println!("Error executing worker: {}", e);
        }
    }
}
