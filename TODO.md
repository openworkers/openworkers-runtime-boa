# TODO

## High Priority

- [ ] **Wire fetch() to OperationsHandle** — Currently uses boa_runtime directly, runner can't intercept
- [ ] **Wire console to OperationsHandle** — Currently prints to stderr, runner can't collect logs
- [ ] **ES Modules support** — `export default { fetch() {} }` style handlers

## Medium Priority

- [ ] **Bindings JS API** — Expose `env.KV`, `env.DB`, `env.STORAGE` to JS
- [ ] **CPU/memory limits** — Currently `limits` parameter is ignored
- [ ] **Improve streaming** — Don't buffer entire RequestBody for streams

## Low Priority

- [ ] **TextEncoder/TextDecoder** — Currently using workarounds
- [ ] **Benchmark suite** — Automated perf comparison with V8

## Won't Do (N/A for Boa)

- Isolate pooling — Not needed, Context creation is cheap
- Thread pinning — Single-threaded interpreter
- GC tracking — Rust memory management
