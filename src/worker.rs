use crate::compat::{Script, TerminationReason};
use crate::task::{HttpResponse, Task};
use boa_engine::{
    Context, Finalize, JsData, JsResult, JsString, Source, Trace, object::builtins::JsPromise,
};
use boa_runtime::RuntimeExtension;
use boa_runtime::fetch::{Fetcher, request::JsRequest, response::JsResponse};
use bytes::Bytes;
use std::cell::RefCell;
use std::rc::Rc;

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
}

impl Worker {
    pub async fn new(
        script: Script,
        _log_tx: Option<std::sync::mpsc::Sender<crate::compat::LogEvent>>,
        _limits: Option<crate::compat::RuntimeLimits>,
    ) -> Result<Self, String> {
        let mut context = Context::default();

        // Register console
        boa_runtime::extensions::ConsoleExtension::default()
            .register(None, &mut context)
            .map_err(|e| format!("Failed to register console: {}", e))?;

        // Register timers
        boa_runtime::extensions::TimeoutExtension
            .register(None, &mut context)
            .map_err(|e| format!("Failed to register timers: {}", e))?;

        // Register URL API
        boa_runtime::extensions::UrlExtension
            .register(None, &mut context)
            .map_err(|e| format!("Failed to register URL: {}", e))?;

        // Register fetch with our custom fetcher using spawn_blocking
        boa_runtime::extensions::FetchExtension(SpawnBlockingFetcher)
            .register(None, &mut context)
            .map_err(|e| format!("Failed to register fetch: {}", e))?;

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
            .map_err(|e| format!("Failed to setup Response workaround: {}", e))?;

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
            .map_err(|e| format!("Setup failed: {}", e))?;

        // Evaluate user script
        context
            .eval(Source::from_bytes(&script.code))
            .map_err(|e| format!("Script evaluation failed: {}", e))?;

        Ok(Self { context })
    }

    pub async fn exec(&mut self, mut task: Task) -> Result<TerminationReason, String> {
        match task {
            Task::Fetch(ref mut init) => {
                let fetch_init = init.take().ok_or("FetchInit already consumed")?;
                let req = &fetch_init.req;

                // Create request object as JSON
                let request_json = serde_json::json!({
                    "method": req.method,
                    "url": req.url,
                    "headers": req.headers,
                });

                // Trigger fetch event - store response in global variable
                let trigger_script = format!(
                    r#"
                    (async function() {{
                        const request = {request_json};
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
                    .map_err(|e| format!("Fetch handler execution failed: {}", e))?;

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
                                    return Err(format!("Promise rejected: {}", err_str));
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
                    .map_err(|e| format!("Failed to get response: {}", e))?;

                if response_obj.is_undefined() {
                    return Err("No response set".to_string());
                }

                // Extract fields from response object
                let resp_obj = response_obj
                    .as_object()
                    .ok_or("Response is not an object")?;

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

                let response = HttpResponse {
                    status,
                    headers,
                    body: crate::task::ResponseBody::Bytes(Bytes::from(body)),
                };

                let _ = fetch_init.res_tx.send(response);
                Ok(TerminationReason::Success)
            }
            Task::Scheduled(ref mut init) => {
                let scheduled_init = init.take().ok_or("ScheduledInit already consumed")?;

                // Trigger scheduled event
                let trigger_script = format!(
                    r#"
                    (async function() {{
                        if (typeof globalThis.__scheduledHandler === 'function') {{
                            const event = {{
                                scheduledTime: {},
                                cron: '',
                                waitUntil: function(promise) {{
                                    this._promise = promise;
                                }}
                            }};

                            const result = globalThis.__scheduledHandler(event);

                            // Wait for the handler to complete
                            if (result && typeof result.then === 'function') {{
                                await result;
                            }}

                            // Wait for waitUntil promise if provided
                            if (event._promise && typeof event._promise.then === 'function') {{
                                await event._promise;
                            }}

                            return true;
                        }}
                        return false;
                    }})()
                    "#,
                    scheduled_init.time
                );

                // Execute the trigger (returns a Promise)
                let promise_result = self
                    .context
                    .eval(Source::from_bytes(&trigger_script))
                    .map_err(|e| format!("Scheduled handler execution failed: {}", e))?;

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
                                    return Err(format!("Scheduled promise rejected: {}", err_str));
                                }
                            }
                        }
                    }
                }

                // Send success signal through channel
                let _ = scheduled_init.res_tx.send(());
                Ok(TerminationReason::Success)
            }
        }
    }
}
