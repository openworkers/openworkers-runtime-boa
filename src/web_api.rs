//! Web APIs: upstream's where `boa_runtime` has them, JavaScript evaluated at
//! init where it does not.

use boa_engine::Context;

pub fn setup_web_apis(context: &mut Context) -> Result<(), boa_engine::JsError> {
    boa_runtime::base64::register(None, context)?;
    boa_runtime::clone::register(None, context)?;
    boa_runtime::microtask::register(None, context)?;

    setup_global_aliases(context)?;
    setup_dom_exception(context)?;
    Ok(())
}

fn setup_global_aliases(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.self = globalThis;
        globalThis.global = globalThis;
        "#,
    ))?;
    Ok(())
}

fn setup_dom_exception(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.DOMException = class DOMException extends Error {
            constructor(message, name) {
                super(message);
                this.name = name || 'Error';
            }
        };
        "#,
    ))?;
    Ok(())
}
