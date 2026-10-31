//! The `env` object a worker reads its variables and bindings from.
//!
//! A binding is one native function per operation, gathered into an object
//! under the name the worker declared. Each returns a promise the host settles
//! off the operations handle, so a binding call suspends the guest the way
//! `fetch` does rather than blocking it.

use boa_engine::Context;
use boa_engine::JsArgs;
use boa_engine::JsError;
use boa_engine::JsObject;
use boa_engine::JsResult;
use boa_engine::JsString;
use boa_engine::JsValue;
use boa_engine::NativeFunction;
use boa_engine::Source;
use boa_engine::js_error;
use boa_engine::js_string;
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsArray;
use boa_engine::object::builtins::JsFunction;
use boa_engine::object::builtins::JsPromise;
use boa_engine::object::builtins::JsUint8Array;
use boa_engine::property::Attribute;
use boa_gc::Finalize;
use boa_gc::Trace;
use openworkers_core::BindingInfo;
use openworkers_core::BindingType;
use openworkers_core::HttpRequest;
#[cfg(feature = "invoke")]
use openworkers_core::InvokeResult;
use openworkers_core::KvOp;
use openworkers_core::KvResult;
use openworkers_core::OperationsHandle;
use openworkers_core::RequestBody;
use std::cell::RefCell;
use std::collections::HashMap;

/// Binding types this runtime answers, which is what the runner checks a
/// worker's declarations against.
pub const SUPPORTED: &[BindingType] = &[
    BindingType::Assets,
    BindingType::Kv,
    #[cfg(feature = "invoke")]
    BindingType::Invoke,
];

/// Install `env` as a global. Variables are plain properties; a binding is an
/// object of functions closing over the handle that serves it.
pub fn install(
    context: &mut Context,
    ops: &OperationsHandle,
    variables: &Option<HashMap<String, String>>,
    bindings: &[BindingInfo],
) -> Result<(), JsError> {
    let mut env = ObjectInitializer::new(context);

    if let Some(variables) = variables {
        for (name, value) in variables {
            env.property(
                js_string!(name.as_str()),
                js_string!(value.as_str()),
                Attribute::all(),
            );
        }
    }

    let env = env.build();

    for binding in bindings {
        let object = match binding.binding_type {
            BindingType::Kv => kv(context, ops, &binding.name),
            BindingType::Assets => assets(context, ops, &binding.name)?,
            #[cfg(feature = "invoke")]
            BindingType::Invoke => invoke(context, ops, &binding.name),
            other => absent(context, &binding.name, other),
        };

        env.set(js_string!(binding.name.as_str()), object, false, context)?;
    }

    context.register_global_property(js_string!("env"), env, Attribute::all())
}

/// Which KV operation a method builds, and from which arguments.
#[derive(Clone, Copy, Debug)]
enum Method {
    Get,
    Put,
    Delete,
    List,
}

/// What one KV method needs to serve a call. None of it is a JavaScript value,
/// so none of it is traced.
#[derive(Finalize, Trace)]
struct Binding {
    #[unsafe_ignore_trace]
    ops: OperationsHandle,
    #[unsafe_ignore_trace]
    name: String,
    #[unsafe_ignore_trace]
    method: Method,
}

/// `get`, `put`, `delete` and `list`, the four the KV op carries.
fn kv(context: &mut Context, ops: &OperationsHandle, name: &str) -> JsObject {
    let mut object = ObjectInitializer::new(context);

    for (js_name, length, method) in [
        ("get", 1usize, Method::Get),
        ("put", 3, Method::Put),
        ("delete", 1, Method::Delete),
        ("list", 1, Method::List),
    ] {
        object.function(
            NativeFunction::from_copy_closure_with_captures(
                |_this, args, held: &Binding, context| {
                    let op = read(held.method, args, context)?;
                    let ops = held.ops.clone();
                    let name = held.name.clone();

                    Ok(JsPromise::from_async_fn(
                        async move |context: &RefCell<&mut Context>| {
                            let answer = ops.handle_binding_kv(&name, op).await;

                            settle(answer, &mut context.borrow_mut())
                        },
                        context,
                    )
                    .into())
                },
                Binding {
                    ops: ops.clone(),
                    name: name.to_string(),
                    method,
                },
            ),
            js_string!(js_name),
            length,
        );
    }

    object.build()
}

/// Build the operation a call asked for.
fn read(method: Method, args: &[JsValue], context: &mut Context) -> JsResult<KvOp> {
    let text = |at: usize, context: &mut Context| -> JsResult<String> {
        Ok(args
            .get(at)
            .cloned()
            .unwrap_or_default()
            .to_string(context)?
            .to_std_string_escaped())
    };

    let number = |object: &JsObject, key: &'static str, context: &mut Context| -> Option<f64> {
        object
            .get(js_string!(key), context)
            .ok()
            .and_then(|value| value.to_number(context).ok())
            .filter(|number| number.is_finite() && *number > 0.0)
    };

    Ok(match method {
        Method::Get => KvOp::Get {
            key: text(0, context)?,
        },
        Method::Delete => KvOp::Delete {
            key: text(0, context)?,
        },
        Method::Put => {
            let key = text(0, context)?;
            let value = args
                .get(1)
                .cloned()
                .unwrap_or_default()
                .to_json(context)?
                .unwrap_or(serde_json::Value::Null);

            // The third argument is the options bag, where expirationTtl is the
            // only key the KV op carries
            let expires_in = match args.get(2).and_then(JsValue::as_object) {
                Some(options) => number(&options, "expirationTtl", context).map(|ttl| ttl as u64),
                None => None,
            };

            KvOp::Put {
                key,
                value,
                expires_in,
            }
        }
        Method::List => match args.first().and_then(JsValue::as_object) {
            Some(options) => {
                let prefix = options
                    .get(js_string!("prefix"), context)
                    .ok()
                    .filter(|value| !value.is_null_or_undefined())
                    .and_then(|value| value.to_string(context).ok())
                    .map(|value| value.to_std_string_escaped());

                KvOp::List {
                    prefix,
                    limit: number(&options, "limit", context).map(|limit| limit as u32),
                }
            }
            None => KvOp::List {
                prefix: None,
                limit: None,
            },
        },
    })
}

/// Turn what the host answered into what the guest asked for, or throw.
fn settle(answer: KvResult, context: &mut Context) -> JsResult<JsValue> {
    match answer {
        KvResult::Value(Some(value)) => JsValue::from_json(&value, context),
        KvResult::Value(None) => Ok(JsValue::null()),
        KvResult::Keys(keys) => {
            let array = JsArray::from_iter(
                keys.iter()
                    .map(|key| JsValue::from(js_string!(key.as_str()))),
                context,
            );

            Ok(array.into())
        }
        KvResult::Ok => Ok(JsValue::undefined()),
        KvResult::Error(message) => Err(js_error!(TypeError: "{}", message)),
    }
}

/// What a call of a binding other than KV needs. None of it is a JavaScript
/// value, so none of it is traced.
#[derive(Finalize, Trace)]
struct Host {
    #[unsafe_ignore_trace]
    ops: OperationsHandle,
    #[unsafe_ignore_trace]
    name: String,
}

/// The guest side of an assets binding. `native(url, method, headers)` asks the
/// host; this turns a Request or a path into those arguments, and the answer
/// into a Response.
const ASSETS_JS: &str = r#"
(function (native) {
    return {
        fetch(input, init) {
            // A bare path is a path on the host of the binding, which new Request
            // refuses. The guest can replace URL and Request, so the host checks
            // the path it gets.
            const request = input instanceof Request
                ? input
                : {
                    url: new URL(String(input), 'http://localhost/').href,
                    method: (init && init.method) || 'GET',
                    headers: new Headers(init && init.headers),
                };
            const headers = {};

            request.headers.forEach((value, key) => { headers[key] = value; });

            return native(request.url, String(request.method), headers).then((answer) =>
                new Response(answer.body.length > 0 ? answer.body : null, {
                    status: answer.status,
                    headers: answer.headers,
                }));
        },
    };
})
"#;

/// `fetch`, which reads a static asset from the host.
fn assets(context: &mut Context, ops: &OperationsHandle, name: &str) -> JsResult<JsObject> {
    let native = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_this, args, held: &Host, context| {
                let request = asset_request(args, context)?;
                let ops = held.ops.clone();
                let name = held.name.clone();

                Ok(JsPromise::from_async_fn(
                    async move |context: &RefCell<&mut Context>| {
                        let response = ops
                            .handle_binding_fetch(&name, request)
                            .await
                            .map_err(|message| js_error!(TypeError: "{}", message))?;
                        let body = response
                            .body
                            .collect()
                            .await
                            .map_err(|message| js_error!(TypeError: "{}", message))?
                            .unwrap_or_default();

                        let context = &mut context.borrow_mut();
                        let body = JsUint8Array::from_iter(body, context)?;
                        let pairs: Vec<JsValue> = response
                            .headers
                            .iter()
                            .map(|(name, value)| {
                                JsArray::from_iter(
                                    [
                                        JsValue::from(js_string!(name.as_str())),
                                        JsValue::from(js_string!(value.as_str())),
                                    ],
                                    context,
                                )
                                .into()
                            })
                            .collect();
                        let headers = JsArray::from_iter(pairs, context);

                        Ok(ObjectInitializer::new(context)
                            .property(js_string!("status"), response.status, Attribute::all())
                            .property(js_string!("headers"), headers, Attribute::all())
                            .property(js_string!("body"), body, Attribute::all())
                            .build()
                            .into())
                    },
                    context,
                )
                .into())
            },
            Host {
                ops: ops.clone(),
                name: name.to_string(),
            },
        ),
    )
    .length(3)
    .build();

    let guest = context
        .eval(Source::from_bytes(ASSETS_JS))?
        .as_object()
        .and_then(JsFunction::from_object)
        .ok_or_else(|| js_error!(TypeError: "the assets binding is not a function"))?;

    guest
        .call(&JsValue::undefined(), &[native.into()], context)?
        .as_object()
        .ok_or_else(|| js_error!(TypeError: "the assets binding is not an object"))
}

/// The request of `native(url, method, headers)`.
fn asset_request(args: &[JsValue], context: &mut Context) -> JsResult<HttpRequest> {
    let url = args
        .get_or_undefined(0)
        .to_string(context)?
        .to_std_string_escaped();
    let method = args
        .get_or_undefined(1)
        .to_string(context)?
        .to_std_string_escaped();
    let headers = match args.get_or_undefined(2).to_json(context)? {
        Some(serde_json::Value::Object(headers)) => headers
            .into_iter()
            .filter_map(|(name, value)| Some((name, value.as_str()?.to_string())))
            .collect(),
        _ => HashMap::new(),
    };

    Ok(HttpRequest {
        method: method
            .parse()
            .map_err(|_| js_error!(TypeError: "fetch does not support the {} method", method))?,
        url,
        headers,
        body: RequestBody::None,
    })
}

/// `invoke(command, args)`, which calls a function of the host by name. The
/// arguments and the answer cross as JSON.
#[cfg(feature = "invoke")]
fn invoke(context: &mut Context, ops: &OperationsHandle, name: &str) -> JsObject {
    ObjectInitializer::new(context)
        .function(
            NativeFunction::from_copy_closure_with_captures(
                |_this, args, held: &Host, context| {
                    let Some(command) = args.get_or_undefined(0).as_string() else {
                        return Ok(JsPromise::reject(
                            js_error!(TypeError: "invoke: the command is not a string"),
                            context,
                        )?
                        .into());
                    };

                    let args = match args.get_or_undefined(1).to_json(context) {
                        Ok(args) => args.unwrap_or(serde_json::Value::Null),
                        Err(error) => return Ok(JsPromise::reject(error, context)?.into()),
                    };

                    let command = command.to_std_string_escaped();
                    let ops = held.ops.clone();
                    let name = held.name.clone();

                    Ok(JsPromise::from_async_fn(
                        async move |context: &RefCell<&mut Context>| match ops
                            .handle_binding_invoke(&name, &command, args)
                            .await
                        {
                            InvokeResult::Value(value) => {
                                JsValue::from_json(&value, &mut context.borrow_mut())
                            }
                            InvokeResult::Error(message) => Err(js_error!(Error: "{}", message)),
                        },
                        context,
                    )
                    .into())
                },
                Host {
                    ops: ops.clone(),
                    name: name.to_string(),
                },
            ),
            js_string!("invoke"),
            2,
        )
        .build()
}

/// A binding type this runtime does not serve. The runner refuses one before a
/// worker reaches here, so this answers a runner newer than this runtime, and
/// says which type is missing rather than leaving a TypeError on undefined.
fn absent(context: &mut Context, name: &str, binding_type: BindingType) -> JsObject {
    let message = format!("the {binding_type:?} binding '{name}' is not served by this runtime");

    ObjectInitializer::new(context)
        .function(
            NativeFunction::from_copy_closure_with_captures(
                |_this, _args, held: &JsString, _context| {
                    Err(js_error!(TypeError: "{}", held.to_std_string_escaped()))
                },
                boa_engine::JsString::from(message.as_str()),
            ),
            js_string!("get"),
            1,
        )
        .build()
}
