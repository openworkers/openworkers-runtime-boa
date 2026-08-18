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

async fn run(script: &str) -> Vec<(LogLevel, String)> {
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
    let _ = rx.await;

    let logs = ops.logs.lock().unwrap();
    logs.clone()
}

#[tokio::test]
async fn test_console_reaches_the_operations_handler() {
    let logs = run(r#"
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
