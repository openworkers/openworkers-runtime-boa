//! Bridges `boa_runtime`'s pluggable seams onto `OperationsHandler`.

use boa_engine::{Context, Finalize, JsResult, Trace};
use boa_runtime::console::{ConsoleState, Logger};
use openworkers_core::{LogLevel, OperationsHandle};

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
