use openworkers_core::Script;
use openworkers_runtime_boa::Worker;

#[tokio::test]
async fn test_settimeout_basic() {
    let script = r#"
        globalThis.timeoutRan = false;

        setTimeout(() => {
            globalThis.timeoutRan = true;
        }, 10);
    "#;

    let script_obj = Script::new(script);
    let _worker = Worker::new(script_obj, None)
        .await
        .expect("Worker should initialize");

    // Give Boa a moment to process promises
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Note: Without proper async integration, this test is limited
    // This demonstrates that setTimeout is defined, even if not fully functional
    println!("setTimeout test completed - basic API is available");
}
