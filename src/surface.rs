//! The shared web surface, installed instead of this crate's own copies.
//!
//! Each module is a classic script evaluated in SURFACE order, against ops it
//! reads out of the native namespace at call time. A module is taken only when
//! every op it reads is in [`PROVIDED_OPS`], so the surface grows one op at a
//! time rather than all at once.

use boa_engine::Context;
use boa_engine::JsError;
use boa_engine::JsValue;
use boa_engine::NativeFunction;
use boa_engine::Source;
use boa_engine::js_string;
use boa_engine::object::ObjectInitializer;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Ops this runtime answers on the native namespace.
pub const PROVIDED_OPS: &[&str] = &["performanceNow", "timeOrigin", "userAgent"];

/// What `navigator.userAgent` reports. `Product/Version (comment)` is the HTTP
/// grammar, so a reader splitting on the slash still finds the version.
fn user_agent() -> String {
    format!("OpenWorkers/{} (boa)", env!("CARGO_PKG_VERSION"))
}

/// Install the native namespace the surface reads its ops from. Call before
/// [`install`], and never capture an op: a module reads it at call time.
pub fn install_ops(context: &mut Context) -> Result<(), JsError> {
    // The wall clock when this worker started, which is what `now` counts from
    let origin = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64() * 1_000.0)
        .unwrap_or(0.0);

    let started = std::time::Instant::now();

    let ops = ObjectInitializer::new(context)
        .function(
            NativeFunction::from_copy_closure(move |_this, _args, _ctx| {
                Ok(JsValue::from(started.elapsed().as_secs_f64() * 1_000.0))
            }),
            js_string!("performanceNow"),
            0,
        )
        .property(
            js_string!("timeOrigin"),
            origin,
            boa_engine::property::Attribute::all(),
        )
        .property(
            js_string!("userAgent"),
            js_string!(user_agent()),
            boa_engine::property::Attribute::all(),
        )
        .build();

    context.register_global_property(
        js_string!(openworkers_wintertc::NATIVE_NAMESPACE),
        ops,
        boa_engine::property::Attribute::all(),
    )
}

/// Modules whose every op this runtime answers, in the order the surface
/// declares: a module patches what the one before it defined.
fn installable() -> impl Iterator<Item = &'static openworkers_wintertc::Module> {
    openworkers_wintertc::SURFACE.iter().filter(|module| {
        module
            .required_ops
            .iter()
            .all(|op| PROVIDED_OPS.contains(op))
    })
}

/// Evaluate the surface into `context`. The caller installs the native
/// namespace first, or a module that reads an op finds nothing there.
pub fn install(context: &mut Context) -> Result<(), JsError> {
    for module in installable() {
        context
            .eval(Source::from_bytes(module.source))
            .map_err(|err| {
                JsError::from_opaque(
                    boa_engine::js_string!(format!("surface module {}: {err}", module.name)).into(),
                )
            })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PROVIDED_OPS;
    use super::installable;

    #[test]
    fn every_installed_module_has_its_ops() {
        for module in installable() {
            for op in module.required_ops {
                assert!(
                    PROVIDED_OPS.contains(op),
                    "{} reads {op}, which this runtime does not answer",
                    module.name
                );
            }
        }
    }

    #[test]
    fn the_surface_is_not_empty() {
        assert!(installable().count() > 0);
    }
}
