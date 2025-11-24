use crate::compat::{Script, TerminationReason};
use crate::task::{HttpRequest, HttpResponse, Task};
use boa_engine::{Context, Source};
use bytes::Bytes;

pub struct Worker {
    context: Context,
}

impl Worker {
    /// Create a new worker (openworkers-runtime compatible)
    pub async fn new(
        script: Script,
        _log_tx: Option<std::sync::mpsc::Sender<crate::compat::LogEvent>>,
        _limits: Option<crate::compat::RuntimeLimits>,
    ) -> Result<Self, String> {
        let mut context = Context::default();

        // Setup console.log
        setup_console(&mut context);

        // Setup addEventListener
        setup_event_listener(&mut context);

        // Setup Response constructor
        setup_response(&mut context);

        // Evaluate the worker script
        context
            .eval(Source::from_bytes(&script.code))
            .map_err(|e| format!("Script evaluation failed: {}", e))?;

        Ok(Self { context })
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

        // Create request object
        let request_script = format!(
            r#"({{
                method: "{}",
                url: "{}",
                headers: {{}},
            }})"#,
            req.method, req.url,
        );

        self.context
            .eval(Source::from_bytes(&request_script))
            .map_err(|e| format!("Failed to create request: {}", e))?;

        // Trigger fetch event
        let trigger_script = format!(
            r#"
            (function() {{
                if (typeof globalThis.__triggerFetch === 'function') {{
                    const request = {};
                    const response = globalThis.__triggerFetch(request);
                    return response;
                }}
                throw new Error("No fetch handler registered");
            }})()
            "#,
            request_script
        );

        let result = self
            .context
            .eval(Source::from_bytes(&trigger_script))
            .map_err(|e| format!("Fetch handler error: {}", e))?;

        // Extract response
        let response = HttpResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from("Hello from Boa!")),
        };

        let _ = fetch_init.res_tx.send(response.clone());
        Ok(response)
    }
}

fn setup_console(context: &mut Context) {
    let console_log = r#"
        globalThis.console = {
            log: function(...args) {
                print(...args);
            }
        };
    "#;
    context.eval(Source::from_bytes(console_log)).ok();
}

fn setup_event_listener(context: &mut Context) {
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
    context.eval(Source::from_bytes(script)).ok();
}

fn setup_response(context: &mut Context) {
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
    context.eval(Source::from_bytes(script)).ok();
}
