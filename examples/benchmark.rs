use openworkers_runtime_boa::{HttpRequest, Script, Task, Worker};
use std::collections::HashMap;
use std::time::Instant;

#[tokio::main]
async fn main() {
    let code = r#"
        addEventListener('fetch', (event) => {
            event.respondWith(new Response('Hello from Boa!', { status: 200 }));
        });
    "#;

    println!("=== Boa Runtime Benchmark ===\n");

    let iterations = 5;
    let mut creation_times = Vec::new();
    let mut exec_times = Vec::new();
    let mut total_times = Vec::new();

    for i in 1..=iterations {
        println!("Iteration {}/{}:", i, iterations);

        let total_start = Instant::now();

        // Measure Worker::new()
        let worker_start = Instant::now();
        let script = Script::new(code);
        let mut worker = Worker::new(script, None, None).await.unwrap();
        let worker_time = worker_start.elapsed();
        creation_times.push(worker_time.as_micros());
        println!("  Worker::new(): {:?}", worker_time);

        // Measure exec()
        let exec_start = Instant::now();
        let req = HttpRequest {
            method: "GET".to_string(),
            url: "http://localhost/".to_string(),
            headers: HashMap::new(),
            body: None,
        };
        let (task, _rx) = Task::fetch(req);
        worker.exec(task).await.unwrap();
        let exec_time = exec_start.elapsed();
        exec_times.push(exec_time.as_micros());
        println!("  exec():        {:?}", exec_time);

        let total_time = total_start.elapsed();
        total_times.push(total_time.as_micros());
        println!("  Total:         {:?}\n", total_time);
    }

    println!("=== Summary ===");
    println!(
        "Worker::new(): avg={}µs, min={}µs, max={}µs",
        creation_times.iter().sum::<u128>() / creation_times.len() as u128,
        creation_times.iter().min().unwrap(),
        creation_times.iter().max().unwrap()
    );
    println!(
        "exec():        avg={}µs, min={}µs, max={}µs",
        exec_times.iter().sum::<u128>() / exec_times.len() as u128,
        exec_times.iter().min().unwrap(),
        exec_times.iter().max().unwrap()
    );
    println!(
        "Total:         avg={}µs, min={}µs, max={}µs",
        total_times.iter().sum::<u128>() / total_times.len() as u128,
        total_times.iter().min().unwrap(),
        total_times.iter().max().unwrap()
    );
}
