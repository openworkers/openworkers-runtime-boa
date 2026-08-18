# OpenWorkers Runtime Boa

Pure Rust JavaScript runtime for serverless workers, built on [Boa](https://github.com/boa-dev/boa).

## Quick Start

```rust
use openworkers_runtime_boa::{Event, HttpMethod, HttpRequest, RequestBody, Script, Worker};
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

`Worker::new` runs with `DefaultOps`, which rejects every outbound operation.
Pass a real `OperationsHandler` through `Worker::new_with_ops` to serve
`fetch()` from the host.

## Supported

- **100% Rust** - no C/C++ dependencies, builds anywhere
- **Fast cold start** - no JIT warmup
- **Events** - `addEventListener('fetch')` and `addEventListener('scheduled')`
- **Web APIs** - console, timers, `fetch`, Request, Response, Headers, URL,
  URLSearchParams, Blob, File, FormData, AbortController, ReadableStream,
  TextEncoder/TextDecoder, atob/btoa, structuredClone
- **Crypto** - `getRandomValues`, `randomUUID`, `subtle.digest`
- **Async/await** - full Promise support

## Not supported

- **RuntimeLimits** - CPU, wall clock and memory limits are accepted and ignored
- **Bindings** - `env` and the KV/storage/database/worker bindings are not exposed to JS
- **ES modules** - only `addEventListener`, not `export default { fetch }`
- **Console capture** - console writes to stderr instead of the OperationsHandler
- **Streaming bodies** - `RequestBody::Stream` and `ResponseBody::Stream` are not read
- **Binary bodies** - request and response bodies round-trip as lossy UTF-8
- **Snapshots** - Boa has no equivalent of the V8 startup snapshot

## Testing

```bash
cargo test
```

## Status

See [TODO.md](TODO.md) for the roadmap.

## License

MIT
