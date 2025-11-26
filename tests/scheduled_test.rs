use openworkers_runtime_boa::{Script, Task, Worker};

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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let (task, rx) = Task::scheduled(1234567890);
    let result = worker.exec(task).await;

    assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
    rx.await.unwrap(); // Should not timeout
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let (task, rx) = Task::scheduled(1234567890);
    let result = worker.exec(task).await;

    assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
    rx.await.unwrap();
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
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let (task, rx) = Task::scheduled(1234567890);
    let result = worker.exec(task).await;

    assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
    rx.await.unwrap();
}
