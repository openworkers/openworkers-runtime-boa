use openworkers_core::{Event, HttpMethod, HttpRequest, RequestBody, Script};
use openworkers_runtime_boa::Worker;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Benchmark local stream creation and consumption (no network)
async fn bench_local_stream(chunk_count: usize, chunk_size: usize) -> (Duration, usize) {
    let code = format!(
        r#"
        addEventListener('fetch', (event) => {{
            const stream = new ReadableStream({{
                start(controller) {{
                    const chunk = new Uint8Array({});
                    for (let i = 0; i < {}; i++) {{
                        controller.enqueue(chunk);
                    }}
                    controller.close();
                }}
            }});
            event.respondWith(new Response(stream));
        }});
    "#,
        chunk_size, chunk_count
    );

    let script = Script::new(code.as_str());
    let mut worker = Worker::new(script, None).await.unwrap();

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: RequestBody::None,
    };

    let start = Instant::now();

    let (event, rx) = Event::fetch(req);
    worker.exec(event).await.unwrap();
    let response = rx.await.unwrap();

    let total_bytes = response.body.collect().await.map(|b| b.len()).unwrap_or(0);

    let elapsed = start.elapsed();
    (elapsed, total_bytes)
}

async fn bench_buffered_response(iterations: u32) -> Duration {
    let code = r#"
        addEventListener('fetch', (event) => {
            event.respondWith(new Response('Hello World from buffered response!'));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let start = Instant::now();

    for _ in 0..iterations {
        let req = HttpRequest {
            method: HttpMethod::Get,
            url: "http://localhost/".to_string(),
            headers: HashMap::new(),
            body: RequestBody::None,
        };

        let (event, rx) = Event::fetch(req);
        worker.exec(event).await.unwrap();
        let response = rx.await.unwrap();
        assert!(!response.body.is_stream());
    }

    start.elapsed()
}

async fn bench_json_response(iterations: u32) -> Duration {
    let code = r#"
        addEventListener('fetch', (event) => {
            const data = { message: 'Hello', timestamp: Date.now(), count: 42 };
            event.respondWith(new Response(JSON.stringify(data), {
                headers: { 'Content-Type': 'application/json' }
            }));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None).await.unwrap();

    let start = Instant::now();

    for _ in 0..iterations {
        let req = HttpRequest {
            method: HttpMethod::Get,
            url: "http://localhost/".to_string(),
            headers: HashMap::new(),
            body: RequestBody::None,
        };

        let (event, rx) = Event::fetch(req);
        worker.exec(event).await.unwrap();
        let _ = rx.await.unwrap();
    }

    start.elapsed()
}

#[tokio::main]
async fn main() {
    println!("OpenWorkers Boa Streaming Benchmark\n");
    println!("========================================\n");

    // Warmup
    println!("Warming up...");
    let _ = bench_buffered_response(5).await;
    println!();

    // Benchmark 1: Buffered responses (local, no network)
    println!("Buffered Response (local, no network):");
    let iterations = 1000;
    let elapsed = bench_buffered_response(iterations).await;
    let per_request = elapsed / iterations;
    println!(
        "  {} iterations in {:.2?} ({:.2?}/req, {:.0} req/s)\n",
        iterations,
        elapsed,
        per_request,
        iterations as f64 / elapsed.as_secs_f64()
    );

    // Benchmark 2: JSON responses
    println!("JSON Response (local, no network):");
    let iterations = 1000;
    let elapsed = bench_json_response(iterations).await;
    let per_request = elapsed / iterations;
    println!(
        "  {} iterations in {:.2?} ({:.2?}/req, {:.0} req/s)\n",
        iterations,
        elapsed,
        per_request,
        iterations as f64 / elapsed.as_secs_f64()
    );

    // Benchmark 3: Local JS stream (no network)
    println!("Local JS ReadableStream (no network):");

    for (chunks, chunk_size) in [(10, 1024), (100, 1024), (10, 10240)] {
        let (elapsed, total_bytes) = bench_local_stream(chunks, chunk_size).await;
        let throughput = if elapsed.as_secs_f64() > 0.0 {
            (total_bytes as f64 / 1024.0 / 1024.0) / elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!(
            "  {} chunks x {} bytes = {} KB in {:.2?} ({:.1} MB/s)",
            chunks,
            chunk_size,
            total_bytes / 1024,
            elapsed,
            throughput
        );
    }

    println!("\n========================================");
    println!("Summary:");
    println!("  - Boa is an interpreter (no JIT), expect slower than V8/JSC");
    println!("  - Good for lightweight/embedded use cases");
    println!("  - Pure Rust implementation (no native deps)");
    println!("\nBenchmark complete!");
}
