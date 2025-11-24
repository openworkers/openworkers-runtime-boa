use crate::compat::{Script, TerminationReason};
use crate::runtime::TokioJobQueue;
use crate::task::{HttpResponse, Task};
use boa_engine::{Context, Source, context::ContextBuilder};
use bytes::Bytes;
use std::cell::RefCell;
use std::rc::Rc;

pub struct Worker {
    context: Context,
    job_queue: Rc<TokioJobQueue>,
    /// Sender for fetch response (set during fetch execution)
    fetch_response_tx: Rc<RefCell<Option<tokio::sync::oneshot::Sender<String>>>>,
}

impl Worker {
    /// Create a new worker (openworkers-runtime compatible)
    pub async fn new(
        script: Script,
        _log_tx: Option<std::sync::mpsc::Sender<crate::compat::LogEvent>>,
        _limits: Option<crate::compat::RuntimeLimits>,
    ) -> Result<Self, String> {
        let job_queue = TokioJobQueue::new();
        let fetch_response_tx: Rc<RefCell<Option<tokio::sync::oneshot::Sender<String>>>> =
            Rc::new(RefCell::new(None));

        // Create context with our job executor
        let context = ContextBuilder::default()
            .job_executor(job_queue.clone())
            .build()
            .map_err(|e| format!("Failed to create context: {}", e))?;

        let mut worker = Self {
            context,
            job_queue,
            fetch_response_tx: fetch_response_tx.clone(),
        };

        // Register boa_runtime extensions (console, timers, fetch, URL, encoding, etc.)
        boa_runtime::register(
            (
                boa_runtime::extensions::ConsoleExtension::default(),
                boa_runtime::extensions::TimeoutExtension,
                boa_runtime::extensions::MicrotaskExtension,
                boa_runtime::extensions::FetchExtension(
                    boa_runtime::fetch::BlockingReqwestFetcher::default(),
                ),
                boa_runtime::extensions::UrlExtension,
                boa_runtime::extensions::EncodingExtension,
                boa_runtime::extensions::StructuredCloneExtension,
            ),
            None,
            &mut worker.context,
        )
        .map_err(|e| format!("Failed to register runtime: {}", e))?;

        // Setup addEventListener with response channel
        setup_event_listener(&mut worker.context, fetch_response_tx)?;

        // Setup Response constructor
        setup_response(&mut worker.context)?;

        // Evaluate the worker script
        worker
            .context
            .eval(Source::from_bytes(&script.code))
            .map_err(|e| format!("Script evaluation failed: {}", e))?;

        Ok(worker)
    }

    /// Execute a task (openworkers-runtime compatible)
    pub async fn exec(&mut self, mut task: Task) -> Result<TerminationReason, String> {
        match task {
            Task::Fetch(ref mut init) => {
                let fetch_init = init.take().ok_or("FetchInit already consumed")?;
                match self.trigger_fetch_event(fetch_init).await {
                    Ok(_) => Ok(TerminationReason::Success),
                    Err(_) => Ok(TerminationReason::Exception),
                }
            }
            Task::Scheduled(_) => {
                // TODO: implement scheduled events
                Ok(TerminationReason::Success)
            }
        }
    }

    async fn trigger_fetch_event(
        &mut self,
        fetch_init: crate::task::FetchInit,
    ) -> Result<HttpResponse, String> {
        let req = &fetch_init.req;

        // Create oneshot channel for response
        let (response_tx, response_rx) = tokio::sync::oneshot::channel::<String>();

        // Store sender in the worker
        *self.fetch_response_tx.borrow_mut() = Some(response_tx);

        // Create request object as JSON string
        let request_json = serde_json::json!({
            "method": req.method,
            "url": req.url,
            "headers": req.headers,
        });

        // Trigger fetch event
        let trigger_script = format!(
            r#"
            (function() {{
                const request = {};
                if (typeof globalThis.__triggerFetch === 'function') {{
                    globalThis.__triggerFetch(request);
                    return true;
                }}
                throw new Error("No fetch handler registered");
            }})()
            "#,
            request_json
        );

        self.context
            .eval(Source::from_bytes(&trigger_script))
            .map_err(|e| format!("Fetch handler error: {}", e))?;

        // Wait for response from JS (sent via __sendFetchResponse)
        // Process callbacks in background while waiting
        let response_json = tokio::select! {
            result = response_rx => {
                result.map_err(|_| "Response channel closed - handler may not have called respondWith")?
            }
            _ = async {
                // Process callbacks in a loop to allow promises/timeouts to resolve
                for _ in 0..200 {
                    self.job_queue.drain_jobs(&mut self.context);
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            } => {
                return Err("Response timeout: no response after 200ms".to_string());
            }
        };

        // Parse the JSON response
        #[derive(serde::Deserialize)]
        struct ResponseData {
            status: u16,
            #[serde(default)]
            headers: Vec<(String, String)>,
            body: String,
        }

        let response_data: ResponseData = serde_json::from_str(&response_json)
            .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

        let response = HttpResponse {
            status: response_data.status,
            headers: response_data.headers,
            body: Some(Bytes::from(response_data.body)),
        };

        let _ = fetch_init.res_tx.send(response.clone());
        Ok(response)
    }
}

fn setup_event_listener(
    context: &mut Context,
    fetch_response_tx: Rc<RefCell<Option<tokio::sync::oneshot::Sender<String>>>>,
) -> Result<(), String> {
    use boa_engine::{JsValue, js_string, native_function::NativeFunction};

    // Create native __sendFetchResponse function
    let send_response = unsafe {
        NativeFunction::from_closure(move |_this, args, _context| {
            if let Some(response_json) = args.get(0) {
                if let Ok(json_str) = response_json.to_string(_context) {
                    let json_string = json_str.to_std_string_escaped();
                    if let Some(tx) = fetch_response_tx.borrow_mut().take() {
                        let _ = tx.send(json_string);
                    }
                }
            }
            Ok(JsValue::undefined())
        })
    };

    context
        .register_global_callable(js_string!("__sendFetchResponse"), 1, send_response)
        .map_err(|e| format!("Failed to register __sendFetchResponse: {}", e))?;

    // Setup addEventListener with response sending
    let script = r#"
        globalThis.addEventListener = function(type, handler) {
            if (type === 'fetch') {
                globalThis.__triggerFetch = async function(request) {
                    const event = {
                        request: request,
                        respondWith: function(response) {
                            this._response = response;
                        }
                    };
                    handler(event);
                    const response = event._response || new Response("No response");

                    // Extract and send response data to Rust
                    const responseData = {
                        status: response.status || 200,
                        body: String(response.body),
                        headers: response.headers || {}
                    };

                    if (typeof globalThis.__sendFetchResponse === 'function') {
                        globalThis.__sendFetchResponse(JSON.stringify(responseData));
                    }

                    return response;
                };
            }
        };
    "#;
    context
        .eval(Source::from_bytes(script))
        .map_err(|e| format!("Failed to setup addEventListener: {}", e))?;
    Ok(())
}

fn setup_response(context: &mut Context) -> Result<(), String> {
    let script = r#"
        globalThis.Response = function(body, init) {
            init = init || {};
            return {
                status: init.status || 200,
                body: String(body),
                headers: init.headers || {}
            };
        };
    "#;
    context
        .eval(Source::from_bytes(script))
        .map_err(|e| format!("Failed to setup Response: {}", e))?;
    Ok(())
}
