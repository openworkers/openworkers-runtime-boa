use openworkers_core::Script;
use openworkers_runtime_boa::Worker;

#[tokio::test]
async fn test_handler_without_response() {
    let script = Script::new(
        r#"
        addEventListener('fetch', function(event) {
            globalThis.__handlerCalled = true;
        });
    "#,
    );
    let mut worker = Worker::new(script, None).await.unwrap();
    let request = openworkers_core::HttpRequest {
        method: openworkers_core::HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: std::collections::HashMap::new(),
        body: openworkers_core::RequestBody::None,
    };
    let (task, _rx) = openworkers_core::Event::fetch(request);
    let result = worker.exec(task).await;
    println!("no Response handler: {:?}", result);
}

#[tokio::test]
async fn test_response_in_user_script() {
    // Create Response during user script eval (not in a handler called from a 2nd eval)
    let script = Script::new(
        r#"
        var testResp = new Response('hello from script');
        console.log('Response created: status=' + testResp.status);
        addEventListener('fetch', function(event) {
            // just set a flag
            globalThis.__test = 1;
        });
    "#,
    );
    let mut worker = Worker::new(script, None).await.unwrap();
    println!("Worker created with Response in user script: OK");

    let request = openworkers_core::HttpRequest {
        method: openworkers_core::HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: std::collections::HashMap::new(),
        body: openworkers_core::RequestBody::None,
    };
    let (task, _rx) = openworkers_core::Event::fetch(request);
    let result = worker.exec(task).await;
    println!("handler result: {:?}", result);
}

#[tokio::test]
async fn test_response_in_handler_no_string_body() {
    // Response with null body (no ReadableStream creation)
    let script = Script::new(
        r#"
        addEventListener('fetch', function(event) {
            event.respondWith(new Response(null, { status: 204 }));
        });
    "#,
    );
    let mut worker = Worker::new(script, None).await.unwrap();
    let request = openworkers_core::HttpRequest {
        method: openworkers_core::HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: std::collections::HashMap::new(),
        body: openworkers_core::RequestBody::None,
    };
    let (task, rx) = openworkers_core::Event::fetch(request);
    let result = worker.exec(task).await;
    println!("null body handler: {:?}", result);
    if let Ok(resp) = rx.await {
        println!("  status: {}", resp.status);
    }
}

#[tokio::test]
async fn test_response_precreated() {
    // Create Response in user script, reuse in handler
    let script = Script::new(
        r#"
        var myResponse = new Response('precreated');
        addEventListener('fetch', function(event) {
            event.respondWith(myResponse);
        });
    "#,
    );
    let mut worker = Worker::new(script, None).await.unwrap();
    let request = openworkers_core::HttpRequest {
        method: openworkers_core::HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: std::collections::HashMap::new(),
        body: openworkers_core::RequestBody::None,
    };
    let (task, rx) = openworkers_core::Event::fetch(request);
    let result = worker.exec(task).await;
    println!("precreated Response handler: {:?}", result);
    if let Ok(resp) = rx.await {
        println!("  status: {}", resp.status);
        if let Some(body) = resp.body.collect().await {
            println!("  body: '{}'", String::from_utf8_lossy(&body));
        }
    }
}
