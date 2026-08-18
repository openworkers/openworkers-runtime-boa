# TODO

## High Priority

- [ ] **CPU and heap ceilings** - only the wall clock limit is enforced, so a
      worker spinning in a loop still holds its thread
- [ ] **Binary-safe bodies** - request and response bodies go through JS as lossy
      UTF-8 strings, which corrupts any non-text payload

## Medium Priority

- [ ] **Bindings JS API** - expose `env` plus the KV, storage, database and worker
      bindings, routed to `handle_binding_*`
- [ ] **Streaming bodies** - `RequestBody::Stream` is dropped and responses are
      always collected into `ResponseBody::Bytes`
- [ ] **ES Modules support** - `export default { fetch() {} }` style handlers,
      which the runner currently lowers to `globalThis.default` before us
- [ ] **`event.waitUntil`** - the fetch event has none, and honouring one needs a
      budget of its own, because the response does not wait for pending work
- [ ] **Concurrent outbound work** - the job executor awaits one async job at a
      time, so `Promise.all([fetch(a), fetch(b)])` runs the two in sequence

## Low Priority

- [ ] **Fill out crypto.subtle** - only `digest` exists; no sign, verify, importKey
      or deriveBits
- [ ] **Benchmark suite** - `examples/ssr_bench.rs` covers SvelteKit SSR, nothing
      else is automated
- [ ] **Outbound `Accept-Language`** - `boa_runtime`'s fetch adds `en-US` to every
      request that does not carry one

## Won't Do (N/A for Boa)

- Isolate pooling - not needed, Context creation is cheap
- Thread pinning - single-threaded interpreter
- GC tracking - Rust memory management
- Snapshots - Boa has no equivalent of the V8 startup snapshot
