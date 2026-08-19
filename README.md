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
  console, timers, `fetch`, AbortController, URL, TextEncoder/TextDecoder,
  atob/btoa and structuredClone come from `boa_runtime`; the rest are ours
- **Logs** - `console` and uncaught handler errors go to the `OperationsHandler`
- **Request lifetime** - the response goes out as soon as `respondWith` settles,
  and the timers and fetches still pending behind it are dropped
- **Crypto** - `getRandomValues`, `randomUUID`, `subtle.digest`
- **Async/await** - full Promise support

## Not supported

- **CPU and memory limits** - only `max_wall_clock_time_ms` is enforced
- **Bindings** - `env` and the KV/storage/database/worker bindings are not exposed to JS
- **ES modules** - only `addEventListener`, not `export default { fetch }`
- **Streaming bodies** - `RequestBody::Stream` and `ResponseBody::Stream` are not read
- **`event.waitUntil`** - the fetch event has no `waitUntil`
- **Binary bodies** - request and response bodies round-trip as lossy UTF-8, so
  `formData()` reads `application/x-www-form-urlencoded` and rejects multipart
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

## Conformance

Runs the 17 SvelteKit scenarios of `openworkers-conformance` and diffs status,
headers in emission order and body bytes against the responses recorded on
`openworkers-runtime-v8`:

```bash
cargo run --release --features conformance --example conformance
```

16 of 17 match byte for byte. `urlencoded-plus` differs on purpose: the
urlencoded parser decodes `+` to a space, which the recorded oracle does not.

## Status

See [TODO.md](TODO.md) for the roadmap.

## License

MIT
