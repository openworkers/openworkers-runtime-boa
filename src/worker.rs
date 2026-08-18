use boa_engine::{
    Context, Finalize, JsData, JsObject, JsString, JsValue, NativeFunction, Source, Trace,
    builtins::promise::PromiseState,
    context::ContextBuilder,
    js_string,
    object::builtins::{JsArray, JsFunction, JsPromise},
    property::Attribute,
};
use boa_gc::{Gc, GcRefCell};
use bytes::Bytes;
use openworkers_core::{
    DefaultOps, Event, HttpRequest, HttpResponse, OperationsHandle, RequestBody, ResponseBody,
    RuntimeLimits, Script, TaskResult, TerminationReason,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::jobs::{Stop, WorkerJobs};
use crate::ops::{OpsFetcher, OpsLogger};
use crate::web_api::setup_web_apis;

/// Fetch handlers are kept in a JS array instead, because the dispatch loop
/// that reads them is itself JS.
#[derive(Default, Trace, Finalize, JsData)]
struct EventListeners {
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
    jobs: Rc<WorkerJobs>,
    budget: Option<Duration>,
    aborted: Arc<AtomicBool>,
}

impl Worker {
    pub async fn new_with_ops(
        script: Script,
        limits: Option<RuntimeLimits>,
        ops: OperationsHandle,
    ) -> Result<Self, TerminationReason> {
        let limits = limits.unwrap_or_default();
        let jobs = Rc::new(WorkerJobs::default());

        let mut context = ContextBuilder::new()
            .job_executor(jobs.clone())
            .build()
            .map_err(|e| {
                TerminationReason::InitializationError(format!("Failed to build context: {}", e))
            })?;

        boa_runtime::console::Console::register_with_logger(
            OpsLogger::new(ops.clone()),
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
        boa_runtime::interval::register(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register timers: {}", e))
        })?;

        // ReadableStream must come before the web APIs, Request/Response bodies are streams
        setup_readable_stream(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!(
                "Failed to register ReadableStream: {}",
                e
            ))
        })?;

        // Registers Headers, Request and Response too; the web APIs replace those
        boa_runtime::fetch::register(OpsFetcher::new(ops.clone()), None, &mut context).map_err(
            |e| TerminationReason::InitializationError(format!("Failed to register fetch: {}", e)),
        )?;

        setup_web_apis(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register web APIs: {}", e))
        })?;

        setup_fetch_shim(&mut context).map_err(|e| {
            TerminationReason::InitializationError(format!("Failed to register fetch shim: {}", e))
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
            jobs,
            budget: match limits.max_wall_clock_time_ms {
                0 => None,
                ms => Some(Duration::from_millis(ms)),
            },
            aborted: Arc::new(AtomicBool::new(false)),
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
        // A script is free to overwrite the helper, so this is not an invariant
        let dispatch = self
            .context
            .global_object()
            .get(js_string!("__dispatchFetch"), &mut self.context)
            .ok()
            .and_then(|v| v.as_object().and_then(JsFunction::from_object))
            .ok_or_else(|| {
                TerminationReason::Other("__dispatchFetch is not callable".to_string())
            })?;

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

        let Some(promise) = result.as_promise() else {
            return self.extract_response_from_js(&result);
        };

        self.settle(&promise).await?;

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
    }

    /// Runs the event loop until `promise` settles, then drops whatever is still
    /// queued: a stray timer must not keep the request open.
    async fn settle(&mut self, promise: &JsPromise) -> Result<(), TerminationReason> {
        let deadline = self.budget.map(|budget| Instant::now() + budget);
        let jobs = self.jobs.clone();
        let done = || !matches!(promise.state(), PromiseState::Pending);

        let stop = {
            let context = RefCell::new(&mut self.context);
            jobs.run_until(&context, deadline, &done).await
        };

        jobs.clear();

        match stop {
            Ok(Stop::Idle) => Ok(()),
            Ok(Stop::Timeout) => Err(TerminationReason::WallClockTimeout),
            Err(e) => Err(TerminationReason::Exception(format!("Job failed: {}", e))),
        }
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
        let handlers: Vec<JsFunction> = listeners.borrow().scheduled.clone();

        if handlers.is_empty() {
            return Ok(());
        }

        let dispatch = self
            .context
            .global_object()
            .get(js_string!("__dispatchScheduled"), &mut self.context)
            .ok()
            .and_then(|v| v.as_object().and_then(JsFunction::from_object))
            .ok_or_else(|| {
                TerminationReason::Other("__dispatchScheduled is not callable".to_string())
            })?;

        let handlers =
            JsArray::from_iter(handlers.into_iter().map(JsValue::from), &mut self.context);

        let args = [handlers.into(), JsValue::from(time as f64)];

        let result = dispatch
            .call(&JsValue::undefined(), &args, &mut self.context)
            .map_err(|e| {
                TerminationReason::Exception(format!("Scheduled dispatch failed: {}", e))
            })?;

        let Some(promise) = result.as_promise() else {
            return Ok(());
        };

        self.settle(&promise).await?;

        match promise.state() {
            PromiseState::Fulfilled(_) => Ok(()),
            PromiseState::Rejected(err) => Err(TerminationReason::Exception(format!(
                "Scheduled handler rejected: {}",
                err.display()
            ))),
            PromiseState::Pending => Err(TerminationReason::Exception(
                "Scheduled handler did not complete".to_string(),
            )),
        }
    }
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

    let subtle = boa_engine::object::ObjectInitializer::new(context)
        .function(
            NativeFunction::from_copy_closure(|_this, args, ctx| {
                let algo = args
                    .first()
                    .and_then(|v| v.to_string(ctx).ok())
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_default();

                let mut data: Vec<u8> = Vec::new();

                if let Some(arr) = args.get(1).and_then(|v| v.as_object())
                    && let Ok(buffer_val) = arr.get(js_string!("buffer"), ctx)
                    && let Some(buffer_obj) = buffer_val.as_object()
                    && let Ok(ab) =
                        boa_engine::object::builtins::JsArrayBuffer::from_object(buffer_obj.clone())
                    && let Some(bytes) = ab.data()
                {
                    data = bytes.to_vec();
                }

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

/// The handler of an `addEventListener('scheduled', ...)` call, `None` for any
/// other event type.
fn scheduled_handler_arg(args: &[JsValue], ctx: &mut Context) -> Option<JsFunction> {
    if args.first()?.to_string(ctx).ok()? != js_string!("scheduled") {
        return None;
    }

    JsFunction::from_object(args.get(1)?.as_object()?.clone())
}

fn setup_event_handling(context: &mut Context) -> Result<(), boa_engine::JsError> {
    use boa_engine::NativeFunction;

    let _ = EventListeners::from_context(context);

    let add_listener = NativeFunction::from_copy_closure(|_this, args, ctx| {
        if let Some(handler) = scheduled_handler_arg(args, ctx) {
            EventListeners::from_context(ctx)
                .borrow_mut()
                .scheduled
                .push(handler);
        }

        Ok(JsValue::undefined())
    });

    let remove_listener = NativeFunction::from_copy_closure(|_this, args, ctx| {
        if let Some(handler) = scheduled_handler_arg(args, ctx) {
            EventListeners::from_context(ctx)
                .borrow_mut()
                .scheduled
                .retain(|listener| !JsObject::equals(listener, &handler));
        }

        Ok(JsValue::undefined())
    });

    context.register_global_callable(js_string!("__nativeAddEventListener"), 2, add_listener)?;
    context.register_global_callable(
        js_string!("__nativeRemoveEventListener"),
        2,
        remove_listener,
    )?;

    context.eval(Source::from_bytes(
        r#"
        globalThis.__fetchHandlers = [];

        globalThis.addEventListener = function(type, handler) {
            __nativeAddEventListener(type, handler);
            if (type === 'fetch') __fetchHandlers.push(handler);
        };

        globalThis.removeEventListener = function(type, handler) {
            __nativeRemoveEventListener(type, handler);

            if (type === 'fetch') {
                __fetchHandlers = __fetchHandlers.filter(function(h) { return h !== handler; });
            }
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

            // A handler failure must not reach the client, only the log handler
            function failed(e) {
                var detail = String(e) + (e && e.stack ? '\n' + e.stack : '');
                console.error('Uncaught exception in fetch handler:', detail);
                return new Response('Internal Server Error', { status: 500 });
            }

            for (var i = 0; i < handlers.length; i++) {
                try {
                    await handlers[i](event);
                } catch (e) {
                    if (!event._response) event._response = failed(e);
                }
            }

            try {
                return await event._response;
            } catch (e) {
                return failed(e);
            }
        };

        globalThis.__dispatchScheduled = async function(handlers, scheduledTime) {
            var event = {
                type: 'scheduled',
                scheduledTime: scheduledTime,
                _waitUntil: [],
                waitUntil: function(promise) { this._waitUntil.push(promise); }
            };

            for (var i = 0; i < handlers.length; i++) {
                await handlers[i](event);
            }

            await Promise.all(event._waitUntil);
        };
        "#,
    ))?;

    Ok(())
}

/// Turns the upstream `Response` that `fetch()` resolves with into ours, which
/// carries a stream body and is what the response extractors read.
fn setup_fetch_shim(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(Source::from_bytes(
        r#"
        (function() {
            var native = globalThis.fetch;

            function plainHeaders(init, request) {
                var out = {};

                if (request && request.headers) {
                    request.headers.forEach(function(v, k) { out[k] = v; });
                }

                if (init instanceof Headers) {
                    init.forEach(function(v, k) { out[k] = v; });
                } else if (Array.isArray(init)) {
                    for (var i = 0; i < init.length; i++) out[init[i][0]] = String(init[i][1]);
                } else if (init && typeof init === 'object') {
                    var keys = Object.keys(init);
                    for (var j = 0; j < keys.length; j++) out[keys[j]] = String(init[keys[j]]);
                }

                return out;
            }

            globalThis.fetch = async function(input, init) {
                init = init || {};

                var request = typeof input === 'string' ? null : input;
                var options = { headers: plainHeaders(init.headers, request) };
                var method = init.method || (request && request.method);

                if (method) options.method = String(method);

                if (init.body !== undefined && init.body !== null) {
                    options.body = String(init.body);
                }

                if (init.signal) options.signal = init.signal;

                var response = await native(request ? String(request.url) : String(input), options);
                var headers = new Headers();

                for (var entry of response.headers) headers.append(entry[0], entry[1]);

                var bytes = await response.bytes();

                return new Response(bytes.length > 0 ? bytes : null, {
                    status: response.status,
                    statusText: response.statusText,
                    headers: headers
                });
            };
        })();
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
