use boa_engine::{
    Context, Finalize, JsData, JsResult, JsString, Source, Trace, object::builtins::JsPromise,
};
use boa_runtime::RuntimeExtension;
use boa_runtime::fetch::{Fetcher, request::JsRequest, response::JsResponse};
use bytes::Bytes;
use openworkers_core::{
    Event, HttpResponse, RequestBody, ResponseBody, RuntimeLimits, Script, TaskResult,
    TerminationReason,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// Custom fetcher that uses spawn_blocking to avoid blocking tokio runtime
#[derive(Clone, Debug, Trace, Finalize, JsData, Default)]
struct SpawnBlockingFetcher;

impl Fetcher for SpawnBlockingFetcher {
    async fn fetch(
        self: Rc<Self>,
        request: JsRequest,
        _context: &RefCell<&mut Context>,
    ) -> JsResult<JsResponse> {
        let req = request.into_inner();
        let url = req.uri().to_string();
        let url_for_result = url.clone();
        let method = req.method().clone();
        let headers: Vec<_> = req
            .headers()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let body = req.body().to_vec();

        // Execute blocking fetch in a separate thread
        let result = tokio::task::spawn_blocking(move || {
            let client = reqwest::blocking::Client::new();
            let mut req_builder = client.request(method, &url);

            for (key, value) in headers {
                req_builder = req_builder.header(key, value);
            }

            let resp = req_builder.body(body).send()?;
            let status = resp.status();
            let resp_headers = resp.headers().clone();
            let bytes = resp.bytes()?;

            Ok::<_, reqwest::Error>((status, resp_headers, bytes))
        })
        .await
        .map_err(|e| boa_engine::JsError::from_rust(e))?
        .map_err(boa_engine::JsError::from_rust)?;

        let (status, resp_headers, bytes) = result;

        let mut builder = http::Response::builder().status(status.as_u16());
        for k in resp_headers.keys() {
            for v in resp_headers.get_all(k) {
                builder = builder.header(k.as_str(), v);
            }
        }

        builder
            .body(bytes.to_vec())
            .map_err(boa_engine::JsError::from_rust)
            .map(|http_response| JsResponse::basic(JsString::from(url_for_result), http_response))
    }
}

pub struct Worker {
    context: Context,
    aborted: Arc<AtomicBool>,
}

impl Worker {
    pub async fn new(
        script: Script,
        _limits: Option<RuntimeLimits>,
    ) -> Result<Self, TerminationReason> {
        let mut context = Context::default();

        // Register console
        boa_runtime::extensions::ConsoleExtension::default()
            .register(None, &mut context)
            .map_err(|e| {
                TerminationReason::InitializationError(format!("Failed to register console: {}", e))
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

        // Register fetch with our custom fetcher using spawn_blocking
        boa_runtime::extensions::FetchExtension(SpawnBlockingFetcher)
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
        })
    }

    /// Abort the worker execution
    pub fn abort(&mut self) {
        self.aborted.store(true, Ordering::SeqCst);
        // Boa doesn't have a direct interrupt mechanism
    }

    pub async fn exec(&mut self, mut event: Event) -> Result<(), TerminationReason> {
        // Check if aborted before starting
        if self.aborted.load(Ordering::SeqCst) {
            return Err(TerminationReason::Aborted);
        }

        match event {
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

impl openworkers_core::Worker for Worker {
    async fn new(script: Script, limits: Option<RuntimeLimits>) -> Result<Self, TerminationReason> {
        Worker::new(script, limits).await
    }

    async fn exec(&mut self, event: Event) -> Result<(), TerminationReason> {
        Worker::exec(self, event).await
    }

    fn abort(&mut self) {
        Worker::abort(self)
    }
}
