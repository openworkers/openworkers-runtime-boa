use openworkers_core::{Event, Script};
use openworkers_runtime_boa::Worker;

#[tokio::test]
async fn test_scheduled_event() {
    let code = r#"
        let scheduledExecuted = false;
        let scheduledTime = 0;

        addEventListener("scheduled", (event) => {
            scheduledExecuted = true;
            scheduledTime = event.scheduledTime;
            console.log(`Scheduled event at ${scheduledTime}`);
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let (event, rx) = Event::from_schedule("test-task-1".to_string(), 1234567890);
    let result = worker.exec(event).await;

    assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
    let task_result = rx.await.unwrap();
    assert!(task_result.success);
}

#[tokio::test]
async fn test_scheduled_with_waituntil() {
    let code = r#"
        addEventListener("scheduled", (event) => {
            const promise = new Promise((resolve) => {
                setTimeout(() => {
                    console.log('Async work completed');
                    resolve();
                }, 10);
            });
            event.waitUntil(promise);
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let (event, rx) = Event::from_schedule("test-task-2".to_string(), 1234567890);
    let result = worker.exec(event).await;

    assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
    let task_result = rx.await.unwrap();
    assert!(task_result.success);
}

#[tokio::test]
async fn test_scheduled_async_handler() {
    let code = r#"
        addEventListener("scheduled", async (event) => {
            console.log('Starting async scheduled handler');
            await new Promise(resolve => setTimeout(resolve, 10));
            console.log('Async scheduled handler completed');
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let (event, rx) = Event::from_schedule("test-task-3".to_string(), 1234567890);
    let result = worker.exec(event).await;

    assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
    let task_result = rx.await.unwrap();
    assert!(task_result.success);
}

#[tokio::test]
async fn test_removed_scheduled_handler_does_not_run() {
    let code = r#"
        globalThis.ran = 0;
        const handler = () => { globalThis.ran += 1; };

        addEventListener("scheduled", handler);
        addEventListener("scheduled", () => { globalThis.ran += 10; });
        removeEventListener("scheduled", handler);

        addEventListener("fetch", (event) => {
            event.respondWith(new Response(String(globalThis.ran)));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let (event, rx) = Event::from_schedule("test-task-4".to_string(), 1234567890);
    worker.exec(event).await.unwrap();
    assert!(rx.await.unwrap().success);

    let req = openworkers_core::HttpRequest {
        method: openworkers_core::HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: std::collections::HashMap::new(),
        body: openworkers_core::RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();

    let body = rx.await.unwrap().body.collect().await.unwrap();
    assert_eq!(String::from_utf8_lossy(&body), "10");
}
