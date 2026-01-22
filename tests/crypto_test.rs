use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;

#[tokio::test]
async fn test_crypto_random_uuid() {
    let script = r#"
        addEventListener('fetch', (event) => {
            const uuid = crypto.randomUUID();
            // UUID v4 format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
            const isValid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(uuid);
            event.respondWith(new Response(JSON.stringify({
                uuid: uuid,
                isValid: isValid,
                length: uuid.length
            })));
        });
    "#;

    let script_obj = Script::new(script);
    let mut worker = Worker::new(script_obj, None)
        .await
        .expect("Worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["isValid"], true);
    assert_eq!(json["length"], 36);
}

#[tokio::test]
async fn test_crypto_get_random_values() {
    let script = r#"
        addEventListener('fetch', (event) => {
            const array = new Uint8Array(16);

            // Check all zeros initially
            const allZerosBefore = array.every(b => b === 0);

            // Fill with random values
            const result = crypto.getRandomValues(array);

            // Check that we got the same array back
            const sameArray = result === array;

            // Check that at least some values are non-zero (very unlikely all zeros)
            const hasNonZero = array.some(b => b !== 0);

            event.respondWith(new Response(JSON.stringify({
                allZerosBefore: allZerosBefore,
                sameArray: sameArray,
                hasNonZero: hasNonZero,
                length: array.length
            })));
        });
    "#;

    let script_obj = Script::new(script);
    let mut worker = Worker::new(script_obj, None)
        .await
        .expect("Worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["allZerosBefore"], true);
    assert_eq!(json["sameArray"], true);
    assert_eq!(json["hasNonZero"], true);
    assert_eq!(json["length"], 16);
}

#[tokio::test]
async fn test_crypto_subtle_digest_sha256() {
    let script = r#"
        addEventListener('fetch', (event) => {
            // "hello" as bytes: [104, 101, 108, 108, 111]
            const data = new Uint8Array([104, 101, 108, 108, 111]);

            // Use native function directly (sync) - returns hex string
            const hashHex = crypto.subtle.__nativeDigest('SHA-256', data);

            // Known SHA-256 of "hello"
            const expected = '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824';

            event.respondWith(new Response(JSON.stringify({
                hash: hashHex,
                expected: expected,
                matches: hashHex === expected,
                length: hashHex.length / 2
            })));
        });
    "#;

    let script_obj = Script::new(script);
    let mut worker = Worker::new(script_obj, None)
        .await
        .expect("Worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["matches"], true);
    assert_eq!(json["length"], 32); // SHA-256 is 32 bytes
}

#[tokio::test]
async fn test_crypto_subtle_digest_sha1() {
    let script = r#"
        addEventListener('fetch', (event) => {
            // "hello" as bytes: [104, 101, 108, 108, 111]
            const data = new Uint8Array([104, 101, 108, 108, 111]);

            // Use native function directly (sync) - returns hex string
            const hashHex = crypto.subtle.__nativeDigest('SHA-1', data);

            // Known SHA-1 of "hello"
            const expected = 'aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d';

            event.respondWith(new Response(JSON.stringify({
                hash: hashHex,
                expected: expected,
                matches: hashHex === expected,
                length: hashHex.length / 2
            })));
        });
    "#;

    let script_obj = Script::new(script);
    let mut worker = Worker::new(script_obj, None)
        .await
        .expect("Worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["matches"], true);
    assert_eq!(json["length"], 20); // SHA-1 is 20 bytes
}

#[tokio::test]
async fn test_crypto_subtle_digest_sha512() {
    let script = r#"
        addEventListener('fetch', (event) => {
            // "hello" as bytes: [104, 101, 108, 108, 111]
            const data = new Uint8Array([104, 101, 108, 108, 111]);

            // Use native function directly (sync)
            const hexResult = crypto.subtle.__nativeDigest('SHA-512', data);

            // SHA-512 produces 128 hex chars = 64 bytes
            const len = hexResult.length / 2;

            event.respondWith(new Response(JSON.stringify({
                length: len
            })));
        });
    "#;

    let script_obj = Script::new(script);
    let mut worker = Worker::new(script_obj, None)
        .await
        .expect("Worker should initialize");

    let request = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (task, rx) = Event::fetch(request);
    worker.exec(task).await.expect("Task should execute");

    let response = rx.await.expect("Should receive response");
    assert_eq!(response.status, 200);

    let body = response.body.collect().await.expect("Should have body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("Should be valid JSON");
    assert_eq!(json["length"], 64); // SHA-512 is 64 bytes
}
