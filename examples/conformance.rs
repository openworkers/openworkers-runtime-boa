//! Runs the SvelteKit conformance suite and diffs against the recorded oracle.
//!
//! cargo run --release --features conformance --example conformance [fixture-dir]

use openworkers_core::{
    Event, HttpMethod, HttpRequest, HttpResponse, LogLevel, OpFuture, OperationsHandler,
    RequestBody, ResponseBody, RuntimeLimits, Script,
};
use openworkers_runtime_boa::Worker;
use openworkers_transform::{CodeLanguage, parse_worker_code};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Bindings are not exposed to JS yet, so `env.ASSETS` is built here instead of
/// coming from `Script::with_bindings`. It 404s like the oracle's binding does.
const ADAPTER: &str = r#"
globalThis.env = {
    ASSETS: {
        fetch: function() { return new Response('', { status: 404 }); }
    }
};

addEventListener('fetch', function(event) {
    var ctx = { waitUntil: function() {}, passThroughOnException: function() {} };
    event.respondWith(globalThis.default.fetch(event.request, globalThis.env, ctx));
});
"#;

#[derive(Deserialize)]
struct Spec {
    bundle: String,
    base_url: String,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    request: RequestSpec,
}

#[derive(Deserialize)]
struct RequestSpec {
    method: String,
    path: String,
    headers: Vec<String>,
    body: Option<String>,
}

#[derive(Deserialize)]
struct Oracle {
    lowered: Lowered,
    scenarios: Vec<Recorded>,
}

#[derive(Deserialize)]
struct Lowered {
    sha256: String,
}

#[derive(Deserialize)]
struct Recorded {
    name: String,
    response: RecordedResponse,
}

#[derive(Deserialize)]
struct RecordedResponse {
    status: u16,
    headers: Vec<String>,
    body_file: String,
    body_sha256: String,
    warm_identical: bool,
}

struct Ops;

impl OperationsHandler for Ops {
    fn handle_binding_fetch(
        &self,
        _binding: &str,
        _request: HttpRequest,
    ) -> OpFuture<'_, Result<HttpResponse, String>> {
        Box::pin(async {
            Ok(HttpResponse {
                status: 404,
                headers: Vec::new(),
                body: ResponseBody::None,
            })
        })
    }

    fn handle_log(&self, level: LogLevel, message: String) {
        println!("    [{level:?}] {message}");
    }
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn dispatch(
    worker: &mut Worker,
    base_url: &str,
    spec: &RequestSpec,
) -> (u16, Vec<String>, Vec<u8>) {
    let headers: HashMap<String, String> = spec
        .headers
        .iter()
        .map(|h| {
            let (name, value) = h.split_once(": ").expect("header must be `name: value`");
            (name.to_string(), value.to_string())
        })
        .collect();

    let req = HttpRequest {
        method: spec.method.parse::<HttpMethod>().expect("bad method"),
        url: format!("{base_url}{}", spec.path),
        headers,
        body: match &spec.body {
            Some(b) => RequestBody::Bytes(b.clone().into_bytes().into()),
            None => RequestBody::None,
        },
    };

    let (task, rx) = Event::fetch(req);
    worker.exec(task).await.expect("exec failed");

    let res = rx.await.expect("no response");
    let body = res
        .body
        .collect()
        .await
        .expect("Should read body")
        .unwrap_or_default();

    let headers = res
        .headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();

    (res.status, headers, body.to_vec())
}

/// A one-line window around the first differing byte, so a 2 KB HTML diff stays
/// readable.
fn first_divergence(expected: &[u8], actual: &[u8]) -> String {
    let at = expected
        .iter()
        .zip(actual)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));

    let from = at.saturating_sub(40);
    let window = |bytes: &[u8]| {
        let to = (at + 60).min(bytes.len());
        String::from_utf8_lossy(&bytes[from.min(bytes.len())..to]).replace('\n', "\\n")
    };

    format!(
        "byte {at} of {} expected / {} actual\n      expected ...{}...\n      actual   ...{}...",
        expected.len(),
        actual.len(),
        window(expected),
        window(actual)
    )
}

fn header_diff(expected: &[String], actual: &[String]) -> Vec<String> {
    let mut out = Vec::new();

    for i in 0..expected.len().max(actual.len()) {
        let e = expected.get(i);
        let a = actual.get(i);

        if e == a {
            continue;
        }

        let short = |h: Option<&String>| match h {
            Some(h) if h.len() > 96 => format!("{}...", &h[..96]),
            Some(h) => h.clone(),
            None => "<none>".to_string(),
        };

        out.push(format!("      [{i}] expected {}", short(e)));
        out.push(format!("           actual   {}", short(a)));
    }

    out
}

#[tokio::main]
async fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../openworkers-conformance/fixtures/sveltekit-app")
        });

    let spec: Spec =
        serde_json::from_slice(&std::fs::read(root.join("scenarios.json")).expect("no scenarios"))
            .expect("bad scenarios.json");

    let oracle: Oracle =
        serde_json::from_slice(&std::fs::read(root.join("oracle.json")).expect("no oracle"))
            .expect("bad oracle.json");

    let bundle = std::fs::read(root.join(&spec.bundle)).expect("no bundle");
    let lowered = parse_worker_code(&bundle, CodeLanguage::JavaScript).expect("transform failed");
    let lowered_sha = sha256(lowered.as_bytes());

    println!(
        "lowered {} bytes sha256 {} ({})",
        lowered.len(),
        lowered_sha,
        if lowered_sha == oracle.lowered.sha256 {
            "same bytes as the oracle"
        } else {
            "DIFFERENT from the oracle"
        }
    );

    let code = format!("{lowered}\n{ADAPTER}");
    let mut passed = 0;
    let mut partial = 0;

    for scenario in &spec.scenarios {
        let recorded = oracle
            .scenarios
            .iter()
            .find(|s| s.name == scenario.name)
            .map(|s| &s.response)
            .expect("scenario missing from the oracle");

        let mut worker = Worker::new_with_ops(
            Script::new(code.as_str()),
            Some(RuntimeLimits::default()),
            std::sync::Arc::new(Ops),
        )
        .await
        .expect("worker creation failed");

        let (status, headers, body) =
            dispatch(&mut worker, &spec.base_url, &scenario.request).await;
        let (warm_status, warm_headers, warm_body) =
            dispatch(&mut worker, &spec.base_url, &scenario.request).await;
        drop(worker);

        let warm_identical = warm_status == status && warm_headers == headers && warm_body == body;
        let body_ok = sha256(&body) == recorded.body_sha256;
        let headers_ok = headers == recorded.headers;
        let status_ok = status == recorded.status;
        let warm_ok = warm_identical == recorded.warm_identical;

        let verdict = match (status_ok, body_ok, headers_ok, warm_ok) {
            (true, true, true, true) => {
                passed += 1;
                "PASS"
            }
            (true, true, false, true) => {
                partial += 1;
                "PARTIAL"
            }
            _ => "FAIL",
        };

        println!("{:<24} {verdict}", scenario.name);

        if !status_ok {
            println!("      status expected {} actual {status}", recorded.status);
        }

        if !headers_ok {
            for line in header_diff(&recorded.headers, &headers) {
                println!("{line}");
            }
        }

        if !body_ok {
            let expected = std::fs::read(root.join(&recorded.body_file)).unwrap_or_default();
            println!("      {}", first_divergence(&expected, &body));
        }

        if !warm_ok {
            println!(
                "      warm_identical expected {} actual {warm_identical}",
                recorded.warm_identical
            );
        }
    }

    println!(
        "\n{passed} pass, {partial} partial, {} fail, of {}",
        spec.scenarios.len() - passed - partial,
        spec.scenarios.len()
    );
}
