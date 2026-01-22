use boa_engine::{
    Context, Finalize, JsData, JsResult, JsString, Source, Trace, object::builtins::JsPromise,
};
use boa_runtime::RuntimeExtension;
use boa_runtime::fetch::{Fetcher, request::JsRequest, response::JsResponse};
use bytes::Bytes;
use openworkers_core::{
    DefaultOps, Event, HttpMethod, HttpRequest, HttpResponse, LogLevel, Operation, OperationResult,
    OperationsHandle, RequestBody, ResponseBody, RuntimeLimits, Script, TaskResult,
    TerminationReason,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Fetcher that routes all HTTP requests through OperationsHandler.
/// This ensures the runtime has NO direct network access - the runner controls all I/O.
#[derive(Clone, Finalize, JsData)]
struct OpsFetcher {
    ops: OperationsHandle,
}

// Manual Trace impl since OperationsHandle (Arc) is safe to ignore
unsafe impl Trace for OpsFetcher {
    boa_gc::custom_trace!(this, mark, {});
}

impl std::fmt::Debug for OpsFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpsFetcher").finish()
    }
}

impl Fetcher for OpsFetcher {
    async fn fetch(
        self: Rc<Self>,
        request: JsRequest,
        _context: &RefCell<&mut Context>,
    ) -> JsResult<JsResponse> {
        let req = request.into_inner();
        let url = req.uri().to_string();
        let url_for_result = url.clone();

        // Convert http::Method to HttpMethod
        let method = match req.method().as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "DELETE" => HttpMethod::Delete,
            "PATCH" => HttpMethod::Patch,
            "HEAD" => HttpMethod::Head,
            "OPTIONS" => HttpMethod::Options,
            _ => HttpMethod::Get,
        };

        // Convert headers
        let headers: HashMap<String, String> = req
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        // Convert body
        let body = if req.body().is_empty() {
            RequestBody::None
        } else {
            RequestBody::Bytes(Bytes::from(req.body().to_vec()))
        };

        let http_request = HttpRequest {
            method,
            url,
            headers,
            body,
        };

        // Route through OperationsHandler
        let result = self.ops.handle(Operation::Fetch(http_request)).await;

        match result {
            OperationResult::Http(Ok(response)) => {
                // Collect response body
                let body_bytes = match response.body {
                    ResponseBody::None => Vec::new(),
                    ResponseBody::Bytes(b) => b.to_vec(),
                    ResponseBody::Stream(mut rx) => {
                        let mut chunks = Vec::new();

                        while let Some(result) = rx.recv().await {
                            if let Ok(bytes) = result {
                                chunks.extend(bytes.to_vec());
                            }
                        }

                        chunks
                    }
                };

                // Build http::Response for JsResponse
                let mut builder = http::Response::builder().status(response.status);

                for (key, value) in response.headers {
                    builder = builder.header(key, value);
                }

                builder
                    .body(body_bytes)
                    .map_err(boa_engine::JsError::from_rust)
                    .map(|http_response| {
                        JsResponse::basic(JsString::from(url_for_result), http_response)
                    })
            }
            OperationResult::Http(Err(e)) => {
                Err(boa_engine::JsNativeError::error().with_message(e).into())
            }
            _ => Err(boa_engine::JsNativeError::error()
                .with_message("Unexpected operation result")
                .into()),
        }
    }
}

pub struct Worker {
    context: Context,
    aborted: Arc<AtomicBool>,
    #[allow(dead_code)]
    ops: OperationsHandle,
}

impl Worker {
    /// Create a new worker with an OperationsHandler
    ///
    /// All operations (fetch, log, etc.) go through the runner's OperationsHandler.
    /// Note: Bindings are not yet wired up to JS in the Boa runtime.
    pub async fn new_with_ops(
        script: Script,
        _limits: Option<RuntimeLimits>,
        ops: OperationsHandle,
    ) -> Result<Self, TerminationReason> {
        let mut context = Context::default();

        // Setup console that routes to OperationsHandler
        // We use our own implementation instead of boa_runtime::ConsoleExtension
        // so that logs go through the runner, not directly to stdout/stderr
        setup_console_with_ops(&mut context, ops.clone()).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register console: {}", e))
        })?;

        // Setup crypto (getRandomValues, randomUUID, subtle.digest)
        setup_crypto(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register crypto: {}", e))
        })?;

        // Setup TextEncoder/TextDecoder
        setup_text_encoding(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register text encoding: {}",
                e
            ))
        })?;

        // Register timers
        boa_runtime::extensions::TimeoutExtension
            .register(None, &mut context)
            .map_err(|e| {
                TerminationReason::InitializationError(format!("Failed to register timers: {}", e))
            })?;

        // Register URL API
        boa_runtime::extensions::UrlExtension
            .register(None, &mut context)
            .map_err(|e| {
                TerminationReason::InitializationError(format!("Failed to register URL: {}", e))
            })?;

        // Register fetch with OpsFetcher that routes through OperationsHandler
        // This ensures the runtime has NO direct network access
        boa_runtime::extensions::FetchExtension(OpsFetcher { ops: ops.clone() })
            .register(None, &mut context)
            .map_err(|e| {
                TerminationReason::InitializationError(format!("Failed to register fetch: {}", e))
            })?;

        // Override Response constructor with our working implementation
        // (until Boa fixes their constructor to actually use the body parameter)
        // TODO: Remove this when https://github.com/boa-dev/boa/issues/4547 is fixed
        let setup_response = r#"
            // Save native Response class
            const NativeResponse = globalThis.Response;
            const nativeFetch = globalThis.fetch;

            // WHATWG Headers class implementation
            globalThis.Headers = class Headers {
                constructor(init) {
                    this._map = new Map();
                    if (init) {
                        if (init instanceof Headers) {
                            for (const [key, value] of init) {
                                this.append(key, value);
                            }
                        } else if (Array.isArray(init)) {
                            for (let i = 0; i < init.length; i++) {
                                const [key, value] = init[i];
                                this.append(key, value);
                            }
                        } else if (typeof init === 'object') {
                            const keys = Object.keys(init);
                            for (let i = 0; i < keys.length; i++) {
                                this.append(keys[i], init[keys[i]]);
                            }
                        }
                    }
                }

                _normalizeKey(name) {
                    return String(name).toLowerCase();
                }

                append(name, value) {
                    const key = this._normalizeKey(name);
                    const existing = this._map.get(key);
                    if (existing !== undefined) {
                        this._map.set(key, existing + ', ' + String(value));
                    } else {
                        this._map.set(key, String(value));
                    }
                }

                delete(name) {
                    this._map.delete(this._normalizeKey(name));
                }

                get(name) {
                    const value = this._map.get(this._normalizeKey(name));
                    return value !== undefined ? value : null;
                }

                has(name) {
                    return this._map.has(this._normalizeKey(name));
                }

                set(name, value) {
                    this._map.set(this._normalizeKey(name), String(value));
                }

                entries() {
                    return this._map.entries();
                }

                keys() {
                    return this._map.keys();
                }

                values() {
                    return this._map.values();
                }

                forEach(callback, thisArg) {
                    this._map.forEach((value, key) => {
                        callback.call(thisArg, value, key, this);
                    });
                }

                [Symbol.iterator]() {
                    return this._map.entries();
                }
            };

            // Improved Response class with proper Headers and ReadableStream support
            globalThis.Response = class Response {
                constructor(body, init) {
                    init = init || {};

                    // Handle body - support string, Uint8Array, ReadableStream
                    if (body === null || body === undefined) {
                        this._body = '';
                        this._bodyStream = null;
                    } else if (body instanceof ReadableStream) {
                        this._body = null;
                        this._bodyStream = body;
                    } else if (typeof body === 'string') {
                        this._body = body;
                        this._bodyStream = null;
                    } else if (body instanceof Uint8Array) {
                        this._body = new TextDecoder().decode(body);
                        this._bodyStream = null;
                    } else {
                        this._body = String(body);
                        this._bodyStream = null;
                    }

                    this.status = init.status || 200;
                    this.statusText = init.statusText || 'OK';
                    this.ok = this.status >= 200 && this.status < 300;
                    this.bodyUsed = false;

                    // Handle headers
                    if (init.headers instanceof Headers) {
                        this.headers = init.headers;
                    } else {
                        this.headers = new Headers(init.headers);
                    }
                }

                // Body property returns ReadableStream or creates one from string
                get body() {
                    if (this._bodyStream) {
                        return this._bodyStream;
                    }
                    if (this._body === null || this._body === '') {
                        return null;
                    }
                    // Create ReadableStream from string body (enqueue string directly)
                    const bodyStr = this._body;
                    return new ReadableStream({
                        start(controller) {
                            controller.enqueue(bodyStr);
                            controller.close();
                        }
                    });
                }

                async text() {
                    if (this.bodyUsed) {
                        throw new TypeError('Body already consumed');
                    }
                    this.bodyUsed = true;

                    if (this._body !== null) {
                        return this._body;
                    }

                    // Read from stream
                    if (this._bodyStream) {
                        const reader = this._bodyStream.getReader();
                        let result = '';
                        while (true) {
                            const { done, value } = await reader.read();
                            if (done) break;
                            // Handle both string and Uint8Array chunks
                            if (typeof value === 'string') {
                                result += value;
                            } else if (value && value.length) {
                                // Assume Uint8Array-like, convert to string
                                for (let i = 0; i < value.length; i++) {
                                    result += String.fromCharCode(value[i]);
                                }
                            }
                        }
                        return result;
                    }

                    return '';
                }

                async json() {
                    const text = await this.text();
                    return JSON.parse(text);
                }

                async arrayBuffer() {
                    const text = await this.text();
                    const encoder = new TextEncoder();
                    return encoder.encode(text).buffer;
                }

                async bytes() {
                    const text = await this.text();
                    return new TextEncoder().encode(text);
                }

                clone() {
                    if (this.bodyUsed) {
                        throw new TypeError('Cannot clone a consumed response');
                    }
                    if (this._bodyStream) {
                        throw new TypeError('Cannot clone a streaming response');
                    }
                    return new Response(this._body, {
                        status: this.status,
                        statusText: this.statusText,
                        headers: new Headers(this.headers)
                    });
                }

                static json(data, init) {
                    init = init || {};
                    const headers = new Headers(init.headers);
                    if (!headers.has('content-type')) {
                        headers.set('content-type', 'application/json');
                    }
                    return new Response(JSON.stringify(data), {
                        ...init,
                        headers: headers
                    });
                }

                static redirect(url, status) {
                    status = status || 302;
                    const headers = new Headers();
                    headers.set('location', url);
                    return new Response(null, { status, headers });
                }
            };

            // Keep native fetch (uses NativeResponse internally)
            globalThis.fetch = nativeFetch;

            // ReadableStream implementation using function constructors (Boa has issues with class getters)
            function ReadableStream(underlyingSource) {
                underlyingSource = underlyingSource || {};
                this._underlyingSource = underlyingSource;
                this._controller = null;
                this._reader = null;
                this._state = 'readable';
                this._storedError = null;

                const controller = new ReadableStreamDefaultController(this);
                this._controller = controller;

                if (underlyingSource.start) {
                    Promise.resolve(underlyingSource.start(controller)).catch(function(e) {
                        controller.error(e);
                    });
                }
            }

            ReadableStream.prototype.getReader = function() {
                if (this._reader) {
                    throw new TypeError('ReadableStream is locked to a reader');
                }
                const reader = new ReadableStreamDefaultReader(this);
                this._reader = reader;
                return reader;
            };

            ReadableStream.prototype.cancel = function(reason) {
                if (this._state === 'closed') return Promise.resolve();
                if (this._state === 'errored') return Promise.reject(this._storedError);

                this._state = 'closed';
                if (this._reader) {
                    this._reader._closePending();
                    this._reader = null;
                }
                if (this._underlyingSource.cancel) {
                    return Promise.resolve(this._underlyingSource.cancel(reason));
                }
                return Promise.resolve();
            };

            Object.defineProperty(ReadableStream.prototype, 'locked', {
                get: function() { return this._reader !== null; }
            });

            globalThis.ReadableStream = ReadableStream;

            function ReadableStreamDefaultController(stream) {
                this._stream = stream;
                this._queue = [];
                this._closeRequested = false;
            }

            ReadableStreamDefaultController.prototype.enqueue = function(chunk) {
                if (this._closeRequested) throw new TypeError('Cannot enqueue after close');
                if (this._stream._state !== 'readable') throw new TypeError('Stream not readable');
                this._queue.push({ type: 'chunk', value: chunk });
                this._processQueue();
            };

            ReadableStreamDefaultController.prototype.close = function() {
                if (this._closeRequested) throw new TypeError('Stream is already closing');
                if (this._stream._state !== 'readable') throw new TypeError('Stream not readable');
                this._closeRequested = true;
                this._queue.push({ type: 'close' });
                this._processQueue();
            };

            ReadableStreamDefaultController.prototype.error = function(error) {
                if (this._stream._state !== 'readable') return;
                this._stream._state = 'errored';
                this._stream._storedError = error;
                if (this._stream._reader) this._stream._reader._errorPending(error);
                this._queue = [];
            };

            ReadableStreamDefaultController.prototype._processQueue = function() {
                if (this._stream._reader) this._stream._reader._processQueue();
            };

            Object.defineProperty(ReadableStreamDefaultController.prototype, 'desiredSize', {
                get: function() {
                    if (this._stream._state === 'errored') return null;
                    if (this._stream._state === 'closed') return 0;
                    return Math.max(0, 1 - this._queue.length);
                }
            });

            globalThis.ReadableStreamDefaultController = ReadableStreamDefaultController;

            function ReadableStreamDefaultReader(stream) {
                if (stream._reader) throw new TypeError('Stream is already locked');
                this._stream = stream;
                this._readRequests = [];
                const self = this;
                this._closedPromise = new Promise(function(resolve, reject) {
                    self._closedPromiseResolve = resolve;
                    self._closedPromiseReject = reject;
                });
            }

            ReadableStreamDefaultReader.prototype.read = function() {
                const self = this;
                if (!this._stream) return Promise.reject(new TypeError('Reader is released'));
                if (this._stream._state === 'errored') return Promise.reject(this._stream._storedError);

                const controller = this._stream._controller;
                if (controller._queue.length > 0) {
                    const item = controller._queue.shift();
                    if (item.type === 'close') {
                        this._stream._state = 'closed';
                        this._closePending();
                        return Promise.resolve({ done: true, value: undefined });
                    }
                    return Promise.resolve({ done: false, value: item.value });
                }

                if (this._stream._state === 'closed') {
                    return Promise.resolve({ done: true, value: undefined });
                }

                return new Promise(function(resolve, reject) {
                    self._readRequests.push({ resolve: resolve, reject: reject });
                });
            };

            ReadableStreamDefaultReader.prototype._processQueue = function() {
                const controller = this._stream._controller;
                while (this._readRequests.length > 0 && controller._queue.length > 0) {
                    const request = this._readRequests.shift();
                    const item = controller._queue.shift();
                    if (item.type === 'close') {
                        this._stream._state = 'closed';
                        request.resolve({ done: true, value: undefined });
                        this._closePending();
                        break;
                    }
                    request.resolve({ done: false, value: item.value });
                }
                if (this._stream._state === 'closed') {
                    while (this._readRequests.length > 0) {
                        this._readRequests.shift().resolve({ done: true, value: undefined });
                    }
                }
            };

            ReadableStreamDefaultReader.prototype._closePending = function() {
                if (this._closedPromiseResolve) {
                    this._closedPromiseResolve();
                    this._closedPromiseResolve = null;
                }
            };

            ReadableStreamDefaultReader.prototype._errorPending = function(error) {
                while (this._readRequests.length > 0) {
                    this._readRequests.shift().reject(error);
                }
                if (this._closedPromiseReject) {
                    this._closedPromiseReject(error);
                    this._closedPromiseReject = null;
                }
            };

            ReadableStreamDefaultReader.prototype.releaseLock = function() {
                if (!this._stream) return;
                if (this._readRequests.length > 0) {
                    throw new TypeError('Cannot release lock while reads pending');
                }
                this._stream._reader = null;
                this._stream = null;
            };

            ReadableStreamDefaultReader.prototype.cancel = function(reason) {
                if (!this._stream) return Promise.reject(new TypeError('Reader is released'));
                const p = this._stream.cancel(reason);
                this.releaseLock();
                return p;
            };

            Object.defineProperty(ReadableStreamDefaultReader.prototype, 'closed', {
                get: function() { return this._closedPromise; }
            });

            globalThis.ReadableStreamDefaultReader = ReadableStreamDefaultReader;
        "#;

        context
            .eval(Source::from_bytes(setup_response))
            .map_err(|e| {
                TerminationReason::InitializationError(format!(
                    "Failed to setup Response workaround: {}",
                    e
                ))
            })?;

        // Setup addEventListener
        let setup = r#"
            globalThis.addEventListener = function(type, handler) {
                if (type === 'fetch') {
                    globalThis.__fetchHandler = handler;
                } else if (type === 'scheduled') {
                    globalThis.__scheduledHandler = handler;
                }
            };
        "#;

        context
            .eval(Source::from_bytes(setup))
            .map_err(|e| TerminationReason::InitializationError(format!("Setup failed: {}", e)))?;

        // Evaluate user script
        let js_code = script.code.as_js().ok_or_else(|| {
            TerminationReason::InitializationError(
                "Boa runtime only supports JavaScript code".to_string(),
            )
        })?;

        context.eval(Source::from_bytes(js_code)).map_err(|e| {
            TerminationReason::Exception(format!("Script evaluation failed: {}", e))
        })?;

        Ok(Self {
            context,
            aborted: Arc::new(AtomicBool::new(false)),
            ops,
        })
    }

    /// Create a new worker with default operations (for testing)
    ///
    /// Note: DefaultOps returns errors for fetch operations.
    /// In production, use `new_with_ops` with a real OperationsHandler.
    pub async fn new(
        script: Script,
        limits: Option<RuntimeLimits>,
    ) -> Result<Self, TerminationReason> {
        let ops: OperationsHandle = Arc::new(DefaultOps);
        Self::new_with_ops(script, limits, ops).await
    }

    /// Abort the worker execution
    pub fn abort(&mut self) {
        self.aborted.store(true, Ordering::SeqCst);
        // Boa doesn't have a direct interrupt mechanism
    }

    pub async fn exec(&mut self, mut task: Event) -> Result<(), TerminationReason> {
        // Check if aborted before starting
        if self.aborted.load(Ordering::SeqCst) {
            return Err(TerminationReason::Aborted);
        }

        match task {
            Event::Fetch(ref mut init) => {
                let fetch_init = init.take().ok_or(TerminationReason::Other(
                    "FetchInit already consumed".to_string(),
                ))?;
                let req = fetch_init.req;

                // Collect request body (handles None, Bytes, and Stream)
                let body_bytes: Option<Bytes> = match req.body {
                    RequestBody::None => None,
                    RequestBody::Bytes(b) => Some(b),
                    RequestBody::Stream(mut rx) => {
                        let mut chunks = Vec::new();

                        while let Some(result) = rx.recv().await {
                            if let Ok(bytes) = result {
                                chunks.push(bytes);
                            }
                        }

                        if chunks.is_empty() {
                            None
                        } else {
                            let total: Vec<u8> = chunks.iter().flat_map(|b| b.to_vec()).collect();
                            Some(Bytes::from(total))
                        }
                    }
                };

                // Convert body to string for JS (UTF-8, fallback to latin1 for binary)
                let body_for_js: serde_json::Value = match &body_bytes {
                    Some(b) => {
                        match std::str::from_utf8(b) {
                            Ok(s) => serde_json::Value::String(s.to_string()),
                            // For binary data, encode as array of bytes
                            Err(_) => serde_json::Value::Array(
                                b.iter()
                                    .map(|&byte| serde_json::Value::Number(byte.into()))
                                    .collect(),
                            ),
                        }
                    }
                    None => serde_json::Value::Null,
                };

                // Create request object as JSON
                let request_json = serde_json::json!({
                    "method": req.method,
                    "url": req.url,
                    "headers": req.headers,
                    "body": body_for_js,
                });

                // Trigger fetch event - store response in global variable
                let trigger_script = format!(
                    r#"
                    (async function() {{
                        const requestData = {request_json};

                        // Build Request-like object with body methods
                        const request = {{
                            method: requestData.method,
                            url: requestData.url,
                            headers: requestData.headers,
                            _body: requestData.body,
                            _bodyUsed: false,

                            get bodyUsed() {{
                                return this._bodyUsed;
                            }},

                            async text() {{
                                if (this._bodyUsed) throw new TypeError('Body already consumed');
                                this._bodyUsed = true;
                                if (this._body === null) return '';
                                if (typeof this._body === 'string') return this._body;
                                // Array of bytes -> string
                                if (Array.isArray(this._body)) {{
                                    return String.fromCharCode.apply(null, this._body);
                                }}
                                return String(this._body);
                            }},

                            async json() {{
                                const text = await this.text();
                                return JSON.parse(text);
                            }},

                            async arrayBuffer() {{
                                if (this._bodyUsed) throw new TypeError('Body already consumed');
                                this._bodyUsed = true;
                                if (this._body === null) return new ArrayBuffer(0);
                                let bytes;
                                if (typeof this._body === 'string') {{
                                    bytes = new TextEncoder().encode(this._body);
                                }} else if (Array.isArray(this._body)) {{
                                    bytes = new Uint8Array(this._body);
                                }} else {{
                                    bytes = new TextEncoder().encode(String(this._body));
                                }}
                                return bytes.buffer;
                            }},

                            async bytes() {{
                                const buffer = await this.arrayBuffer();
                                return new Uint8Array(buffer);
                            }},

                            clone() {{
                                if (this._bodyUsed) throw new TypeError('Cannot clone consumed request');
                                return {{
                                    ...this,
                                    _bodyUsed: false,
                                    text: this.text,
                                    json: this.json,
                                    arrayBuffer: this.arrayBuffer,
                                    bytes: this.bytes,
                                    clone: this.clone
                                }};
                            }}
                        }};

                        if (typeof globalThis.__fetchHandler === 'function') {{
                            const event = {{
                                request: request,
                                respondWith: function(response) {{
                                    this._response = response;
                                }}
                            }};

                            // Call handler (may or may not be async)
                            const result = globalThis.__fetchHandler(event);
                            if (result && typeof result.then === 'function') {{
                                await result;
                            }}

                            let response = event._response || new Response("No response");

                            // If respondWith was called with a Promise, await it
                            if (response && typeof response.then === 'function') {{
                                response = await response;
                            }}

                            // Extract body (try multiple methods for compatibility)
                            let bodyText = '';

                            // Try .text() first (standard Response API)
                            if (response.text && typeof response.text === 'function') {{
                                try {{
                                    bodyText = await response.text();
                                }} catch (e) {{
                                    // Ignore .text() errors, fallback to _body
                                }}
                            }}

                            // Fallback to internal _body property (our implementation stores string here)
                            if (!bodyText && response._body !== undefined && response._body !== null) {{
                                bodyText = String(response._body);
                            }}

                            // Extract headers (support both Headers class and plain objects)
                            const headersArray = [];
                            if (response.headers) {{
                                if (response.headers instanceof Headers) {{
                                    for (const [key, value] of response.headers) {{
                                        headersArray.push([key, value]);
                                    }}
                                }} else if (typeof response.headers === 'object') {{
                                    const keys = Object.keys(response.headers);
                                    for (let i = 0; i < keys.length; i++) {{
                                        const key = keys[i];
                                        headersArray.push([key, String(response.headers[key])]);
                                    }}
                                }}
                            }}

                            // Store in global for Rust to read
                            globalThis.__lastResponse = {{
                                status: response.status || 200,
                                body: bodyText,
                                headers: headersArray
                            }};

                            return response;
                        }}
                        throw new Error("No fetch handler registered");
                    }})()
                    "#,
                    request_json = request_json
                );

                // Execute the trigger (returns a Promise)
                let promise_result = self
                    .context
                    .eval(Source::from_bytes(&trigger_script))
                    .map_err(|e| {
                        TerminationReason::Exception(format!(
                            "Fetch handler execution failed: {}",
                            e
                        ))
                    })?;

                // If it's a promise, we need to run jobs until it resolves
                if let Some(promise_obj) = promise_result.as_object() {
                    if let Ok(promise) = JsPromise::from_object(promise_obj.clone()) {
                        // Process jobs until promise settles
                        for _ in 0..200 {
                            let _ = self.context.run_jobs();
                            // Check promise state
                            match promise.state() {
                                boa_engine::builtins::promise::PromiseState::Pending => {
                                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                                }
                                boa_engine::builtins::promise::PromiseState::Fulfilled(_) => {
                                    break;
                                }
                                boa_engine::builtins::promise::PromiseState::Rejected(err) => {
                                    let err_str = err
                                        .to_string(&mut self.context)
                                        .map(|s| s.to_std_string_escaped())
                                        .unwrap_or_else(|_| "Unknown error".to_string());
                                    return Err(TerminationReason::Exception(format!(
                                        "Promise rejected: {}",
                                        err_str
                                    )));
                                }
                            }
                        }
                    } else {
                        // Not a promise, process jobs anyway
                        for _ in 0..50 {
                            let _ = self.context.run_jobs();
                            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        }
                    }
                } else {
                    // Process pending jobs anyway
                    for _ in 0..50 {
                        let _ = self.context.run_jobs();
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                }

                // Read response from global variable
                let response_obj = self
                    .context
                    .global_object()
                    .get(boa_engine::js_string!("__lastResponse"), &mut self.context)
                    .map_err(|e| {
                        TerminationReason::Exception(format!("Failed to get response: {}", e))
                    })?;

                if response_obj.is_undefined() {
                    return Err(TerminationReason::Exception("No response set".to_string()));
                }

                // Extract fields from response object
                let resp_obj = response_obj
                    .as_object()
                    .ok_or(TerminationReason::Exception(
                        "Response is not an object".to_string(),
                    ))?;

                let status = resp_obj
                    .get(boa_engine::js_string!("status"), &mut self.context)
                    .ok()
                    .and_then(|v| v.to_number(&mut self.context).ok())
                    .unwrap_or(200.0) as u16;

                let body = resp_obj
                    .get(boa_engine::js_string!("body"), &mut self.context)
                    .ok()
                    .and_then(|v| v.to_string(&mut self.context).ok())
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_default();

                // Extract headers array
                let mut headers = vec![];
                if let Ok(headers_val) =
                    resp_obj.get(boa_engine::js_string!("headers"), &mut self.context)
                {
                    if let Some(headers_arr) = headers_val.as_object() {
                        if let Ok(length) =
                            headers_arr.get(boa_engine::js_string!("length"), &mut self.context)
                        {
                            if let Ok(len) = length.to_u32(&mut self.context) {
                                for i in 0..len {
                                    if let Ok(item) = headers_arr.get(i, &mut self.context) {
                                        if let Some(pair) = item.as_object() {
                                            let key = pair
                                                .get(0u32, &mut self.context)
                                                .ok()
                                                .and_then(|v| v.to_string(&mut self.context).ok())
                                                .map(|s| s.to_std_string_escaped())
                                                .unwrap_or_default();
                                            let value = pair
                                                .get(1u32, &mut self.context)
                                                .ok()
                                                .and_then(|v| v.to_string(&mut self.context).ok())
                                                .map(|s| s.to_std_string_escaped())
                                                .unwrap_or_default();
                                            if !key.is_empty() {
                                                headers.push((key, value));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                log::debug!(
                    "[Boa Worker] Response received - status: {}, body length: {}, headers: {}",
                    status,
                    body.len(),
                    headers.len()
                );

                // Convert body to stream or None based on whether it's empty
                let http_body = if body.is_empty() {
                    ResponseBody::None
                } else {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    let body_bytes = Bytes::from(body);

                    // Send the body chunk in a background task
                    tokio::spawn(async move {
                        let _ = tx.send(Ok(body_bytes)).await;
                    });

                    ResponseBody::Stream(rx)
                };

                let response = HttpResponse {
                    status,
                    headers,
                    body: http_body,
                };

                let _ = fetch_init.res_tx.send(response);
                Ok(())
            }
            Event::Task(ref mut init) => {
                let task_init = init.take().ok_or(TerminationReason::Other(
                    "TaskInit already consumed".to_string(),
                ))?;

                // Extract scheduled time from source if available
                let scheduled_time = match &task_init.source {
                    Some(openworkers_core::TaskSource::Schedule { time }) => *time,
                    _ => 0,
                };

                // Serialize payload for JS
                let payload_json = task_init
                    .payload
                    .as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "null".to_string());

                // Trigger task event
                let trigger_script = format!(
                    r#"
                    (async function() {{
                        if (typeof globalThis.__scheduledHandler === 'function') {{
                            const event = {{
                                scheduledTime: {scheduled_time},
                                taskId: "{task_id}",
                                payload: {payload_json},
                                attempt: {attempt},
                                cron: '',
                                waitUntil: function(promise) {{
                                    this._promise = promise;
                                }},
                                _result: {{ success: true, data: null, error: null }}
                            }};

                            try {{
                                const result = globalThis.__scheduledHandler(event);

                                // Wait for the handler to complete
                                if (result && typeof result.then === 'function') {{
                                    const resolved = await result;
                                    if (resolved !== undefined) {{
                                        event._result.data = resolved;
                                    }}
                                }}

                                // Wait for waitUntil promise if provided
                                if (event._promise && typeof event._promise.then === 'function') {{
                                    await event._promise;
                                }}

                                globalThis.__lastTaskResult = event._result;
                                return true;
                            }} catch (e) {{
                                globalThis.__lastTaskResult = {{
                                    success: false,
                                    data: null,
                                    error: e.message || String(e)
                                }};
                                return false;
                            }}
                        }}
                        globalThis.__lastTaskResult = {{
                            success: false,
                            data: null,
                            error: "No scheduled handler registered"
                        }};
                        return false;
                    }})()
                    "#,
                    scheduled_time = scheduled_time,
                    task_id = task_init.task_id,
                    payload_json = payload_json,
                    attempt = task_init.attempt,
                );

                // Execute the trigger (returns a Promise)
                let promise_result = self
                    .context
                    .eval(Source::from_bytes(&trigger_script))
                    .map_err(|e| {
                        TerminationReason::Exception(format!(
                            "Task handler execution failed: {}",
                            e
                        ))
                    })?;

                // Process jobs until promise settles
                if let Some(promise_obj) = promise_result.as_object() {
                    if let Ok(promise) = JsPromise::from_object(promise_obj.clone()) {
                        for _ in 0..200 {
                            let _ = self.context.run_jobs();

                            match promise.state() {
                                boa_engine::builtins::promise::PromiseState::Pending => {
                                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                                }
                                boa_engine::builtins::promise::PromiseState::Fulfilled(_) => {
                                    break;
                                }
                                boa_engine::builtins::promise::PromiseState::Rejected(err) => {
                                    let err_str = err
                                        .to_string(&mut self.context)
                                        .map(|s| s.to_std_string_escaped())
                                        .unwrap_or_else(|_| "Unknown error".to_string());

                                    let _ = task_init.res_tx.send(TaskResult::err(err_str.clone()));
                                    return Err(TerminationReason::Exception(format!(
                                        "Task promise rejected: {}",
                                        err_str
                                    )));
                                }
                            }
                        }
                    }
                }

                // Read result from global variable
                let task_result = match self.context.global_object().get(
                    boa_engine::js_string!("__lastTaskResult"),
                    &mut self.context,
                ) {
                    Ok(result_val) => {
                        if let Some(obj) = result_val.as_object() {
                            let success = obj
                                .get(boa_engine::js_string!("success"), &mut self.context)
                                .ok()
                                .map(|v| v.to_boolean())
                                .unwrap_or(true);

                            if success {
                                TaskResult::success()
                            } else {
                                let error = obj
                                    .get(boa_engine::js_string!("error"), &mut self.context)
                                    .ok()
                                    .filter(|v| !v.is_null_or_undefined())
                                    .and_then(|v| v.to_string(&mut self.context).ok())
                                    .map(|s| s.to_std_string_escaped())
                                    .unwrap_or_else(|| "Unknown error".to_string());

                                TaskResult::err(error)
                            }
                        } else {
                            TaskResult::success()
                        }
                    }
                    Err(_) => TaskResult::success(),
                };

                // Send result through channel
                let _ = task_init.res_tx.send(task_result);
                Ok(())
            }
        }
    }
}

/// Setup console that routes logs through OperationsHandler.
/// This ensures logs go through the runner, not directly to stdout/stderr.
fn setup_console_with_ops(
    context: &mut Context,
    ops: OperationsHandle,
) -> Result<(), boa_engine::JsError> {
    use boa_engine::{JsValue, NativeFunction, js_string, property::Attribute};

    // Helper to create a log function for a specific level
    fn make_log_fn(ops: OperationsHandle, level: LogLevel) -> NativeFunction {
        // SAFETY: The closure captures only Send+Sync types (Arc, LogLevel)
        // and doesn't hold references to local stack variables
        unsafe {
            NativeFunction::from_closure(move |_this, args, context| {
                // Format arguments as strings
                let message = args
                    .iter()
                    .map(|arg| {
                        arg.to_string(context)
                            .map(|s| s.to_std_string_escaped())
                            .unwrap_or_else(|_| "[object]".to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ");

                // Fire-and-forget: send log to ops handler
                let ops = ops.clone();
                let level = level;
                tokio::spawn(async move {
                    let _ = ops.handle(Operation::Log { level, message }).await;
                });

                Ok(JsValue::undefined())
            })
        }
    }

    // Create console object
    let console = boa_engine::object::ObjectInitializer::new(context)
        .function(
            make_log_fn(ops.clone(), LogLevel::Info),
            js_string!("log"),
            0,
        )
        .function(
            make_log_fn(ops.clone(), LogLevel::Info),
            js_string!("info"),
            0,
        )
        .function(
            make_log_fn(ops.clone(), LogLevel::Warn),
            js_string!("warn"),
            0,
        )
        .function(
            make_log_fn(ops.clone(), LogLevel::Error),
            js_string!("error"),
            0,
        )
        .function(
            make_log_fn(ops.clone(), LogLevel::Debug),
            js_string!("debug"),
            0,
        )
        .function(
            make_log_fn(ops.clone(), LogLevel::Trace),
            js_string!("trace"),
            0,
        )
        .build();

    // Register as globalThis.console
    context.register_global_property(js_string!("console"), console, Attribute::all())?;

    Ok(())
}

/// Setup crypto global with getRandomValues, randomUUID, and subtle.digest
fn setup_crypto(context: &mut Context) -> Result<(), boa_engine::JsError> {
    use boa_engine::{JsValue, NativeFunction, js_string, property::Attribute};
    use ring::{digest, rand};

    // Create crypto object
    let crypto =
        boa_engine::object::ObjectInitializer::new(context)
            // crypto.randomUUID()
            .function(
                NativeFunction::from_copy_closure(|_this, _args, _ctx| {
                    let uuid = uuid::Uuid::new_v4().to_string();
                    Ok(JsValue::from(boa_engine::JsString::from(uuid)))
                }),
                js_string!("randomUUID"),
                0,
            )
            // crypto._getRandomValues(array) - modifies in place
            .function(
                NativeFunction::from_copy_closure(|_this, args, ctx| {
                    if let Some(array) = args.get(0).and_then(|v| v.as_object()) {
                        // Get the underlying ArrayBuffer
                        if let Ok(buffer_val) = array.get(js_string!("buffer"), ctx) {
                            if let Some(buffer_obj) = buffer_val.as_object() {
                                if let Ok(ab) =
                                    boa_engine::object::builtins::JsArrayBuffer::from_object(
                                        buffer_obj.clone(),
                                    )
                                {
                                    let rng = rand::SystemRandom::new();
                                    let len = ab.data().map(|d| d.len()).unwrap_or(0);
                                    let mut bytes = vec![0u8; len];

                                    if rand::SecureRandom::fill(&rng, &mut bytes).is_ok() {
                                        if let Some(mut data) = ab.data_mut() {
                                            data.copy_from_slice(&bytes);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(JsValue::undefined())
                }),
                js_string!("_getRandomValues"),
                1,
            )
            .build();

    // Create crypto.subtle object
    let subtle =
        boa_engine::object::ObjectInitializer::new(context)
            // crypto.subtle.__nativeDigest(algorithm, data) -> hex string
            .function(
                NativeFunction::from_copy_closure(|_this, args, ctx| {
                    let algo = args
                        .get(0)
                        .and_then(|v| v.to_string(ctx).ok())
                        .map(|s| s.to_std_string_escaped())
                        .unwrap_or_default();

                    let data: Vec<u8> = if let Some(arr) = args.get(1).and_then(|v| v.as_object()) {
                        // Try to get bytes from TypedArray
                        if let Ok(buffer_val) = arr.get(js_string!("buffer"), ctx) {
                            if let Some(buffer_obj) = buffer_val.as_object() {
                                if let Ok(ab) =
                                    boa_engine::object::builtins::JsArrayBuffer::from_object(
                                        buffer_obj.clone(),
                                    )
                                {
                                    ab.data().map(|d| d.to_vec()).unwrap_or_default()
                                } else {
                                    Vec::new()
                                }
                            } else {
                                Vec::new()
                            }
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    };

                    let algorithm = match algo.to_uppercase().as_str() {
                        "SHA-1" => &digest::SHA1_FOR_LEGACY_USE_ONLY,
                        "SHA-256" => &digest::SHA256,
                        "SHA-384" => &digest::SHA384,
                        "SHA-512" => &digest::SHA512,
                        _ => {
                            return Err(boa_engine::JsNativeError::error()
                                .with_message(format!("Unsupported algorithm: {}", algo))
                                .into());
                        }
                    };

                    let result = digest::digest(algorithm, &data);
                    let hex: String = result
                        .as_ref()
                        .iter()
                        .map(|b| format!("{:02x}", b))
                        .collect();

                    Ok(JsValue::from(boa_engine::JsString::from(hex)))
                }),
                js_string!("__nativeDigest"),
                2,
            )
            .build();

    // Add subtle to crypto
    crypto.set(js_string!("subtle"), subtle, false, context)?;

    // Register crypto globally
    context.register_global_property(js_string!("crypto"), crypto, Attribute::all())?;

    // JS wrappers
    context.eval(boa_engine::Source::from_bytes(
        r#"
        // Wrapper for getRandomValues
        (function() {
            const _native = crypto._getRandomValues;
            crypto.getRandomValues = function(array) {
                _native(array);
                return array;
            };
            delete crypto._getRandomValues;
        })();

        // Wrapper for subtle.digest
        crypto.subtle.digest = function(algorithm, data) {
            return new Promise((resolve, reject) => {
                try {
                    let bytes;
                    if (data instanceof ArrayBuffer) {
                        bytes = new Uint8Array(data);
                    } else if (data instanceof Uint8Array) {
                        bytes = data;
                    } else {
                        reject(new Error('Data must be ArrayBuffer or Uint8Array'));
                        return;
                    }
                    const algoName = typeof algorithm === 'string' ? algorithm : algorithm.name;
                    const hexResult = crypto.subtle.__nativeDigest(algoName, bytes);

                    // Convert hex to ArrayBuffer
                    const len = hexResult.length / 2;
                    const buffer = new ArrayBuffer(len);
                    const view = new Uint8Array(buffer);
                    for (let i = 0; i < len; i++) {
                        view[i] = parseInt(hexResult.substr(i * 2, 2), 16);
                    }
                    resolve(buffer);
                } catch (e) {
                    reject(e);
                }
            });
        };
        "#,
    ))?;

    Ok(())
}

/// Setup TextEncoder and TextDecoder APIs
fn setup_text_encoding(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        // TextEncoder - encode strings to UTF-8 bytes
        globalThis.TextEncoder = class TextEncoder {
            constructor() {
                this.encoding = 'utf-8';
            }

            encode(input) {
                const str = String(input || '');
                const bytes = [];

                // UTF-8 encoding with proper surrogate pair handling
                for (let i = 0; i < str.length; i++) {
                    let code = str.codePointAt(i);

                    // Skip low surrogate (already processed with high surrogate)
                    if (code > 0xFFFF) i++;

                    if (code < 0x80) {
                        bytes.push(code);
                    } else if (code < 0x800) {
                        bytes.push(0xC0 | (code >> 6));
                        bytes.push(0x80 | (code & 0x3F));
                    } else if (code < 0x10000) {
                        bytes.push(0xE0 | (code >> 12));
                        bytes.push(0x80 | ((code >> 6) & 0x3F));
                        bytes.push(0x80 | (code & 0x3F));
                    } else {
                        bytes.push(0xF0 | (code >> 18));
                        bytes.push(0x80 | ((code >> 12) & 0x3F));
                        bytes.push(0x80 | ((code >> 6) & 0x3F));
                        bytes.push(0x80 | (code & 0x3F));
                    }
                }

                return new Uint8Array(bytes);
            }
        };

        // TextDecoder - decode UTF-8 bytes to strings
        globalThis.TextDecoder = class TextDecoder {
            constructor(encoding = 'utf-8') {
                this.encoding = encoding.toLowerCase();
                if (this.encoding !== 'utf-8' && this.encoding !== 'utf8') {
                    throw new RangeError('Only UTF-8 encoding is supported');
                }
            }

            decode(input) {
                if (!input) return '';

                // Convert to Uint8Array if needed
                const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
                const chars = [];

                // Simple UTF-8 decoding
                let i = 0;
                while (i < bytes.length) {
                    const byte1 = bytes[i++];

                    if (byte1 < 0x80) {
                        // 1-byte character (ASCII)
                        chars.push(String.fromCharCode(byte1));
                    } else if ((byte1 & 0xE0) === 0xC0) {
                        // 2-byte character
                        const byte2 = bytes[i++];
                        const code = ((byte1 & 0x1F) << 6) | (byte2 & 0x3F);
                        chars.push(String.fromCharCode(code));
                    } else if ((byte1 & 0xF0) === 0xE0) {
                        // 3-byte character
                        const byte2 = bytes[i++];
                        const byte3 = bytes[i++];
                        const code = ((byte1 & 0x0F) << 12) | ((byte2 & 0x3F) << 6) | (byte3 & 0x3F);
                        chars.push(String.fromCharCode(code));
                    } else if ((byte1 & 0xF8) === 0xF0) {
                        // 4-byte character (emojis, etc.)
                        const byte2 = bytes[i++];
                        const byte3 = bytes[i++];
                        const byte4 = bytes[i++];
                        const code = ((byte1 & 0x07) << 18) | ((byte2 & 0x3F) << 12) |
                                    ((byte3 & 0x3F) << 6) | (byte4 & 0x3F);
                        chars.push(String.fromCodePoint(code));
                    } else {
                        // Invalid UTF-8, skip
                        chars.push('\uFFFD'); // Replacement character
                    }
                }

                return chars.join('');
            }
        };
        "#,
    ))?;

    Ok(())
}

impl openworkers_core::Worker for Worker {
    async fn new(script: Script, limits: Option<RuntimeLimits>) -> Result<Self, TerminationReason> {
        Worker::new(script, limits).await
    }

    async fn exec(&mut self, task: Event) -> Result<(), TerminationReason> {
        Worker::exec(self, task).await
    }

    fn abort(&mut self) {
        Worker::abort(self)
    }
}
