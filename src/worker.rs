use boa_engine::{
    Context, Finalize, JsData, JsObject, JsString, JsValue, NativeFunction, Source, Trace,
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

/// Caps the drain loop, so a script chaining fetches and timers forever cannot
/// hold the thread.
const MAX_DRAIN_ROUNDS: usize = 100;

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
    ops: OperationsHandle,
}

impl Worker {
    pub async fn new_with_ops(
        script: Script,
        _limits: Option<RuntimeLimits>,
        ops: OperationsHandle,
    ) -> Result<Self, TerminationReason> {
        let mut context = Context::default();

        boa_runtime::console::Console::register_with_logger(
            boa_runtime::console::DefaultLogger,
            &mut context,
        )
        .map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register console: {}", e))
        })?;

        setup_crypto(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register crypto: {}", e))
        })?;

        boa_runtime::text::register(None, &mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register text encoding: {}",
                e
            ))
        })?;

        // Timers must come before the web APIs, AbortSignal.timeout uses setTimeout
        setup_timers(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register timers: {}", e))
        })?;

        // ReadableStream must come before the web APIs, Request/Response bodies are streams
        setup_readable_stream(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register ReadableStream: {}",
                e
            ))
        })?;

        setup_web_apis(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register web APIs: {}", e))
        })?;

        setup_fetch_global(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register fetch: {}", e))
        })?;

        setup_event_handling(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register event handling: {}",
                e
            ))
        })?;

        setup_response_extractors(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register response extractors: {}",
                e
            ))
        })?;

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

    /// Create a worker with `DefaultOps`, which rejects every outbound operation.
    pub async fn new(
        script: Script,
        limits: Option<RuntimeLimits>,
    ) -> Result<Self, TerminationReason> {
        let ops: OperationsHandle = Arc::new(DefaultOps);
        Self::new_with_ops(script, limits, ops).await
    }

    /// Only takes effect between tasks, a running handler is not interrupted.
    pub fn abort(&mut self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

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

    /// Dispatches through the JS helper registered at init, so a request parses
    /// no source of its own.
    async fn handle_fetch(
        &mut self,
        request: HttpRequest,
    ) -> Result<HttpResponse, TerminationReason> {
        let dispatch = self
            .context
            .global_object()
            .get(js_string!("__dispatchFetch"), &mut self.context)
            .ok()
            .and_then(|v| v.as_object().and_then(JsFunction::from_object))
            .expect("__dispatchFetch is registered at init");

        let headers = JsObject::with_object_proto(self.context.intrinsics());

        for (name, value) in &request.headers {
            headers
                .set(
                    JsString::from(name.as_str()),
                    JsString::from(value.as_str()),
                    false,
                    &mut self.context,
                )
                .map_err(|e| {
                    TerminationReason::Exception(format!("Request headers rejected: {}", e))
                })?;
        }

        let body = match &request.body {
            RequestBody::Bytes(b) if !b.is_empty() => {
                JsValue::from(JsString::from(String::from_utf8_lossy(b).as_ref()))
            }
            RequestBody::Bytes(_) | RequestBody::None | RequestBody::Stream(_) => JsValue::null(),
        };

        let args = [
            JsValue::from(JsString::from(request.url.as_str())),
            JsValue::from(JsString::from(request.method.to_string())),
            JsValue::from(headers),
            body,
        ];

        let result = dispatch
            .call(&JsValue::undefined(), &args, &mut self.context)
            .map_err(|e| {
                let msg = format!("{}", e);

                if msg.contains("__no_handlers__") {
                    return TerminationReason::Other("No fetch handlers registered".to_string());
                }

                TerminationReason::Exception(format!("Dispatch failed: {}", e))
            })?;

        let _ = self.context.run_jobs();

        // Drain until neither yields work, or an `await fetch(...)` never resolves
        for _ in 0..MAX_DRAIN_ROUNDS {
            let fetch_count = self.resolve_pending_fetches().await;
            let timer_count = self.resolve_pending_timers().await;

            if fetch_count == 0 && timer_count == 0 {
                break;
            }

            let _ = self.context.run_jobs();
        }

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

    /// Returns how many fetches were run, so the caller knows whether to loop again.
    async fn resolve_pending_fetches(&mut self) -> usize {
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

            let mut headers = std::collections::HashMap::new();

            if let Ok(headers_val) = entry_obj.get(js_string!("headers"), &mut self.context)
                && let Some(headers_obj) = headers_val.as_object()
                && let Ok(keys) = headers_obj.own_property_keys(&mut self.context)
            {
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

        let _ = self.context.eval(Source::from_bytes(
            b"globalThis.__pendingFetches.length = 0;",
        ));

        let count = entries.len();

        for (request, resolve, reject) in entries {
            match self.ops.handle_fetch(request).await {
                Ok(response) => {
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

    /// Sleeps out each queued delay before firing it; the JS side only records
    /// timers, it never waits.
    async fn resolve_pending_timers(&mut self) -> usize {
        let pending_arr = match self
            .context
            .global_object()
            .get(js_string!("__pendingTimers"), &mut self.context)
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

        let mut timers = Vec::with_capacity(len);

        for i in 0..len {
            let entry = match arr_obj.get(i as u32, &mut self.context) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let entry_obj = match entry.as_object() {
                Some(obj) => obj,
                None => continue,
            };

            let id = entry_obj
                .get(js_string!("id"), &mut self.context)
                .ok()
                .and_then(|v| v.to_u32(&mut self.context).ok())
                .unwrap_or(0);

            let delay = entry_obj
                .get(js_string!("delay"), &mut self.context)
                .ok()
                .and_then(|v| v.to_u32(&mut self.context).ok())
                .unwrap_or(0) as u64;

            timers.push((id, delay));
        }

        let _ = self.context.eval(Source::from_bytes(
            b"globalThis.__pendingTimers.length = 0;",
        ));

        let count = timers.len();

        for (id, delay) in timers {
            if delay > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
            }

            let cancelled_check = format!(
                "globalThis.__cancelledTimers.has({}) ? (globalThis.__cancelledTimers.delete({}), true) : false",
                id, id
            );

            let was_cancelled = self
                .context
                .eval(Source::from_bytes(cancelled_check.as_bytes()))
                .map(|v| v.to_boolean())
                .unwrap_or(false);

            if !was_cancelled {
                let exec_code = format!("globalThis.__executeTimer({})", id);
                let _ = self.context.eval(Source::from_bytes(exec_code.as_bytes()));
                let _ = self.context.run_jobs();
            }
        }

        count
    }

    fn build_response_js(&self, response: HttpResponse) -> String {
        let body = match &response.body {
            ResponseBody::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
            ResponseBody::None | ResponseBody::Stream(_) => None,
        };

        let headers: std::collections::HashMap<&str, &str> = response
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        format!(
            "(new Response({}, {{ status: {}, headers: {} }}))",
            js_literal(&body),
            response.status,
            js_literal(&headers)
        )
    }

    fn extract_response_from_js(
        &mut self,
        value: &JsValue,
    ) -> Result<HttpResponse, TerminationReason> {
        let resp_obj = match value.as_object() {
            Some(obj) => obj,
            None => {
                // Not an object means respondWith was never called
                return Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: ResponseBody::None,
                });
            }
        };

        let status = resp_obj
            .get(js_string!("status"), &mut self.context)
            .ok()
            .and_then(|v| v.to_u32(&mut self.context).ok())
            .unwrap_or(200) as u16;

        // Headers live in a JS Map, which is why they come back through a JS helper
        let mut headers = Vec::new();

        // The helper returns a flat [key, value, key, value, ...]
        if let Ok(headers_val) = resp_obj.get(js_string!("headers"), &mut self.context)
            && let Some(headers_obj) = headers_val.as_object()
            && let Ok(extractor) = self
                .context
                .global_object()
                .get(js_string!("__extractHeaders"), &mut self.context)
            && let Some(extractor_fn) = extractor
                .as_object()
                .and_then(|o| JsFunction::from_object(o.clone()))
            && let Ok(result) = extractor_fn.call(
                &JsValue::undefined(),
                &[headers_obj.clone().into()],
                &mut self.context,
            )
            && let Some(arr) = result.as_object()
        {
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

        // The body is a ReadableStream, so it is drained by a JS helper too
        let mut body = ResponseBody::None;

        if let Ok(body_val) = resp_obj.get(js_string!("body"), &mut self.context)
            && body_val.is_object()
            && let Ok(extractor) = self
                .context
                .global_object()
                .get(js_string!("__extractBody"), &mut self.context)
            && let Some(extractor_fn) = extractor
                .as_object()
                .and_then(|o| JsFunction::from_object(o.clone()))
            && let Ok(result) =
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

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    async fn handle_scheduled(&mut self, time: u64) -> Result<(), TerminationReason> {
        let listeners = EventListeners::from_context(&mut self.context);
        let scheduled_handlers: Vec<JsFunction> = listeners.borrow().scheduled.clone();

        if scheduled_handlers.is_empty() {
            return Ok(());
        }

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

        for handler in scheduled_handlers {
            let event_val = event_value.clone();

            let job = PromiseJob::new(move |context| {
                handler.call(
                    &JsValue::undefined(),
                    std::slice::from_ref(&event_val),
                    context,
                )
            });

            self.context.enqueue_job(job.into());
        }

        if let Err(e) = self.context.run_jobs() {
            return Err(TerminationReason::Exception(format!(
                "Scheduled handler error: {}",
                e
            )));
        }

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

                    for _ in 0..MAX_DRAIN_ROUNDS {
                        let timer_count = self.resolve_pending_timers().await;

                        if timer_count == 0 {
                            break;
                        }

                        let _ = self.context.run_jobs();
                    }

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

/// JSON syntax is a subset of JS expression syntax, so a serialized value can
/// be spliced into generated source without breaking out of its literal.
fn js_literal<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("request and response fields are serializable")
}

fn setup_crypto(context: &mut Context) -> Result<(), boa_engine::JsError> {
    use ring::{digest, rand};

    let crypto = boa_engine::object::ObjectInitializer::new(context)
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
                if let Some(array) = args.first().and_then(|v| v.as_object())
                    && let Ok(buffer_val) = array.get(js_string!("buffer"), ctx)
                    && let Some(buffer_obj) = buffer_val.as_object()
                    && let Ok(ab) =
                        boa_engine::object::builtins::JsArrayBuffer::from_object(buffer_obj.clone())
                {
                    let rng = rand::SystemRandom::new();
                    let len = ab.data().map(|d| d.len()).unwrap_or(0);
                    let mut bytes = vec![0u8; len];

                    if rand::SecureRandom::fill(&rng, &mut bytes).is_ok()
                        && let Some(mut data) = ab.data_mut()
                    {
                        data.copy_from_slice(&bytes);
                    }
                }
                Ok(JsValue::undefined())
            }),
            js_string!("_getRandomValues"),
            1,
        )
        .build();

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

/// Queues timers for the Rust side to sleep on, except zero-delay ones, which
/// go straight to a microtask.
fn setup_timers(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(Source::from_bytes(
        r#"
        globalThis.__timerId = 0;
        globalThis.__timerCallbacks = new Map();
        globalThis.__pendingTimers = [];
        globalThis.__cancelledTimers = new Set();

        globalThis.__executeTimer = function(id) {
            var entry = globalThis.__timerCallbacks.get(id);

            if (!entry) return;

            if (entry.type === 'timeout') {
                globalThis.__timerCallbacks.delete(id);
            }

            try {
                entry.callback.apply(undefined, entry.args);
            } catch (e) {
                // Timer callback errors should not crash the runtime
            }

            if (entry.type === 'interval') {
                globalThis.__pendingTimers.push({
                    id: id,
                    delay: entry.delay,
                    type: 'interval'
                });
            }
        };

        globalThis.setTimeout = function(callback, delay) {
            var args = [];

            for (var i = 2; i < arguments.length; i++) {
                args.push(arguments[i]);
            }

            var id = ++globalThis.__timerId;
            var ms = Math.max(0, Number(delay) || 0);

            globalThis.__timerCallbacks.set(id, {
                callback: callback,
                args: args,
                type: 'timeout',
                delay: ms
            });

            if (ms === 0) {
                Promise.resolve().then(function() {
                    if (!globalThis.__cancelledTimers.has(id)) {
                        globalThis.__executeTimer(id);
                    } else {
                        globalThis.__cancelledTimers.delete(id);
                    }
                });
            } else {
                globalThis.__pendingTimers.push({
                    id: id,
                    delay: ms,
                    type: 'timeout'
                });
            }

            return id;
        };

        globalThis.setInterval = function(callback, interval) {
            var args = [];

            for (var i = 2; i < arguments.length; i++) {
                args.push(arguments[i]);
            }

            var id = ++globalThis.__timerId;
            var ms = Math.max(0, Number(interval) || 0);

            globalThis.__timerCallbacks.set(id, {
                callback: callback,
                args: args,
                type: 'interval',
                delay: ms
            });

            globalThis.__pendingTimers.push({
                id: id,
                delay: ms,
                type: 'interval'
            });

            return id;
        };

        globalThis.clearTimeout = function(id) {
            globalThis.__timerCallbacks.delete(id);
            globalThis.__cancelledTimers.add(id);
        };

        globalThis.clearInterval = function(id) {
            globalThis.__timerCallbacks.delete(id);
            globalThis.__cancelledTimers.add(id);
        };
        "#,
    ))?;
    Ok(())
}

/// fetch() only queues into __pendingFetches; the Rust side is what runs the
/// request through the operations handler.
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

/// The subset of WHATWG streams that Request and Response bodies need.
fn setup_readable_stream(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(Source::from_bytes(
        r#"
        globalThis.ReadableStreamDefaultController = class ReadableStreamDefaultController {
            constructor(stream) {
                this._stream = stream;
                this._queue = [];
                this._closeRequested = false;
            }

            enqueue(chunk) {
                if (this._closeRequested) {
                    throw new TypeError('Cannot enqueue after close');
                }

                if (this._stream._state !== 'readable') {
                    throw new TypeError('Stream is not in readable state');
                }

                this._queue.push({ type: 'chunk', value: chunk });
                this._processQueue();
            }

            close() {
                if (this._closeRequested) {
                    throw new TypeError('Stream is already closing');
                }

                if (this._stream._state !== 'readable') {
                    throw new TypeError('Stream is not in readable state');
                }

                this._closeRequested = true;
                this._queue.push({ type: 'close' });
                this._processQueue();
            }

            error(error) {
                if (this._stream._state !== 'readable') {
                    return;
                }

                this._stream._state = 'errored';
                this._stream._storedError = error;

                if (this._stream._reader) {
                    this._stream._reader._errorPending(error);
                }

                this._queue = [];
            }

            _processQueue() {
                if (this._stream._reader) {
                    this._stream._reader._processQueue();
                }
            }

            get desiredSize() {
                if (this._stream._state === 'errored') return null;
                if (this._stream._state === 'closed') return 0;
                return Math.max(0, 1 - this._queue.length);
            }
        };
        "#,
    ))?;

    context.eval(Source::from_bytes(
        r#"
        globalThis.ReadableStreamDefaultReader = class ReadableStreamDefaultReader {
            constructor(stream) {
                if (stream._reader) {
                    throw new TypeError('Stream is already locked');
                }

                this._stream = stream;
                this._readRequests = [];
                this._closedPromiseResolve = null;
                this._closedPromiseReject = null;

                var self = this;
                this._closedPromise = new Promise(function(resolve, reject) {
                    self._closedPromiseResolve = resolve;
                    self._closedPromiseReject = reject;
                });
            }

            read() {
                if (!this._stream) {
                    return Promise.reject(new TypeError('Reader is released'));
                }

                if (this._stream._state === 'errored') {
                    return Promise.reject(this._stream._storedError);
                }

                var controller = this._stream._controller;

                if (controller._queue.length > 0) {
                    var item = controller._queue.shift();

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

                var underlyingSource = this._stream._underlyingSource;

                if (underlyingSource && underlyingSource.pull) {
                    var self = this;

                    return new Promise(function(resolve, reject) {
                        self._readRequests.push({ resolve: resolve, reject: reject });

                        var pullPromise = underlyingSource.pull(controller);

                        if (pullPromise && typeof pullPromise.then === 'function') {
                            pullPromise.catch(function(e) {
                                controller.error(e);
                            });
                        }
                    });
                }

                var self = this;

                return new Promise(function(resolve, reject) {
                    self._readRequests.push({ resolve: resolve, reject: reject });
                });
            }

            _processQueue() {
                var controller = this._stream._controller;

                while (this._readRequests.length > 0 && controller._queue.length > 0) {
                    var request = this._readRequests.shift();
                    var item = controller._queue.shift();

                    if (item.type === 'close') {
                        this._stream._state = 'closed';
                        request.resolve({ done: true, value: undefined });
                        this._closePending();
                        break;
                    } else {
                        request.resolve({ done: false, value: item.value });
                    }
                }

                if (this._stream._state === 'closed' && this._readRequests.length > 0) {
                    while (this._readRequests.length > 0) {
                        var req = this._readRequests.shift();
                        req.resolve({ done: true, value: undefined });
                    }
                }
            }

            _closePending() {
                if (this._closedPromiseResolve) {
                    this._closedPromiseResolve();
                    this._closedPromiseResolve = null;
                }
            }

            _errorPending(error) {
                while (this._readRequests.length > 0) {
                    var req = this._readRequests.shift();
                    req.reject(error);
                }

                if (this._closedPromiseReject) {
                    this._closedPromiseReject(error);
                    this._closedPromiseReject = null;
                }
            }

            releaseLock() {
                if (!this._stream) return;

                if (this._readRequests.length > 0) {
                    throw new TypeError('Cannot release lock while read requests are pending');
                }

                this._stream._reader = null;
                this._stream = null;
            }

            cancel(reason) {
                if (!this._stream) {
                    return Promise.reject(new TypeError('Reader is released'));
                }

                var cancelPromise = this._stream.cancel(reason);
                this.releaseLock();
                return cancelPromise;
            }

            get closed() {
                return this._closedPromise;
            }
        };
        "#,
    ))?;

    context.eval(Source::from_bytes(
        r#"
        globalThis.ReadableStream = class ReadableStream {
            constructor(underlyingSource) {
                underlyingSource = underlyingSource || {};
                this._underlyingSource = underlyingSource;
                this._controller = null;
                this._reader = null;
                this._state = 'readable';
                this._storedError = null;

                var controller = new ReadableStreamDefaultController(this);
                this._controller = controller;

                if (underlyingSource.start) {
                    var startResult = underlyingSource.start(controller);

                    if (startResult && typeof startResult.then === 'function') {
                        startResult.catch(function(e) {
                            controller.error(e);
                        });
                    }
                }
            }

            getReader() {
                if (this._reader) {
                    throw new TypeError('ReadableStream is locked to a reader');
                }

                var reader = new ReadableStreamDefaultReader(this);
                this._reader = reader;
                return reader;
            }

            cancel(reason) {
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
            }

            get locked() {
                return this._reader !== null;
            }

            tee() {
                if (this.locked) {
                    throw new TypeError('Cannot tee a locked stream');
                }

                var reader = this.getReader();
                var canceled1 = false;
                var canceled2 = false;
                var closedOrErrored = false;
                var readPromise = null;

                function cloneValue(value) {
                    if (value instanceof Uint8Array) {
                        return new Uint8Array(value);
                    }

                    return value;
                }

                function pullBoth(controller1, controller2) {
                    if (closedOrErrored) return Promise.resolve();
                    if (readPromise) return readPromise;

                    readPromise = reader.read().then(function(result) {
                        readPromise = null;

                        if (result.done) {
                            closedOrErrored = true;
                            try { controller1.close(); } catch (e) {}
                            try { controller2.close(); } catch (e) {}
                            reader.releaseLock();
                            return;
                        }

                        if (!canceled1) controller1.enqueue(result.value);
                        if (!canceled2) controller2.enqueue(cloneValue(result.value));
                    }).catch(function(e) {
                        readPromise = null;
                        try { controller1.error(e); } catch (err) {}
                        try { controller2.error(e); } catch (err) {}
                    });

                    return readPromise;
                }

                var ctrl1 = null;
                var ctrl2 = null;

                var branch1 = new ReadableStream({
                    start: function(controller) { ctrl1 = controller; },
                    pull: function(controller) { return pullBoth(controller, ctrl2); },
                    cancel: function(reason) {
                        canceled1 = true;
                        if (canceled2) return reader.cancel(reason);
                        return Promise.resolve();
                    }
                });

                var branch2 = new ReadableStream({
                    start: function(controller) { ctrl2 = controller; },
                    pull: function(controller) { return pullBoth(ctrl1, controller); },
                    cancel: function(reason) {
                        canceled2 = true;
                        if (canceled1) return reader.cancel(reason);
                        return Promise.resolve();
                    }
                });

                return [branch1, branch2];
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

        globalThis.__dispatchFetch = async function(url, method, headers, body) {
            var handlers = globalThis.__fetchHandlers;

            if (handlers.length === 0) throw new Error('__no_handlers__');

            var event = {
                type: 'fetch',
                request: new Request(url, { method: method, headers: headers, body: body }),
                _response: null,
                respondWith: function(r) { this._response = r; }
            };

            for (var i = 0; i < handlers.length; i++) {
                try {
                    await handlers[i](event);
                } catch (e) {
                    if (!event._response) {
                        event._response = new Response(
                            'Error: ' + (e.message || e), { status: 500 }
                        );
                    }
                }
            }

            return await event._response;
        };
        "#,
    ))?;

    Ok(())
}

/// Map iteration and ReadableStream queue reading are awkward through Boa's Rust
/// API alone, so they are done in JS.
fn setup_response_extractors(context: &mut Context) -> Result<(), boa_engine::JsError> {
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

    context.eval(Source::from_bytes(
        r#"
        globalThis.__extractBody = function(stream) {
            var queue = null;
            if (stream && stream._controller && stream._controller._queue) {
                queue = stream._controller._queue;
            } else if (stream && stream._queue) {
                queue = stream._queue;
            }
            if (!queue) return '';
            var chunks = [];
            for (var i = 0; i < queue.length; i++) {
                var item = queue[i];
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
