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
  TextEncoder/TextDecoder, atob/btoa, structuredClone, queueMicrotask.
  console, URL, TextEncoder/TextDecoder, atob/btoa and structuredClone come
  from `boa_runtime`; the rest are ours
- **Crypto** - `getRandomValues`, `randomUUID`, `subtle.digest`
- **Async/await** - full Promise support

## Not supported

- **RuntimeLimits** - CPU, wall clock and memory limits are accepted and ignored
- **Bindings** - `env` and the KV/storage/database/worker bindings are not exposed to JS
- **ES modules** - only `addEventListener`, not `export default { fetch }`
- **Console capture** - console writes to stdout and stderr instead of the
  OperationsHandler
- **Streaming bodies** - `RequestBody::Stream` and `ResponseBody::Stream` are not read
- **Binary bodies** - request and response bodies round-trip as lossy UTF-8
- **Snapshots** - Boa has no equivalent of the V8 startup snapshot

## Testing

```bash
cargo test
```

## SSR benchmark

Renders a real 303 KB SvelteKit bundle, the openworkers-website build:

```bash
cargo run --release --features ssr-bench --example ssr_bench
```

The output is byte-identical to the same fixture rendered on V8
(sha256 `2ccbe4f9d1c98441dbe07ee2307597719adbcfbe5e181aa9a03fcd5d572a584c`).

## Status

See [TODO.md](TODO.md) for the roadmap.

## License

MIT
