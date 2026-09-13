use openworkers_core::BindingInfo;
use openworkers_core::BindingType;
use openworkers_core::Event;
use openworkers_core::HttpMethod;
use openworkers_core::HttpRequest;
use openworkers_core::KvOp;
use openworkers_core::KvResult;
use openworkers_core::LogLevel;
use openworkers_core::OpFuture;
use openworkers_core::OperationsHandler;
use openworkers_core::RequestBody;
use openworkers_core::Script;
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;
use std::sync::Arc;

/// Answers the KV op by echoing what it was asked, so a test can assert the
/// call reached the host with the arguments the guest gave.
struct Kv;

impl OperationsHandler for Kv {
    fn handle_log(&self, _level: LogLevel, _message: String) {}

    fn handle_binding_kv(&self, binding: &str, op: KvOp) -> OpFuture<'_, KvResult> {
        let answer = match op {
            KvOp::Get { key } => match key.as_str() {
                "absent" => KvResult::Value(None),
                _ => KvResult::Value(Some(serde_json::json!({
                    "binding": binding,
                    "key": key,
                }))),
            },
            KvOp::Put {
                key,
                value,
                expires_in,
            } => KvResult::Value(Some(serde_json::json!({
                "key": key,
                "value": value,
                "expiresIn": expires_in,
            }))),
            KvOp::Delete { .. } => KvResult::Ok,
            KvOp::List { prefix, limit } => KvResult::Keys(vec![
                prefix.unwrap_or_else(|| "none".to_string()),
                limit
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            ]),
        };

        Box::pin(async move { answer })
    }
}

/// Run `body` against a worker holding one KV binding named CACHE.
async fn eval(body: &str) -> String {
    let script = format!(
        r#"addEventListener('fetch', (event) => {{
            event.respondWith((async function() {{
                let out;

                try {{
                    out = String(await (async function() {{ {body} }})());
                }} catch (e) {{
                    out = 'threw ' + e;
                }}

                return new Response(out);
            }})());
        }});"#
    );

    let mut script = Script::new(script.as_str());

    script.bindings = vec![BindingInfo {
        name: "CACHE".to_string(),
        binding_type: BindingType::Kv,
    }];
    script.env = Some(HashMap::from([(
        "GREETING".to_string(),
        "hello".to_string(),
    )]));

    let mut worker = Worker::new_with_ops(script, None, Arc::new(Kv))
        .await
        .expect("worker should initialize");

    let (event, receiver) = Event::fetch(HttpRequest {
        method: HttpMethod::Get,
        url: "https://example.com/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    });

    worker.exec(event).await.expect("worker should run");

    let response = receiver.await.expect("a response");
    let body = response
        .body
        .collect()
        .await
        .expect("a readable body")
        .unwrap_or_default();

    String::from_utf8_lossy(&body).to_string()
}

#[tokio::test]
async fn env_carries_the_variables() {
    assert_eq!(eval("return env.GREETING;").await, "hello");
}

#[tokio::test]
async fn get_reaches_the_host_with_its_key() {
    let out = eval("const v = await env.CACHE.get('k'); return v.binding + '/' + v.key;").await;

    assert_eq!(out, "CACHE/k");
}

#[tokio::test]
async fn a_missing_key_is_null() {
    assert_eq!(eval("return await env.CACHE.get('absent');").await, "null");
}

#[tokio::test]
async fn put_carries_the_value_and_its_ttl() {
    let out = eval(
        "const v = await env.CACHE.put('k', { a: 1 }, { expirationTtl: 60 });
         return v.key + '/' + v.value.a + '/' + v.expiresIn;",
    )
    .await;

    assert_eq!(out, "k/1/60");
}

#[tokio::test]
async fn put_without_options_has_no_ttl() {
    let out = eval("const v = await env.CACHE.put('k', 'x'); return String(v.expiresIn);").await;

    assert_eq!(out, "null");
}

#[tokio::test]
async fn delete_resolves_with_nothing() {
    assert_eq!(
        eval("return String(await env.CACHE.delete('k'));").await,
        "undefined"
    );
}

#[tokio::test]
async fn list_passes_its_prefix_and_limit() {
    let out = eval("return (await env.CACHE.list({ prefix: 'a/', limit: 5 })).join(',');").await;

    assert_eq!(out, "a/,5");
}

#[tokio::test]
async fn list_without_options_asks_for_neither() {
    assert_eq!(
        eval("return (await env.CACHE.list()).join(',');").await,
        "none,none"
    );
}

#[tokio::test]
async fn a_binding_the_worker_did_not_declare_is_absent() {
    assert_eq!(eval("return String(env.MISSING);").await, "undefined");
}
