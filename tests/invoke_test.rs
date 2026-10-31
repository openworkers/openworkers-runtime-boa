#![cfg(feature = "invoke")]

use openworkers_core::BindingInfo;
use openworkers_core::BindingType;
use openworkers_core::Event;
use openworkers_core::HttpMethod;
use openworkers_core::HttpRequest;
use openworkers_core::InvokeResult;
use openworkers_core::LogLevel;
use openworkers_core::OpFuture;
use openworkers_core::OperationsHandler;
use openworkers_core::RequestBody;
use openworkers_core::Script;
use openworkers_runtime_boa::Worker;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// `fail` answers an error; any other command echoes what the host got.
struct Host;

impl OperationsHandler for Host {
    fn handle_log(&self, _level: LogLevel, _message: String) {}

    fn handle_binding_invoke(
        &self,
        binding: &str,
        command: &str,
        args: Value,
    ) -> OpFuture<'_, InvokeResult> {
        let answer = match command {
            "fail" => InvokeResult::Error("it failed".to_string()),
            _ => InvokeResult::Value(json!({
                "binding": binding,
                "command": command,
                "args": args,
            })),
        };

        Box::pin(async move { answer })
    }
}

/// Run `body` against a worker holding an invoke binding named RUST.
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

    script.bindings = vec![BindingInfo::invoke("RUST")];

    let mut worker = Worker::new_with_ops(script, None, Arc::new(Host))
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
async fn a_call_reaches_the_host_and_the_answer_comes_back() {
    let out = eval("return JSON.stringify(await env.RUST.invoke('add', { a: 1, b: [2] }));").await;
    let out: Value = serde_json::from_str(&out).expect(&out);

    assert_eq!(
        out,
        json!({ "binding": "RUST", "command": "add", "args": { "a": 1, "b": [2] } })
    );
}

#[tokio::test]
async fn no_args_is_null() {
    let out = eval("return JSON.stringify((await env.RUST.invoke('ping')).args);").await;

    assert_eq!(out, "null");
}

#[tokio::test]
async fn an_error_of_the_host_rejects_with_its_message() {
    let out = eval("return await env.RUST.invoke('fail');").await;

    assert_eq!(out, "threw Error: it failed");
}

#[tokio::test]
async fn a_command_that_is_not_a_string_rejects() {
    let out = eval(
        "const promise = env.RUST.invoke(1);
         return (promise instanceof Promise) + ' ' + await promise.catch((e) => e.name);",
    )
    .await;

    assert_eq!(out, "true TypeError");
}

#[tokio::test]
async fn args_that_are_not_json_reject() {
    let out =
        eval("return await env.RUST.invoke('x', 1n).catch((e) => 'rejected ' + e.name);").await;

    assert_eq!(out, "rejected TypeError");
}

#[test]
fn invoke_is_supported() {
    assert!(openworkers_runtime_boa::SUPPORTED_BINDINGS.contains(&BindingType::Invoke));
}
