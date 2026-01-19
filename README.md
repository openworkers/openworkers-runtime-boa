# OpenWorkers Runtime Boa

Pure Rust JavaScript runtime for serverless workers, built on [Boa](https://github.com/boa-dev/boa).

## Quick Start

```rust
use openworkers_runtime_boa::{Worker, Script, Event, HttpRequest, HttpMethod, RequestBody};
use std::collections::HashMap;

let script = Script::new(r#"
    addEventListener('fetch', event => {
        event.respondWith(new Response('Hello from Boa!'));
    });
"#);

let mut worker = Worker::new(script, None).await?;

let req = HttpRequest {
    method: HttpMethod::Get,
    url: "http://localhost/".to_string(),
    headers: HashMap::new(),
    body: RequestBody::None,
};

let (task, rx) = Event::fetch(req);
worker.exec(task).await?;

let response = rx.await?;
```

## Features

- **100% Rust** — No C/C++ dependencies, builds anywhere
- **Fast cold start** — No JIT warmup
- **Web APIs** — fetch, setTimeout, Response, Request, URL, console
- **Async/await** — Full Promise support
- **Streaming** — ReadableStream support

## Testing

```bash
cargo test
```

## Status

See [TODO.md](TODO.md) for current limitations and roadmap.

## License

MIT
