//! Wake a worker, server-side render a SvelteKit page, return the HTML, sleep.
//!
//! cargo run --release --features ssr-bench --example ssr_bench [bundle.js] [url]

use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use openworkers_transform::{CodeLanguage, parse_worker_code};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const WARM_RENDERS: usize = 20;
const COLD_CYCLES: usize = 10;
const IDLE_WORKERS: usize = 10;

/// Bridges the module worker export to addEventListener, which is the only
/// dispatch shape this runtime knows. ASSETS answers 404: no static files here.
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

/// Measures dispatch alone. Run in both a tiny and the 354 KB context, it says
/// whether per-request cost tracks script size: the dispatch snippet is
/// regenerated and re-parsed on every request, but it does not grow with it.
const TRIVIAL: &str = r#"
addEventListener('fetch', function(event) {
    event.respondWith(new Response('<html></html>'));
});
"#;

struct Rendered {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

async fn render(worker: &mut Worker, url: &str) -> Rendered {
    let req = HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let (event, rx) = Event::fetch(req);

    worker.exec(event).await.expect("fetch dispatch failed");

    let response = rx.await.expect("no response sent");
    let status = response.status;
    let headers = response.headers.clone();
    let body = response.body.collect().await.unwrap_or_default();

    Rendered {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn report(label: &str, mut samples: Vec<Duration>) {
    samples.sort();

    println!(
        "{:<38} min {:>9.2?}   median {:>9.2?}",
        label,
        samples[0],
        samples[samples.len() / 2]
    );
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

fn rss_kb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        format!(
            "{}/examples/fixtures/sveltekit_worker.js",
            env!("CARGO_MANIFEST_DIR")
        )
    });

    // Every route of this build is prerendered, so a rendered page can only be
    // reached through a path the router does not know: the SvelteKit error page.
    let url = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "http://localhost/ssr-bench".to_string());

    let source = std::fs::read(&path).expect("bundle not readable");
    println!("bundle {} ({} B ESM)", path, source.len());

    let lowered = parse_worker_code(&source, CodeLanguage::JavaScript).expect("lowering failed");
    println!("lowered to {} B classic script", lowered.len());

    let code = format!("{}\n{}", lowered, ADAPTER);

    let start = Instant::now();
    let mut worker = Worker::new(Script::new(code.as_str()), None)
        .await
        .expect("worker creation failed");
    let creation = start.elapsed();

    let start = Instant::now();
    let first = render(&mut worker, &url).await;
    let first_render = start.elapsed();

    println!(
        "\nstatus {} body {} B sha256 {}",
        first.status,
        first.body.len(),
        sha256(first.body.as_bytes())
    );

    for (name, value) in &first.headers {
        println!("  {}: {}", name, value);
    }

    println!("\n{}\n", &first.body[..first.body.len().min(200)]);
    assert!(
        first.body.contains("<html"),
        "expected HTML, got {} B",
        first.body.len()
    );

    let mut warm = Vec::with_capacity(WARM_RENDERS);

    for _ in 0..WARM_RENDERS {
        let start = Instant::now();
        let out = render(&mut worker, &url).await;
        warm.push(start.elapsed());
        assert!(out.body == first.body, "warm render diverged");
    }

    let mut cold = Vec::with_capacity(COLD_CYCLES);

    for _ in 0..COLD_CYCLES {
        let start = Instant::now();
        let mut w = Worker::new(Script::new(code.as_str()), None)
            .await
            .expect("worker creation failed");
        let out = render(&mut w, &url).await;
        cold.push(start.elapsed());
        assert!(out.body == first.body, "cold render diverged");
    }

    let big_context = format!("{}\n{}", lowered, TRIVIAL);
    let mut floors = Vec::new();

    for (label, script) in [
        ("dispatch floor, 100 B script", TRIVIAL),
        ("dispatch floor, 354 KB script", big_context.as_str()),
    ] {
        let mut w = Worker::new(Script::new(script), None)
            .await
            .expect("worker creation failed");
        let mut samples = Vec::with_capacity(WARM_RENDERS);

        for _ in 0..WARM_RENDERS {
            let start = Instant::now();
            render(&mut w, "http://localhost/").await;
            samples.push(start.elapsed());
        }

        floors.push((label, samples));
    }

    println!("worker creation (parse + compile)      {:>13.2?}", creation);
    println!(
        "first render                           {:>13.2?}",
        first_render
    );
    report(&format!("warm render (x{})", WARM_RENDERS), warm);
    report(&format!("cold cycle (x{})", COLD_CYCLES), cold);
    for (label, samples) in floors {
        report(label, samples);
    }

    let before = rss_kb();
    let mut idle = Vec::with_capacity(IDLE_WORKERS);

    for _ in 0..IDLE_WORKERS {
        idle.push(
            Worker::new(Script::new(code.as_str()), None)
                .await
                .expect("worker creation failed"),
        );
    }

    let after = rss_kb();
    println!(
        "rss {} MB, {} KB per idle worker",
        after / 1024,
        (after - before) / IDLE_WORKERS as u64
    );
}
