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

            // Override Response constructor to make it actually work
            globalThis.Response = function(body, init) {
                init = init || {};
                this.body = String(body || '');
                this.status = init.status || 200;
                this.headers = init.headers || {};
                this.text = async function() {
                    return this.body;
                };
            };

            // Keep native fetch (uses NativeResponse internally)
            globalThis.fetch = nativeFetch;
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
                                    // Ignore .text() errors, fallback to .body
                                }}
                            }}

                            // Fallback to .body property (our simple implementation)
                            if (!bodyText && response.body !== undefined) {{
                                bodyText = String(response.body);
                            }}

                            // Extract headers
                            const headersArray = [];
                            if (response.headers && typeof response.headers === 'object') {{
                                for (const key in response.headers) {{
                                    if (response.headers.hasOwnProperty(key)) {{
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
