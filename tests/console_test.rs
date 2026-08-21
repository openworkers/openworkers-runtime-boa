use openworkers_core::{
    Event, HttpMethod, HttpRequest, LogLevel, OperationsHandler, RequestBody, Script,
};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct CollectingOps {
    logs: Mutex<Vec<(LogLevel, String)>>,
}

impl OperationsHandler for CollectingOps {
    fn handle_log(&self, level: LogLevel, message: String) {
        self.logs.lock().unwrap().push((level, message));
    }
}

/// Returns the logs the script produced and the body and status it responded with.
async fn run(script: &str) -> (Vec<(LogLevel, String)>, u16, String) {
    let ops = Arc::new(CollectingOps::default());
    let mut worker = Worker::new_with_ops(Script::new(script), None, ops.clone())
        .await
        .expect("worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("task should execute");

    let response = rx.await.expect("should receive response");
    let body = response
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap_or_default();
    let logs = ops.logs.lock().unwrap().clone();

    (
        logs,
        response.status,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

#[tokio::test]
async fn test_console_reaches_the_operations_handler() {
    let (logs, _, _) = run(r#"
        addEventListener('fetch', (event) => {
            console.log('plain', 1);
            console.warn('careful');
            console.error('boom');
            event.respondWith(new Response('ok'));
        });
    "#)
    .await;

    assert_eq!(
        logs,
        vec![
            (LogLevel::Log, "plain 1".to_string()),
            (LogLevel::Warn, "careful".to_string()),
            (LogLevel::Error, "boom".to_string()),
        ]
    );
}

#[tokio::test]
async fn test_thrown_handler_error_is_logged_not_served() {
    let (logs, status, body) = run(r#"
        addEventListener('fetch', () => { throw new Error('db://user:hunter2@host'); });
    "#)
    .await;

    assert_eq!(status, 500);
    assert_eq!(body, "Internal Server Error");
    assert!(logs[0].1.contains("hunter2"), "{:?}", logs);
}

#[tokio::test]
async fn test_rejected_respond_with_is_logged_not_served() {
    let (logs, status, body) = run(r#"
        addEventListener('fetch', (event) => {
            event.respondWith(Promise.reject(new Error('db://user:hunter2@host')));
        });
    "#)
    .await;

    assert_eq!(status, 500);
    assert_eq!(body, "Internal Server Error");
    assert!(logs[0].1.contains("hunter2"), "{:?}", logs);
}
