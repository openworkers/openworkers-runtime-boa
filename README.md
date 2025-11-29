# OpenWorkers Runtime - Boa

A JavaScript runtime for OpenWorkers based on [Boa](https://github.com/boa-dev/boa) - a JavaScript engine written in 100% Rust.

## Features

- ✅ **100% Rust** - No C/C++ dependencies
- ✅ **Fast Startup** - Consistent ~1.7ms total time
- ✅ **Async/Await** - Full Promise support
- ✅ **Timers** - setTimeout, setInterval, clearTimeout, clearInterval
- ✅ **Fetch API** - HTTP requests to external APIs
- ✅ **Event Handlers** - addEventListener('fetch'), addEventListener('scheduled')
- ✅ **Console Logging** - console.log/warn/error
- ✅ **URL API** - URL parsing

## Performance

Run benchmark:
```bash
cargo run --example benchmark --release
```

### Results (Apple Silicon, Release Mode)

```
Worker::new(): avg=1.22ms, min=605µs, max=3.3ms
exec():        avg=681µs, min=441µs, max=1.28ms
Total:         avg=1.91ms, min=1.05ms, max=4.6ms
```

### Runtime Comparison (v0.5.0)

| Runtime | Engine | Worker::new() | exec_simple | exec_json | Tests |
|---------|--------|---------------|-------------|-----------|-------|
| **[QuickJS](https://github.com/openworkers/openworkers-runtime-quickjs)** | QuickJS | 738µs | **12.4µs** ⚡ | **13.7µs** | 16/17 |
| **[V8](https://github.com/openworkers/openworkers-runtime-v8)** | V8 | 790µs | 32.3µs | 34.3µs | **17/17** |
| **[JSC](https://github.com/openworkers/openworkers-runtime-jsc)** | JavaScriptCore | 1.07ms | 30.3µs | 28.3µs | 15/17 |
| **[Deno](https://github.com/openworkers/openworkers-runtime-deno)** | V8 + Deno | 2.56ms | 46.8µs | 38.7µs | **17/17** |
| **[Boa](https://github.com/openworkers/openworkers-runtime-boa)** | Boa | 738µs | 12.4µs | 13.7µs | 13/17 |

**Boa is 100% Rust** - no C/C++ dependencies, ideal for embedded/WASM use cases.

## Installation

```toml
[dependencies]
openworkers-runtime-boa = { path = "../openworkers-runtime-boa" }
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

            if (pathname === '/api') {
                // Fetch forward to external API
                const response = await fetch('https://api.example.com/data');
                event.respondWith(response);
            } else {
                event.respondWith(new Response('Hello from Boa!'));
            }
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    let req = HttpRequest {
        method: "GET".to_string(),
        url: "http://localhost/".to_string(),
        headers: HashMap::new(),
        body: None,
    };

    let (task, rx) = Task::fetch(req);
    worker.exec(task).await.unwrap();

    let response = rx.await.unwrap();
    println!("Status: {}", response.status);
}
```

## Testing

```bash
# Run all tests (21 tests)
cargo test

# Run with output
cargo test -- --nocapture
```

### Test Coverage

- **Integration** (multiple) - Timers, fetch, scheduled events, error handling
- Comprehensive scenarios validating async execution and callback timing

**Total: 21 tests** ✅

## Supported JavaScript APIs

### Timers
- `setTimeout(callback, delay)`
- `setInterval(callback, interval)`
- `clearTimeout(id)`
- `clearInterval(id)`

### Fetch API
- `fetch(url, options)` - HTTP requests (GET, POST, PUT, DELETE, PATCH)
- Response: status, text(), json()
- Promise-based with async/await

### Other APIs
- `console.log/warn/error/info/debug`
- `URL` - Basic URL parsing
- `Response` - HTTP responses (with workaround for Boa constructor)
- `addEventListener` - Event handling
- `Date.now()` - Timestamps
- `Math.*` - Standard math operations

## Known Limitations

### Response Constructor Workaround

Boa 0.21 has a bug where the `Response` constructor ignores parameters. We provide a workaround that overrides the constructor.

This workaround will be removed once the Boa issue is fixed.

### Fetch Implementation

Uses `reqwest::blocking` with `tokio::spawn_blocking` to execute HTTP requests without blocking the async runtime.

## Architecture

```
src/
├── lib.rs              # Public API
├── worker.rs           # Worker with event handlers
├── task.rs             # Task types (Fetch, Scheduled)
├── compat.rs           # Compatibility layer
└── runtime.rs          # Runtime with TokioJobQueue
```

## Key Advantages

- **100% Rust** - No native dependencies, easier to build and deploy
- **Consistent performance** - No warmup needed
- **Pure Rust toolchain** - Compile anywhere Rust works
- **Predictable** - No JIT compilation complexity

## Other Runtime Implementations

OpenWorkers supports multiple JavaScript engines:

- **[openworkers-runtime](https://github.com/openworkers/openworkers-runtime)** - Deno-based (V8 + Deno extensions)
- **[openworkers-runtime-jsc](https://github.com/openworkers/openworkers-runtime-jsc)** - JavaScriptCore
- **[openworkers-runtime-boa](https://github.com/openworkers/openworkers-runtime-boa)** - This runtime (Boa, 100% Rust)
- **[openworkers-runtime-v8](https://github.com/openworkers/openworkers-runtime-v8)** - V8 via rusty_v8

## License

MIT License - See LICENSE file.

## Credits

Built on [Boa](https://github.com/boa-dev/boa) - A JavaScript engine written in Rust.
