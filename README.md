# openworkers-runtime-boa

A high-performance JavaScript runtime for OpenWorkers based on [Boa](https://github.com/boa-dev/boa) - a 100% Rust JavaScript engine.

## Features

- ✅ **100% Rust** - No C/C++ dependencies
- ✅ **Fast cold start** - ~8ms average (5.2ms init + 2.9ms exec)
- ✅ **Web APIs** - fetch(), URL, console, timers
- ✅ **Event handlers** - addEventListener('fetch'), addEventListener('scheduled')
- ✅ **Async/await** - Full Promise support
- ✅ **HTTP forwarding** - External fetch() calls with reqwest

## Performance

```
Worker::new(): avg=5.2ms, min=3ms, max=12ms
exec():        avg=2.9ms, min=2.5ms, max=3.6ms
Total:         avg=8.2ms, min=5.7ms, max=15.8ms
```

Significantly faster than V8-based runtimes (Deno: ~22ms cold start).

## Installation

```toml
[dependencies]
openworkers-runtime-boa = "0.1"
```

## Usage

```rust
use openworkers_runtime_boa::{Worker, Script, Task, HttpRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() {
    let code = r#"
        addEventListener('fetch', async (event) => {
            const { pathname } = new URL(event.request.url);
            event.respondWith(new Response(`Hello from ${pathname}!`));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/test".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    println!("Status: {}", response.status);
}
```

## Supported APIs

### Web APIs
- `fetch()` - HTTP requests with external APIs
- `URL` / `URLSearchParams` - URL parsing
- `console.log/warn/error/info/debug` - Logging
- `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` - Timers
- `Response` - HTTP responses (with workaround for Boa 0.21 constructor bug)
- `Request` - HTTP requests (via fetch)

### Event Handlers
- `addEventListener('fetch', handler)` - Handle HTTP requests
- `addEventListener('scheduled', handler)` - Handle scheduled/cron events
- `event.respondWith(response)` - Send HTTP response
- `event.waitUntil(promise)` - Wait for async operations

## Examples

Run examples with:

```bash
cargo run --example simple_test
cargo run --example benchmark
cargo run --example test_real_fetch
cargo run --example test_fetch_forward
```

## Testing

```bash
cargo test
```

21 integration tests covering all functionality.

## Known Limitations

### Response Constructor Workaround

Boa 0.21 has a bug where the `Response` constructor ignores the `body` and `options` parameters. We provide a workaround that overrides the constructor with a working implementation.

This workaround will be removed once [Boa issue #XXXXX](https://github.com/boa-dev/boa/issues/XXXXX) is fixed.

Meanwhile:
- ✅ `new Response(body, options)` works with our workaround
- ✅ `fetch()` returns native Boa Response objects
- ✅ All Web API compatibility maintained

### Fetch Implementation

Uses `reqwest::blocking` with `tokio::spawn_blocking` to execute HTTP requests without blocking the async runtime.

## Comparison with Other Runtimes

| Runtime | Language | Cold Start | Maturity |
|---------|----------|------------|----------|
| **Boa** | 100% Rust | ~8ms | Beta |
| JSCore | Rust + C | ~2ms | Stable |
| Deno | Rust + C++ | ~22ms | Production |

## Architecture

- `worker.rs` - Worker implementation with event handling
- `runtime.rs` - TokioJobQueue for async job execution
- `task.rs` - HTTP request/response types
- `compat.rs` - Compatibility layer with other runtimes

## License

See LICENSE file.

## Contributing

This runtime is part of the [OpenWorkers](https://openworkers.com) project.

## Credits

Built on [Boa](https://github.com/boa-dev/boa) - A JavaScript engine written in Rust.
