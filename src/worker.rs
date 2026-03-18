use boa_engine::{
    Context, Finalize, JsData, JsValue, NativeFunction, Source, Trace,
    builtins::promise::PromiseState, job::PromiseJob, js_string, object::builtins::JsFunction,
    property::Attribute,
};
use boa_gc::{Gc, GcRefCell};
use bytes::Bytes;
use openworkers_core::{
    DefaultOps, Event, HttpRequest, HttpResponse, OperationsHandle, RequestBody, ResponseBody,
    RuntimeLimits, Script, TaskResult, TerminationReason,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::web_api::setup_web_apis;

/// Event listeners stored on the Rust side (following boa_runtime's pattern)
#[derive(Default, Trace, Finalize, JsData)]
struct EventListeners {
    fetch: Vec<JsFunction>,
    scheduled: Vec<JsFunction>,
}

impl EventListeners {
    fn from_context(context: &mut Context) -> Gc<GcRefCell<Self>> {
        if !context.has_data::<Gc<GcRefCell<EventListeners>>>() {
            context.insert_data(Gc::new(GcRefCell::new(Self::default())));
        }

        context
            .get_data::<Gc<GcRefCell<Self>>>()
            .expect("Should have been inserted")
            .clone()
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
    pub async fn new_with_ops(
        script: Script,
        _limits: Option<RuntimeLimits>,
        ops: OperationsHandle,
    ) -> Result<Self, TerminationReason> {
        let mut context = Context::default();

        eprintln!("[DEBUG] Starting Worker initialization");

        // Setup console that routes to OperationsHandler
        setup_console_with_ops(&mut context, ops.clone()).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register console: {}", e))
        })?;
        eprintln!("[DEBUG] Console setup complete");

        // Setup crypto (getRandomValues, randomUUID, subtle.digest)
        setup_crypto(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register crypto: {}", e))
        })?;
        eprintln!("[DEBUG] Crypto setup complete");

        // Setup TextEncoder/TextDecoder
        setup_text_encoding(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register text encoding: {}",
                e
            ))
        })?;
        eprintln!("[DEBUG] TextEncoding setup complete");

        // Setup timers (setTimeout, setInterval, clearTimeout, clearInterval)
        // Must be before web APIs since AbortSignal.timeout uses setTimeout
        setup_timers(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register timers: {}", e))
        })?;
        eprintln!("[DEBUG] Timers setup complete");

        // Setup ReadableStream (must be before web APIs since Request/Response use it)
        setup_readable_stream(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register ReadableStream: {}",
                e
            ))
        })?;
        eprintln!("[DEBUG] ReadableStream setup complete");

        // Setup Web APIs (Headers, Request, Response, URL, Blob, FormData, etc.)
        setup_web_apis(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register web APIs: {}", e))
        })?;
        eprintln!("[DEBUG] Web APIs setup complete");

        // Setup fetch() global (stub — will be wired to OperationsHandle later)
        setup_fetch_global(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register fetch: {}", e))
        })?;
        eprintln!("[DEBUG] Fetch global setup complete");

        // Setup event handling (addEventListener, dispatchEvent)
        setup_event_handling(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register event handling: {}",
                e
            ))
        })?;

        // Setup response extraction helpers (used by handle_fetch)
        setup_response_extractors(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register response extractors: {}",
                e
            ))
        })?;
        eprintln!("[DEBUG] Event handling setup complete");

        // Evaluate user script
        let js_code = script.code.as_js().ok_or_else(|| {
            TerminationReason::InitializationError(
                "Boa runtime only supports JavaScript code".to_string(),
            )
        })?;

        eprintln!("[DEBUG] Evaluating user script...");
        context.eval(Source::from_bytes(js_code)).map_err(|e| {
            TerminationReason::Exception(format!("Script evaluation failed: {}", e))
        })?;
        eprintln!("[DEBUG] User script evaluation complete");

        Ok(Self {
            context,
            aborted: Arc::new(AtomicBool::new(false)),
            ops,
        })
    }

    /// Create a new worker with DefaultOps (stubs)
    pub async fn new(
        script: Script,
        limits: Option<RuntimeLimits>,
    ) -> Result<Self, TerminationReason> {
        let ops: OperationsHandle = Arc::new(DefaultOps);
        Self::new_with_ops(script, limits, ops).await
    }

    /// Abort execution
    pub fn abort(&mut self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    /// Execute a task
    pub async fn exec(&mut self, mut task: Event) -> Result<(), TerminationReason> {
        if self.aborted.load(Ordering::SeqCst) {
            return Err(TerminationReason::Aborted);
        }

        match &mut task {
            Event::Fetch(init_opt) => {
                let init = init_opt.take().ok_or_else(|| {
                    TerminationReason::Other("FetchInit already taken".to_string())
                })?;
                let response = self.handle_fetch(init.req).await?;
                let _ = init.res_tx.send(response);
                Ok(())
            }
            Event::Task(init_opt) => {
                let init = init_opt.take().ok_or_else(|| {
                    TerminationReason::Other("TaskInit already taken".to_string())
                })?;

                let scheduled_time = match &init.source {
                    Some(openworkers_core::TaskSource::Schedule { time }) => *time,
                    _ => 0,
                };

                match self.handle_scheduled(scheduled_time).await {
                    Ok(()) => {
                        let _ = init.res_tx.send(TaskResult::success());
                        Ok(())
                    }
                    Err(e) => {
                        let _ = init.res_tx.send(TaskResult::err(e.to_string()));
                        Err(e)
                    }
                }
            }
        }
    }

    /// Handle a fetch event
    ///
    /// Dispatch happens entirely in JS to avoid Boa 0.21 environment panics
    /// when calling stored functions from Rust that instantiate Response/ReadableStream.
    async fn handle_fetch(
        &mut self,
        request: HttpRequest,
    ) -> Result<HttpResponse, TerminationReason> {
        let headers_json =
            serde_json::to_string(&request.headers).unwrap_or_else(|_| "{}".to_string());

        let body_str = match &request.body {
            RequestBody::Bytes(b) => String::from_utf8_lossy(b).to_string(),
            RequestBody::None => String::new(),
            RequestBody::Stream(_) => String::new(),
        };

        // Create event, dispatch handlers, extract response — all in JS.
        // Uses async IIFE to properly handle: async handlers, Promise-based respondWith,
        // and error handling. Returns a Promise that resolves to the Response object.
        let dispatch_code = format!(
            r#"(async function() {{
                var handlers = globalThis.__fetchHandlers || [];
                if (handlers.length === 0) throw new Error('__no_handlers__');

                var headers = {{}};
                try {{ headers = JSON.parse('{}'); }} catch(e) {{}}

                var request = new Request('{}', {{
                    method: '{}',
                    headers: headers,
                    body: {}
                }});

                var event = {{
                    type: 'fetch',
                    request: request,
                    _response: null,
                    respondWith: function(r) {{ this._response = r; }}
                }};

                for (var i = 0; i < handlers.length; i++) {{
                    try {{
                        await handlers[i](event);
                    }} catch(e) {{
                        if (!event._response) {{
                            event._response = new Response(
                                'Error: ' + (e.message || e), {{ status: 500 }}
                            );
                        }}
                    }}
                }}

                var response = event._response;
                if (response && typeof response.then === 'function') {{
                    response = await response;
                }}

                return response;
            }})()"#,
            headers_json.replace('\\', "\\\\").replace('\'', "\\'"),
            request.url.replace('\'', "\\'"),
            request.method,
            if body_str.is_empty() {
                "null".to_string()
            } else {
                format!("'{}'", body_str.replace('\\', "\\\\").replace('\'', "\\'"))
            }
        );

        let result = self
            .context
            .eval(Source::from_bytes(dispatch_code.as_bytes()))
            .map_err(|e| {
                let msg = format!("{}", e);

                if msg.contains("__no_handlers__") {
                    return TerminationReason::Other("No fetch handlers registered".to_string());
                }

                TerminationReason::Exception(format!("Dispatch failed: {}", e))
            })?;

        // Run jobs and process pending fetches in a loop until the outer promise resolves
        let _ = self.context.run_jobs();

        // Loop: drain pending fetches → resolve → run_jobs → repeat
        // This handles `await fetch(...)` inside handlers
        for _ in 0..100 {
            let pending_count = self.resolve_pending_fetches().await;

            if pending_count == 0 {
                break;
            }

            let _ = self.context.run_jobs();
        }

        // The result is a Promise — extract the resolved value
        if let Some(promise) = result.as_promise() {
            match promise.state() {
                PromiseState::Fulfilled(val) => self.extract_response_from_js(&val),
                PromiseState::Rejected(err) => Err(TerminationReason::Exception(format!(
                    "Fetch handler rejected: {}",
                    err.display()
                ))),
                PromiseState::Pending => Err(TerminationReason::Exception(
                    "Fetch handler did not complete".to_string(),
                )),
            }
        } else {
            self.extract_response_from_js(&result)
        }
    }

    /// Drain pending fetch requests from __pendingFetches, execute them via ops,
    /// and resolve/reject their promises. Returns the number of fetches processed.
    async fn resolve_pending_fetches(&mut self) -> usize {
        // Read __pendingFetches array
        let pending_arr = match self
            .context
            .global_object()
            .get(js_string!("__pendingFetches"), &mut self.context)
        {
            Ok(val) => val,
            Err(_) => return 0,
        };

        let arr_obj = match pending_arr.as_object() {
            Some(obj) => obj.clone(),
            None => return 0,
        };

        let len = arr_obj
            .get(js_string!("length"), &mut self.context)
            .ok()
            .and_then(|v| v.to_u32(&mut self.context).ok())
            .unwrap_or(0) as usize;

        if len == 0 {
            return 0;
        }

        // Collect all pending fetch entries
        let mut entries = Vec::with_capacity(len);

        for i in 0..len {
            let entry = match arr_obj.get(i as u32, &mut self.context) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let entry_obj = match entry.as_object() {
                Some(obj) => obj,
                None => continue,
            };

            let url = entry_obj
                .get(js_string!("url"), &mut self.context)
                .ok()
                .and_then(|v| {
                    v.to_string(&mut self.context)
                        .ok()
                        .map(|s| s.to_std_string_escaped())
                })
                .unwrap_or_default();

            let method = entry_obj
                .get(js_string!("method"), &mut self.context)
                .ok()
                .and_then(|v| {
                    v.to_string(&mut self.context)
                        .ok()
                        .map(|s| s.to_std_string_escaped())
                })
                .unwrap_or_else(|| "GET".to_string());

            let body_val = entry_obj
                .get(js_string!("body"), &mut self.context)
                .ok()
                .and_then(|v| {
                    if v.is_null_or_undefined() {
                        None
                    } else {
                        v.to_string(&mut self.context)
                            .ok()
                            .map(|s| s.to_std_string_escaped())
                    }
                });

            // Extract headers as HashMap
            let mut headers = std::collections::HashMap::new();

            if let Ok(headers_val) = entry_obj.get(js_string!("headers"), &mut self.context) {
                if let Some(headers_obj) = headers_val.as_object() {
                    if let Ok(keys) = headers_obj.own_property_keys(&mut self.context) {
                        for key in keys {
                            let key_str = key.to_string();

                            if let Ok(val) = headers_obj.get(key, &mut self.context) {
                                let val_str = val
                                    .to_string(&mut self.context)
                                    .map(|s| s.to_std_string_escaped())
                                    .unwrap_or_default();
                                headers.insert(key_str, val_str);
                            }
                        }
                    }
                }
            }

            let resolve: Option<JsFunction> = entry_obj
                .get(js_string!("resolve"), &mut self.context)
                .ok()
                .and_then(|v| {
                    let obj = v.as_object()?.clone();
                    JsFunction::from_object(obj)
                });

            let reject: Option<JsFunction> = entry_obj
                .get(js_string!("reject"), &mut self.context)
                .ok()
                .and_then(|v| {
                    let obj = v.as_object()?.clone();
                    JsFunction::from_object(obj)
                });

            if let (Some(resolve), Some(reject)) = (resolve, reject) {
                let http_method = match method.as_str() {
                    "GET" => openworkers_core::HttpMethod::Get,
                    "POST" => openworkers_core::HttpMethod::Post,
                    "PUT" => openworkers_core::HttpMethod::Put,
                    "DELETE" => openworkers_core::HttpMethod::Delete,
                    "PATCH" => openworkers_core::HttpMethod::Patch,
                    "HEAD" => openworkers_core::HttpMethod::Head,
                    "OPTIONS" => openworkers_core::HttpMethod::Options,
                    _ => openworkers_core::HttpMethod::Get,
                };

                let body = match body_val {
                    Some(b) => RequestBody::Bytes(Bytes::from(b)),
                    None => RequestBody::None,
                };

                entries.push((
                    HttpRequest {
                        url,
                        method: http_method,
                        headers,
                        body,
                    },
                    resolve,
                    reject,
                ));
            }
        }

        // Clear the array
        let _ = self.context.eval(Source::from_bytes(
            b"globalThis.__pendingFetches.length = 0;",
        ));

        let count = entries.len();

        // Execute each fetch via ops and resolve/reject
        for (request, resolve, reject) in entries {
            match self.ops.handle_fetch(request).await {
                Ok(response) => {
                    // Build a JS Response object from the HttpResponse
                    let response_code = self.build_response_js(response);

                    match self
                        .context
                        .eval(Source::from_bytes(response_code.as_bytes()))
                    {
                        Ok(js_response) => {
                            let _ = resolve.call(
                                &JsValue::undefined(),
                                &[js_response],
                                &mut self.context,
                            );
                        }
                        Err(e) => {
                            let err_msg = JsValue::from(js_string!(format!(
                                "Failed to construct response: {}",
                                e
                            )));
                            let _ =
                                reject.call(&JsValue::undefined(), &[err_msg], &mut self.context);
                        }
                    }
                }
                Err(e) => {
                    let err_msg = JsValue::from(js_string!(format!("fetch failed: {}", e)));
                    let _ = reject.call(&JsValue::undefined(), &[err_msg], &mut self.context);
                }
            }
        }

        count
    }

    /// Build JS code to create a Response object from an HttpResponse
    fn build_response_js(&self, response: HttpResponse) -> String {
        let body_str = match &response.body {
            ResponseBody::Bytes(b) => {
                let s = String::from_utf8_lossy(b);
                format!(
                    "'{}'",
                    s.replace('\\', "\\\\")
                        .replace('\'', "\\'")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                )
            }
            ResponseBody::None => "null".to_string(),
            ResponseBody::Stream(_) => "null".to_string(),
        };

        let headers_obj: String = response
            .headers
            .iter()
            .map(|(k, v)| format!("'{}': '{}'", k.replace('\'', "\\'"), v.replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            "(new Response({}, {{ status: {}, headers: {{{}}} }}))",
            body_str, response.status, headers_obj
        )
    }

    /// Extract HttpResponse from a JS Response object (our web_api.rs Response class)
    /// Reads status, headers._map, and body._queue via Rust API — no eval needed.
    fn extract_response_from_js(
        &mut self,
        value: &JsValue,
    ) -> Result<HttpResponse, TerminationReason> {
        let resp_obj = match value.as_object() {
            Some(obj) => obj,
            None => {
                // No response set (respondWith not called)
                return Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: ResponseBody::None,
                });
            }
        };

        // Status
        let status = resp_obj
            .get(js_string!("status"), &mut self.context)
            .ok()
            .and_then(|v| v.to_u32(&mut self.context).ok())
            .unwrap_or(200) as u16;

        // Headers: our Headers class stores data in _map (a JS Map)
        let mut headers = Vec::new();

        if let Ok(headers_val) = resp_obj.get(js_string!("headers"), &mut self.context) {
            if let Some(headers_obj) = headers_val.as_object() {
                // Call __extractHeaders helper registered at init
                if let Ok(extractor) = self
                    .context
                    .global_object()
                    .get(js_string!("__extractHeaders"), &mut self.context)
                {
                    if let Some(extractor_fn) = extractor
                        .as_object()
                        .and_then(|o| JsFunction::from_object(o.clone()))
                    {
                        if let Ok(result) = extractor_fn.call(
                            &JsValue::undefined(),
                            &[headers_obj.clone().into()],
                            &mut self.context,
                        ) {
                            // Result is a flat array: [key, value, key, value, ...]
                            if let Some(arr) = result.as_object() {
                                let len = arr
                                    .get(js_string!("length"), &mut self.context)
                                    .ok()
                                    .and_then(|v| v.to_u32(&mut self.context).ok())
                                    .unwrap_or(0);

                                let mut i = 0;

                                while i + 1 < len {
                                    let key = arr
                                        .get(i, &mut self.context)
                                        .ok()
                                        .and_then(|v| {
                                            v.to_string(&mut self.context)
                                                .ok()
                                                .map(|s| s.to_std_string_escaped())
                                        })
                                        .unwrap_or_default();

                                    let val = arr
                                        .get(i + 1, &mut self.context)
                                        .ok()
                                        .and_then(|v| {
                                            v.to_string(&mut self.context)
                                                .ok()
                                                .map(|s| s.to_std_string_escaped())
                                        })
                                        .unwrap_or_default();

                                    headers.push((key, val));
                                    i += 2;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Body: our Response stores body as a ReadableStream with an internal _queue
        // Call __extractBody helper registered at init
        let mut body = ResponseBody::None;

        if let Ok(body_val) = resp_obj.get(js_string!("body"), &mut self.context) {
            if body_val.is_object() {
                if let Ok(extractor) = self
                    .context
                    .global_object()
                    .get(js_string!("__extractBody"), &mut self.context)
                {
                    if let Some(extractor_fn) = extractor
                        .as_object()
                        .and_then(|o| JsFunction::from_object(o.clone()))
                    {
                        if let Ok(result) =
                            extractor_fn.call(&JsValue::undefined(), &[body_val], &mut self.context)
                        {
                            let body_str = result
                                .to_string(&mut self.context)
                                .map(|s| s.to_std_string_escaped())
                                .unwrap_or_default();

                            if !body_str.is_empty() {
                                body = ResponseBody::Bytes(Bytes::from(body_str));
                            }
                        }
                    }
                }
            }
        }

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    /// Handle a scheduled event
    async fn handle_scheduled(&mut self, time: u64) -> Result<(), TerminationReason> {
        // Get the scheduled event listeners from Rust storage
        let listeners = EventListeners::from_context(&mut self.context);
        let scheduled_handlers: Vec<JsFunction> = listeners.borrow().scheduled.clone();

        if scheduled_handlers.is_empty() {
            // No scheduled handlers, just return success
            return Ok(());
        }

        // Create the scheduled event object
        let setup_code = format!(
            r#"(function() {{
                globalThis.__currentScheduledEvent = {{
                    type: 'scheduled',
                    scheduledTime: {},
                    _waitUntilPromises: [],
                    waitUntil: function(promise) {{
                        this._waitUntilPromises.push(promise);
                    }}
                }};
                return globalThis.__currentScheduledEvent;
            }})()"#,
            time
        );

        let event_value = self
            .context
            .eval(Source::from_bytes(setup_code.as_bytes()))
            .map_err(|e| {
                TerminationReason::Exception(format!("Failed to create scheduled event: {}", e))
            })?;

        // Enqueue handlers as jobs
        for handler in scheduled_handlers {
            let event_val = event_value.clone();

            let job = PromiseJob::new(move |context| {
                handler.call(&JsValue::undefined(), &[event_val.clone()], context)
            });

            self.context.enqueue_job(job.into());
        }

        // Run the jobs
        if let Err(e) = self.context.run_jobs() {
            return Err(TerminationReason::Exception(format!(
                "Scheduled handler error: {}",
                e
            )));
        }

        // Wait for waitUntil promises
        let wait_code = r#"(async function() {
            var event = globalThis.__currentScheduledEvent;
            if (event && event._waitUntilPromises.length > 0) {
                await Promise.all(event._waitUntilPromises);
            }
            return true;
        })()"#;

        let result = self.context.eval(Source::from_bytes(wait_code.as_bytes()));

        match result {
            Ok(value) => {
                if let Some(promise) = value.as_promise() {
                    let _ = self.context.run_jobs();

                    match promise.state() {
                        PromiseState::Fulfilled(_) => Ok(()),
                        PromiseState::Rejected(err) => Err(TerminationReason::Exception(format!(
                            "Scheduled handler rejected: {}",
                            err.display()
                        ))),
                        PromiseState::Pending => Err(TerminationReason::Exception(
                            "Promise still pending".to_string(),
                        )),
                    }
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(TerminationReason::Exception(format!(
                "Scheduled wait failed: {}",
                e
            ))),
        }
    }
}

// ============================================================================
// Setup functions
// ============================================================================

/// Setup console (basic implementation using eprintln)
/// Note: OperationsHandler logging integration would require unsafe code in Boa
fn setup_console_with_ops(
    context: &mut Context,
    _ops: OperationsHandle,
) -> Result<(), boa_engine::JsError> {
    let console = boa_engine::object::ObjectInitializer::new(context)
        .function(
            NativeFunction::from_copy_closure(|_this, args, ctx| {
                let msg = args_to_string(args, ctx);
                eprintln!("{}", msg);
                Ok(JsValue::undefined())
            }),
            js_string!("log"),
            0,
        )
        .function(
            NativeFunction::from_copy_closure(|_this, args, ctx| {
                let msg = args_to_string(args, ctx);
                eprintln!("[WARN] {}", msg);
                Ok(JsValue::undefined())
            }),
            js_string!("warn"),
            0,
        )
        .function(
            NativeFunction::from_copy_closure(|_this, args, ctx| {
                let msg = args_to_string(args, ctx);
                eprintln!("[ERROR] {}", msg);
                Ok(JsValue::undefined())
            }),
            js_string!("error"),
            0,
        )
        .function(
            NativeFunction::from_copy_closure(|_this, args, ctx| {
                let msg = args_to_string(args, ctx);
                eprintln!("[INFO] {}", msg);
                Ok(JsValue::undefined())
            }),
            js_string!("info"),
            0,
        )
        .function(
            NativeFunction::from_copy_closure(|_this, args, ctx| {
                let msg = args_to_string(args, ctx);
                eprintln!("[DEBUG] {}", msg);
                Ok(JsValue::undefined())
            }),
            js_string!("debug"),
            0,
        )
        .build();

    context.register_global_property(js_string!("console"), console, Attribute::all())?;
    Ok(())
}

fn args_to_string(args: &[JsValue], ctx: &mut Context) -> String {
    args.iter()
        .map(|arg| {
            arg.to_string(ctx)
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_else(|_| "[object]".to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Setup crypto global
fn setup_crypto(context: &mut Context) -> Result<(), boa_engine::JsError> {
    use ring::{digest, rand};

    let crypto =
        boa_engine::object::ObjectInitializer::new(context)
            .function(
                NativeFunction::from_copy_closure(|_this, _args, _ctx| {
                    let uuid = uuid::Uuid::new_v4().to_string();
                    Ok(JsValue::from(boa_engine::JsString::from(uuid)))
                }),
                js_string!("randomUUID"),
                0,
            )
            .function(
                NativeFunction::from_copy_closure(|_this, args, ctx| {
                    if let Some(array) = args.first().and_then(|v| v.as_object()) {
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

    // Create crypto.subtle
    let subtle =
        boa_engine::object::ObjectInitializer::new(context)
            .function(
                NativeFunction::from_copy_closure(|_this, args, ctx| {
                    let algo = args
                        .first()
                        .and_then(|v| v.to_string(ctx).ok())
                        .map(|s| s.to_std_string_escaped())
                        .unwrap_or_default();

                    let data: Vec<u8> = if let Some(arr) = args.get(1).and_then(|v| v.as_object()) {
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

    crypto.set(js_string!("subtle"), subtle, false, context)?;
    context.register_global_property(js_string!("crypto"), crypto, Attribute::all())?;

    // JS wrappers
    context.eval(Source::from_bytes(
        r#"
        (function() {
            const _native = crypto._getRandomValues;
            crypto.getRandomValues = function(array) {
                _native(array);
                return array;
            };
            delete crypto._getRandomValues;
        })();

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

/// Setup TextEncoder/TextDecoder
fn setup_text_encoding(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(Source::from_bytes(
        r#"
        globalThis.TextEncoder = class TextEncoder {
            constructor() {
                this.encoding = 'utf-8';
            }

            encode(input) {
                const str = String(input || '');
                const bytes = [];

                for (let i = 0; i < str.length; i++) {
                    let code = str.codePointAt(i);
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

        globalThis.TextDecoder = class TextDecoder {
            constructor(encoding = 'utf-8') {
                this.encoding = encoding.toLowerCase();
                if (this.encoding !== 'utf-8' && this.encoding !== 'utf8') {
                    throw new RangeError('Only UTF-8 encoding is supported');
                }
            }

            decode(input) {
                if (!input) return '';

                const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
                const chars = [];

                let i = 0;
                while (i < bytes.length) {
                    const byte1 = bytes[i++];

                    if (byte1 < 0x80) {
                        chars.push(String.fromCharCode(byte1));
                    } else if ((byte1 & 0xE0) === 0xC0) {
                        const byte2 = bytes[i++];
                        const code = ((byte1 & 0x1F) << 6) | (byte2 & 0x3F);
                        chars.push(String.fromCharCode(code));
                    } else if ((byte1 & 0xF0) === 0xE0) {
                        const byte2 = bytes[i++];
                        const byte3 = bytes[i++];
                        const code = ((byte1 & 0x0F) << 12) | ((byte2 & 0x3F) << 6) | (byte3 & 0x3F);
                        chars.push(String.fromCharCode(code));
                    } else if ((byte1 & 0xF8) === 0xF0) {
                        const byte2 = bytes[i++];
                        const byte3 = bytes[i++];
                        const byte4 = bytes[i++];
                        const code = ((byte1 & 0x07) << 18) | ((byte2 & 0x3F) << 12) |
                                    ((byte3 & 0x3F) << 6) | (byte4 & 0x3F);
                        chars.push(String.fromCodePoint(code));
                    } else {
                        chars.push('\uFFFD');
                    }
                }

                return chars.join('');
            }
        };
        "#,
    ))?;
    Ok(())
}

/// Setup timers (setTimeout, setInterval, clearTimeout, clearInterval)
fn setup_timers(context: &mut Context) -> Result<(), boa_engine::JsError> {
    // For now, provide stub implementations that execute immediately
    // A full implementation would require integration with tokio runtime
    context.eval(Source::from_bytes(
        r#"
        globalThis.__timerId = 0;
        globalThis.__timers = new Map();

        globalThis.setTimeout = function(callback, delay, ...args) {
            const id = ++globalThis.__timerId;
            // For now, just schedule for next tick (simplified)
            // In a full implementation, this would use native timers
            Promise.resolve().then(() => {
                if (globalThis.__timers.has(id)) {
                    globalThis.__timers.delete(id);
                    callback(...args);
                }
            });
            globalThis.__timers.set(id, { callback, args, type: 'timeout' });
            return id;
        };

        globalThis.setInterval = function(callback, interval, ...args) {
            const id = ++globalThis.__timerId;
            // Simplified - just runs once for now
            globalThis.__timers.set(id, { callback, args, type: 'interval' });
            return id;
        };

        globalThis.clearTimeout = function(id) {
            globalThis.__timers.delete(id);
        };

        globalThis.clearInterval = function(id) {
            globalThis.__timers.delete(id);
        };
        "#,
    ))?;
    Ok(())
}

/// Setup fetch() global function
///
/// Creates a JS-side fetch() that stores pending requests in __pendingFetches.
/// The Rust side drains these and resolves them via OperationsHandler::handle_fetch.
fn setup_fetch_global(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(Source::from_bytes(
        r#"
        globalThis.__pendingFetches = [];

        globalThis.fetch = function(input, init) {
            init = init || {};

            var url = typeof input === 'string' ? input : (input && input.url ? input.url : String(input));
            var method = (init.method || 'GET').toUpperCase();
            var headers = {};

            if (init.headers) {
                if (init.headers instanceof Headers) {
                    init.headers._map.forEach(function(v, k) { headers[k] = v; });
                } else if (typeof init.headers === 'object') {
                    var keys = Object.keys(init.headers);

                    for (var i = 0; i < keys.length; i++) {
                        headers[keys[i].toLowerCase()] = String(init.headers[keys[i]]);
                    }
                }
            }

            var body = init.body !== undefined && init.body !== null ? String(init.body) : null;

            return new Promise(function(resolve, reject) {
                globalThis.__pendingFetches.push({
                    url: url,
                    method: method,
                    headers: headers,
                    body: body,
                    resolve: resolve,
                    reject: reject
                });
            });
        };
        "#,
    ))?;
    Ok(())
}

/// Setup ReadableStream
fn setup_readable_stream(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(Source::from_bytes(
        r#"
        globalThis.ReadableStream = class ReadableStream {
            constructor(underlyingSource = {}, strategy = {}) {
                this._controller = null;
                this._locked = false;
                this._state = 'readable';
                this._reader = null;
                this._storedError = undefined;
                this._queue = [];

                const controller = {
                    _stream: this,
                    enqueue: (chunk) => {
                        if (this._state !== 'readable') return;
                        this._queue.push({ type: 'chunk', value: chunk });
                        if (this._reader && this._reader._resolveRead) {
                            const resolve = this._reader._resolveRead;
                            this._reader._resolveRead = null;
                            const item = this._queue.shift();
                            resolve({ value: item.value, done: false });
                        }
                    },
                    close: () => {
                        if (this._state !== 'readable') return;
                        this._state = 'closed';
                        this._queue.push({ type: 'close' });
                        if (this._reader && this._reader._resolveRead) {
                            const resolve = this._reader._resolveRead;
                            this._reader._resolveRead = null;
                            resolve({ value: undefined, done: true });
                        }
                    },
                    error: (e) => {
                        if (this._state !== 'readable') return;
                        this._state = 'errored';
                        this._storedError = e;
                        if (this._reader && this._reader._rejectRead) {
                            this._reader._rejectRead(e);
                        }
                    }
                };
                this._controller = controller;

                if (underlyingSource.start) {
                    underlyingSource.start(controller);
                }
            }

            get locked() {
                return this._locked;
            }

            getReader() {
                if (this._locked) {
                    throw new TypeError('ReadableStream is locked');
                }
                this._locked = true;

                const stream = this;
                const reader = {
                    _stream: stream,
                    _resolveRead: null,
                    _rejectRead: null,

                    read() {
                        return new Promise((resolve, reject) => {
                            if (stream._queue.length > 0) {
                                const item = stream._queue.shift();
                                if (item.type === 'close') {
                                    resolve({ value: undefined, done: true });
                                } else {
                                    resolve({ value: item.value, done: false });
                                }
                            } else if (stream._state === 'closed') {
                                resolve({ value: undefined, done: true });
                            } else if (stream._state === 'errored') {
                                reject(stream._storedError);
                            } else {
                                this._resolveRead = resolve;
                                this._rejectRead = reject;
                            }
                        });
                    },

                    releaseLock() {
                        stream._locked = false;
                        stream._reader = null;
                    },

                    cancel(reason) {
                        stream._state = 'closed';
                        return Promise.resolve();
                    }
                };

                this._reader = reader;
                return reader;
            }

            tee() {
                const chunks = [];
                const reader = this.getReader();

                const branch1 = new ReadableStream({
                    async start(controller) {
                        try {
                            while (true) {
                                const { done, value } = await reader.read();
                                if (done) {
                                    controller.close();
                                    break;
                                }
                                chunks.push(value);
                                controller.enqueue(value);
                            }
                        } catch (e) {
                            controller.error(e);
                        }
                    }
                });

                const branch2 = new ReadableStream({
                    start(controller) {
                        for (const chunk of chunks) {
                            controller.enqueue(chunk);
                        }
                        controller.close();
                    }
                });

                return [branch1, branch2];
            }

            cancel(reason) {
                this._state = 'closed';
                return Promise.resolve();
            }
        };
        "#,
    ))?;
    Ok(())
}

/// Setup event handling
///
/// Stores handlers in both Rust-side (EventListeners via context data) and JS-side arrays.
/// Rust-side storage is used by handle_scheduled (where PromiseJob dispatch works fine).
/// JS-side storage is used by handle_fetch (Boa 0.21 panics when calling stored functions
/// from Rust that instantiate Response/ReadableStream due to nested environment issues).
fn setup_event_handling(context: &mut Context) -> Result<(), boa_engine::JsError> {
    use boa_engine::NativeFunction;

    // Initialize the Rust-side event listeners storage
    let _ = EventListeners::from_context(context);

    // Native addEventListener that stores in Rust-side EventListeners
    let add_listener = NativeFunction::from_copy_closure(|_this, args, ctx| {
        let event_type = args
            .first()
            .and_then(|v| v.to_string(ctx).ok())
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default();

        let handler = args
            .get(1)
            .and_then(|v| v.as_object())
            .and_then(|obj| JsFunction::from_object(obj.clone()));

        if let Some(handler) = handler {
            let listeners = EventListeners::from_context(ctx);
            let mut listeners = listeners.borrow_mut();

            match event_type.as_str() {
                "fetch" => listeners.fetch.push(handler),
                "scheduled" => listeners.scheduled.push(handler),
                _ => {}
            }
        }

        Ok(JsValue::undefined())
    });

    context.register_global_callable(js_string!("__nativeAddEventListener"), 2, add_listener)?;

    // JS-side arrays + wrapper that stores in both Rust and JS
    context.eval(Source::from_bytes(
        r#"
        globalThis.__fetchHandlers = [];
        globalThis.__scheduledHandlers = [];

        globalThis.addEventListener = function(type, handler) {
            __nativeAddEventListener(type, handler);
            if (type === 'fetch') __fetchHandlers.push(handler);
            else if (type === 'scheduled') __scheduledHandlers.push(handler);
        };

        globalThis.removeEventListener = function(type) {
            if (type === 'fetch') __fetchHandlers = [];
            else if (type === 'scheduled') __scheduledHandlers = [];
        };
        "#,
    ))?;

    Ok(())
}

/// Setup response extraction helpers (registered once at init, called per-request)
/// These are minimal JS functions that handle Map iteration and ReadableStream queue
/// reading — operations that are awkward to do via Boa's Rust API alone.
fn setup_response_extractors(context: &mut Context) -> Result<(), boa_engine::JsError> {
    // Extract headers from a Headers instance → flat array [key, val, key, val, ...]
    context.eval(Source::from_bytes(
        r#"
        globalThis.__extractHeaders = function(headers) {
            var result = [];
            if (headers && headers._map) {
                headers._map.forEach(function(value, key) {
                    result.push(key);
                    result.push(String(value));
                });
            }
            return result;
        };
        "#,
    ))?;

    // Extract body text from a ReadableStream's internal _queue
    context.eval(Source::from_bytes(
        r#"
        globalThis.__extractBody = function(stream) {
            if (!stream || !stream._queue) return '';
            var chunks = [];
            for (var i = 0; i < stream._queue.length; i++) {
                var item = stream._queue[i];
                if (item.type === 'chunk' && item.value) {
                    if (item.value instanceof Uint8Array) {
                        chunks.push(new TextDecoder().decode(item.value));
                    } else {
                        chunks.push(String(item.value));
                    }
                }
            }
            return chunks.join('');
        };
        "#,
    ))?;

    Ok(())
}

// ============================================================================
// Trait implementations
// ============================================================================

impl openworkers_core::Worker for Worker {
    async fn new(script: Script, limits: Option<RuntimeLimits>) -> Result<Self, TerminationReason> {
        Worker::new(script, limits).await
    }

    async fn exec(&mut self, task: Event) -> Result<(), TerminationReason> {
        Worker::exec(self, task).await
    }

    fn abort(&mut self) {
        Worker::abort(self);
    }
}
