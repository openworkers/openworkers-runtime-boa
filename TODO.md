# TODO

## High Priority

- [ ] **Enforce RuntimeLimits** - `limits` is accepted and ignored, so a worker can
      spin forever; no CPU, wall clock or heap ceiling
- [ ] **Wire console to OperationsHandler** - console writes to stderr, so the runner
      cannot collect worker logs
- [ ] **Binary-safe bodies** - request and response bodies go through JS as lossy
      UTF-8 strings, which corrupts any non-text payload

## Medium Priority

- [ ] **Bindings JS API** - expose `env` plus the KV, storage, database and worker
      bindings, routed to `handle_binding_*`
- [ ] **Streaming bodies** - `RequestBody::Stream` is dropped and responses are
      always collected into `ResponseBody::Bytes`
- [ ] **ES Modules support** - `export default { fetch() {} }` style handlers,
      which the runner currently lowers to `globalThis.default` before us
- [ ] **Build fetch() responses without eval** - `resolve_pending_fetches` still
      generates a `new Response(...)` snippet per outbound fetch

## Low Priority

- [ ] **Fill out crypto.subtle** - only `digest` exists; no sign, verify, importKey
      or deriveBits
- [ ] **Benchmark suite** - `examples/ssr_bench.rs` covers SvelteKit SSR, nothing
      else is automated

## Won't Do (N/A for Boa)

- Isolate pooling - not needed, Context creation is cheap
- Thread pinning - single-threaded interpreter
- GC tracking - Rust memory management
- Snapshots - Boa has no equivalent of the V8 startup snapshot
