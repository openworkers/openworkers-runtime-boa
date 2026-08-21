//! Bridges `boa_runtime`'s pluggable seams onto `OperationsHandler`.

use boa_engine::{Context, Finalize, JsData, JsError, JsObject, JsResult, Trace, js_error};
use boa_runtime::console::{ConsoleState, Logger};
use boa_runtime::fetch::Fetcher;
use boa_runtime::fetch::request::JsRequest;
use boa_runtime::fetch::response::JsResponse;
use openworkers_core::{HttpRequest, LogLevel, OperationsHandle, RequestBody};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Routes `console` output to the runner instead of stdout and stderr.
#[derive(Trace, Finalize)]
pub struct OpsLogger(#[unsafe_ignore_trace] OperationsHandle);

impl OpsLogger {
    pub fn new(ops: OperationsHandle) -> Self {
        Self(ops)
    }

    fn emit(&self, level: LogLevel, msg: String) -> JsResult<()> {
        self.0.handle_log(level, msg);
        Ok(())
    }
}

impl Logger for OpsLogger {
    fn log(&self, msg: String, _state: &ConsoleState, _context: &mut Context) -> JsResult<()> {
        self.emit(LogLevel::Log, msg)
    }

    fn info(&self, msg: String, _state: &ConsoleState, _context: &mut Context) -> JsResult<()> {
        self.emit(LogLevel::Info, msg)
    }

    fn warn(&self, msg: String, _state: &ConsoleState, _context: &mut Context) -> JsResult<()> {
        self.emit(LogLevel::Warn, msg)
    }

    fn error(&self, msg: String, _state: &ConsoleState, _context: &mut Context) -> JsResult<()> {
        self.emit(LogLevel::Error, msg)
    }

    fn debug(&self, msg: String, _state: &ConsoleState, _context: &mut Context) -> JsResult<()> {
        self.emit(LogLevel::Debug, msg)
    }
}

/// Serves `fetch()` from the runner.
#[derive(Trace, Finalize, JsData)]
pub struct OpsFetcher(#[unsafe_ignore_trace] OperationsHandle);

impl OpsFetcher {
    pub fn new(ops: OperationsHandle) -> Self {
        Self(ops)
    }
}

impl Fetcher for OpsFetcher {
    async fn fetch(
        self: Rc<Self>,
        request: JsRequest,
        _signal: Option<JsObject>,
        _context: &RefCell<&mut Context>,
    ) -> JsResult<JsResponse> {
        let url = request.uri().to_string();
        let (parts, body) = request.into_inner().into_parts();

        let mut headers = HashMap::with_capacity(parts.headers.keys_len());

        for name in parts.headers.keys() {
            let joined = parts
                .headers
                .get_all(name)
                .into_iter()
                .filter_map(|value| value.to_str().ok())
                .collect::<Vec<_>>()
                .join(", ");

            headers.insert(name.as_str().to_string(), joined);
        }

        let method = parts.method.as_str().parse().map_err(
            |_| js_error!(TypeError: "fetch does not support the {} method", parts.method),
        )?;

        let outbound = HttpRequest {
            method,
            url: url.clone(),
            headers,
            body: if body.is_empty() {
                RequestBody::None
            } else {
                RequestBody::Bytes(body.into())
            },
        };

        let response = self
            .0
            .handle_fetch(outbound)
            .await
            .map_err(|e| js_error!(TypeError: "fetch failed: {}", e))?;

        let mut builder = http::Response::builder().status(response.status);

        for (name, value) in response.headers {
            builder = builder.header(name, value);
        }

        let body = response
            .body
            .collect()
            .await
            .map_err(|e| js_error!(TypeError: "fetch failed to read the body: {}", e))?
            .unwrap_or_default();

        builder
            .body(body.to_vec())
            .map(|inner| JsResponse::basic(url.into(), inner))
            .map_err(JsError::from_rust)
    }
}
