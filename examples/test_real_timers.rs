use openworkers_runtime_boa::{Script, Worker};
use std::time::Instant;

#[tokio::main]
async fn main() {
    env_logger::init();

    let code = r#"
        globalThis.timeoutRan = false;
        globalThis.timeoutValue = 0;

        addEventListener('fetch', (event) => {
            console.log('Fetch handler called');
            
            setTimeout(() => {
                console.log('Timeout executed!');
                globalThis.timeoutRan = true;
                globalThis.timeoutValue = 42;
            }, 100);  // 100ms delay

            event.respondWith(new Response('Waiting for timeout...'));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    println!("=== Testing Real Async Timers in Boa ===\n");

    use openworkers_runtime_boa::{HttpRequest, Task};
    use std::collections::HashMap;

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let start = Instant::now();
    let (task, _rx) = Task::fetch(req);

    worker.exec(task).await.unwrap();

    let elapsed = start.elapsed();

    println!("✅ Fetch completed in {:?}", elapsed);
    println!("   Note: If setTimeout works properly, it should have added ~100ms");

    if elapsed.as_millis() >= 100 {
        println!("   ✅ Delay detected! setTimeout is working asynchronously!");
    } else {
        println!("   ⚠️  No significant delay - setTimeout may not be fully async yet");
    }
}
