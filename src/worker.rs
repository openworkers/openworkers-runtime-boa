use crate::compat::{Script, TerminationReason};
use crate::runtime::TokioJobQueue;
use crate::task::{HttpResponse, Task};
use boa_engine::{Context, Source, context::ContextBuilder};
use bytes::Bytes;
use std::rc::Rc;

pub struct Worker {
    context: Context,
    job_queue: Rc<TokioJobQueue>,
}

impl Worker {
    /// Create a new worker (openworkers-runtime compatible)
    pub async fn new(
        script: Script,
        _log_tx: Option<std::sync::mpsc::Sender<crate::compat::LogEvent>>,
        _limits: Option<crate::compat::RuntimeLimits>,
    ) -> Result<Self, String> {
        let job_queue = TokioJobQueue::new();

        // Create context with our job executor
        let context = ContextBuilder::default()
            .job_executor(job_queue.clone())
            .build()
            .map_err(|e| format!("Failed to create context: {}", e))?;

        let mut worker = Self { context, job_queue };

        // Register boa_runtime extensions (console, timers, etc.)
        boa_runtime::register(
            (
                boa_runtime::extensions::ConsoleExtension::default(),
                boa_runtime::extensions::TimeoutExtension,
                boa_runtime::extensions::MicrotaskExtension,
            ),
            None,
            &mut worker.context,
        )
        .map_err(|e| format!("Failed to register runtime: {}", e))?;

        // Setup addEventListener
        setup_event_listener(&mut worker.context)?;

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
                    const response = globalThis.__triggerFetch(request);
                    // Store for extraction
                    globalThis.__lastResponse = response;
                    return response;
                }}
                throw new Error("No fetch handler registered");
            }})()
            "#,
            request_json
        );

        self.context
            .eval(Source::from_bytes(&trigger_script))
            .map_err(|e| format!("Fetch handler error: {}", e))?;

        // Process jobs to handle any pending promises/timeouts
        // Run for up to 200ms to allow timeouts to fire
        for _ in 0..200 {
            self.job_queue.drain_jobs(&mut self.context);
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        // Extract response
        let response_val = self
            .context
            .eval(Source::from_bytes("globalThis.__lastResponse"))
            .map_err(|e| format!("Failed to get response: {}", e))?;

        let response = if let Some(obj) = response_val.as_object() {
            let status = obj
                .get(boa_engine::js_string!("status"), &mut self.context)
                .ok()
                .and_then(|v| v.to_number(&mut self.context).ok())
                .unwrap_or(200.0) as u16;

            let body = obj
                .get(boa_engine::js_string!("body"), &mut self.context)
                .ok()
                .and_then(|v| v.to_string(&mut self.context).ok())
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_default();

            HttpResponse {
                status,
                headers: vec![],
                body: Some(Bytes::from(body)),
            }
        } else {
            HttpResponse {
                status: 500,
                headers: vec![],
                body: Some(Bytes::from("Invalid response")),
            }
        };

        let _ = fetch_init.res_tx.send(response.clone());
        Ok(response)
    }
}

fn setup_event_listener(context: &mut Context) -> Result<(), String> {
    let script = r#"
        globalThis.addEventListener = function(type, handler) {
            if (type === 'fetch') {
                globalThis.__triggerFetch = function(request) {
                    const event = {
                        request: request,
                        respondWith: function(response) {
                            this._response = response;
                        }
                    };
                    handler(event);
                    return event._response || new Response("No response");
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
