//! Web APIs: upstream's where `boa_runtime` has them, JavaScript evaluated at
//! init where it does not.

use boa_engine::Context;

pub fn setup_web_apis(context: &mut Context) -> Result<(), boa_engine::JsError> {
    boa_runtime::base64::register(None, context)?;
    boa_runtime::clone::register(None, context)?;
    boa_runtime::microtask::register(None, context)?;

    setup_global_aliases(context)?;
    setup_dom_exception(context)?;
    setup_url(context)?;
    setup_blob(context)?;
    setup_form_data(context)?;
    setup_abort_controller(context)?;
    setup_headers(context)?;
    setup_request(context)?;
    setup_response(context)?;
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

/// `boa_runtime` parses URLs but leaves `searchParams` unimplemented, so
/// URLSearchParams is layered on top with the URL as the source of truth: reads
/// re-parse `search`, writes assign it back.
fn setup_url(context: &mut Context) -> Result<(), boa_engine::JsError> {
    boa_runtime::url::Url::register(None, context)?;

    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.URLSearchParams = class URLSearchParams {
            constructor(init) {
                this._entries = [];
                this._url = null;
                this._lastSearch = null;

                if (!init) return;

                if (typeof init === 'string') {
                    this._entries = URLSearchParams._parse(init);
                } else if (init instanceof URLSearchParams) {
                    this._entries = init._all().map(([k, v]) => [k, v]);
                } else if (Array.isArray(init)) {
                    for (const [key, value] of init) {
                        this._entries.push([String(key), String(value)]);
                    }
                } else if (typeof init === 'object') {
                    for (const key of Object.keys(init)) {
                        this._entries.push([key, String(init[key])]);
                    }
                }
            }

            static _parse(str) {
                const query = str.startsWith('?') ? str.slice(1) : str;
                const entries = [];

                if (!query) return entries;

                for (const pair of query.split('&')) {
                    if (!pair) continue;

                    const eq = pair.indexOf('=');
                    const rawKey = eq === -1 ? pair : pair.slice(0, eq);
                    const rawValue = eq === -1 ? '' : pair.slice(eq + 1);

                    entries.push([
                        decodeURIComponent(rawKey.replace(/\+/g, ' ')),
                        decodeURIComponent(rawValue.replace(/\+/g, ' '))
                    ]);
                }

                return entries;
            }

            static _encode(str) {
                // encodeURIComponent keeps !'()~, which the urlencoded serializer escapes
                return encodeURIComponent(str)
                    .replace(/[!'()~]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase())
                    .replace(/%20/g, '+');
            }

            _all() {
                if (this._url && this._url.search !== this._lastSearch) {
                    this._lastSearch = this._url.search;
                    this._entries = URLSearchParams._parse(this._lastSearch);
                }

                return this._entries;
            }

            _serialize() {
                return this._entries
                    .map(([k, v]) => URLSearchParams._encode(k) + '=' + URLSearchParams._encode(v))
                    .join('&');
            }

            _sync() {
                if (!this._url) return;

                const query = this._serialize();
                this._lastSearch = query ? '?' + query : '';
                this._url.search = this._lastSearch;
            }

            append(name, value) {
                this._all().push([String(name), String(value)]);
                this._sync();
            }

            delete(name) {
                const key = String(name);
                this._entries = this._all().filter(([k]) => k !== key);
                this._sync();
            }

            get(name) {
                const entry = this._all().find(([k]) => k === String(name));
                return entry ? entry[1] : null;
            }

            getAll(name) {
                return this._all().filter(([k]) => k === String(name)).map(([, v]) => v);
            }

            has(name) {
                return this._all().some(([k]) => k === String(name));
            }

            set(name, value) {
                const key = String(name);
                const entries = this._all();
                const idx = entries.findIndex(([k]) => k === key);

                if (idx === -1) {
                    entries.push([key, String(value)]);
                } else {
                    entries[idx][1] = String(value);
                    this._entries = entries.filter(([k], i) => k !== key || i === idx);
                }

                this._sync();
            }

            sort() {
                this._all().sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
                this._sync();
            }

            toString() {
                this._all();
                return this._serialize();
            }

            *entries() {
                yield* this._all().map(([k, v]) => [k, v]);
            }

            *keys() {
                for (const [k] of this._all()) yield k;
            }

            *values() {
                for (const [, v] of this._all()) yield v;
            }

            forEach(callback, thisArg) {
                for (const [key, value] of this._all()) {
                    callback.call(thisArg, value, key, this);
                }
            }

            [Symbol.iterator]() {
                return this.entries();
            }

            get size() {
                return this._all().length;
            }
        };

        (function() {
            const bound = new WeakMap();

            Object.defineProperty(URL.prototype, 'searchParams', {
                configurable: true,
                get: function() {
                    let params = bound.get(this);

                    if (!params) {
                        params = new URLSearchParams(this.search);
                        params._url = this;
                        params._lastSearch = this.search;
                        bound.set(this, params);
                    }

                    return params;
                }
            });
        })();
        "#,
    ))?;
    Ok(())
}

fn setup_blob(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.Blob = class Blob {
            constructor(blobParts = [], options = {}) {
                this.type = options.type || '';
                this._parts = [];

                for (const part of blobParts) {
                    if (part instanceof Blob) {
                        this._parts.push(...part._parts);
                    } else if (part instanceof ArrayBuffer) {
                        this._parts.push(new Uint8Array(part));
                    } else if (ArrayBuffer.isView(part)) {
                        this._parts.push(new Uint8Array(part.buffer, part.byteOffset, part.byteLength));
                    } else {
                        this._parts.push(new TextEncoder().encode(String(part)));
                    }
                }
            }

            get size() {
                return this._parts.reduce((sum, part) => sum + part.byteLength, 0);
            }

            slice(start = 0, end = this.size, contentType = '') {
                const bytes = this._getBytes();
                const sliced = bytes.slice(start, end);
                return new Blob([sliced], { type: contentType });
            }

            async arrayBuffer() {
                return this._getBytes().buffer;
            }

            async text() {
                return new TextDecoder().decode(this._getBytes());
            }

            stream() {
                var bytes = this._getBytes();
                return new ReadableStream({
                    start: function(c) { c.enqueue(bytes); c.close(); }
                });
            }

            _getBytes() {
                if (this._parts.length === 0) return new Uint8Array(0);
                if (this._parts.length === 1) return this._parts[0];

                const totalLength = this._parts.reduce((sum, p) => sum + p.byteLength, 0);
                const result = new Uint8Array(totalLength);
                let offset = 0;
                for (const part of this._parts) {
                    result.set(part, offset);
                    offset += part.byteLength;
                }
                return result;
            }
        };

        globalThis.File = class File extends Blob {
            constructor(fileBits, fileName, options = {}) {
                super(fileBits, options);
                this.name = fileName;
                this.lastModified = options.lastModified || Date.now();
            }
        };
        "#,
    ))?;
    Ok(())
}

fn setup_form_data(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.FormData = class FormData {
            constructor() {
                this._entries = [];
            }

            append(name, value, filename) {
                if (value instanceof Blob && filename === undefined && value instanceof File) {
                    filename = value.name;
                }
                this._entries.push([String(name), value, filename]);
            }

            delete(name) {
                const strName = String(name);
                this._entries = this._entries.filter(([k]) => k !== strName);
            }

            get(name) {
                const strName = String(name);
                const entry = this._entries.find(([k]) => k === strName);
                return entry ? entry[1] : null;
            }

            getAll(name) {
                const strName = String(name);
                return this._entries.filter(([k]) => k === strName).map(([, v]) => v);
            }

            has(name) {
                const strName = String(name);
                return this._entries.some(([k]) => k === strName);
            }

            set(name, value, filename) {
                const strName = String(name);
                if (value instanceof Blob && filename === undefined && value instanceof File) {
                    filename = value.name;
                }
                this._entries = this._entries.filter(([k]) => k !== strName);
                this._entries.push([strName, value, filename]);
            }

            *entries() {
                for (const [name, value] of this._entries) {
                    yield [name, value];
                }
            }

            *keys() {
                for (const [name] of this._entries) {
                    yield name;
                }
            }

            *values() {
                for (const [, value] of this._entries) {
                    yield value;
                }
            }

            forEach(callback, thisArg) {
                for (const [name, value] of this._entries) {
                    callback.call(thisArg, value, name, this);
                }
            }

            [Symbol.iterator]() {
                return this.entries();
            }
        };
        "#,
    ))?;
    Ok(())
}

fn setup_abort_controller(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.AbortSignal = class AbortSignal {
            constructor() {
                this.aborted = false;
                this.reason = undefined;
                this._listeners = [];
            }

            addEventListener(type, listener) {
                if (type === 'abort') {
                    this._listeners.push(listener);
                }
            }

            removeEventListener(type, listener) {
                if (type === 'abort') {
                    this._listeners = this._listeners.filter(l => l !== listener);
                }
            }

            throwIfAborted() {
                if (this.aborted) {
                    throw this.reason;
                }
            }

            _abort(reason) {
                if (this.aborted) return;
                this.aborted = true;
                this.reason = reason;
                const event = { type: 'abort', target: this };
                for (const listener of this._listeners) {
                    try { listener(event); } catch (e) { console.error(e); }
                }
            }

            static abort(reason) {
                const signal = new AbortSignal();
                signal._abort(reason || new DOMException('Aborted', 'AbortError'));
                return signal;
            }

            static timeout(ms) {
                const signal = new AbortSignal();
                setTimeout(() => {
                    signal._abort(new DOMException('Timeout', 'TimeoutError'));
                }, ms);
                return signal;
            }
        };

        globalThis.AbortController = class AbortController {
            constructor() {
                this.signal = new AbortSignal();
            }

            abort(reason) {
                this.signal._abort(reason || new DOMException('Aborted', 'AbortError'));
            }
        };
        "#,
    ))?;
    Ok(())
}

fn setup_headers(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.Headers = class Headers {
            constructor(init) {
                this._map = new Map();

                if (init) {
                    if (init instanceof Headers) {
                        for (const [key, value] of init) {
                            this._map.set(key, value);
                        }
                    } else if (Array.isArray(init)) {
                        for (const [key, value] of init) {
                            this.append(key, value);
                        }
                    } else if (typeof init === 'object') {
                        for (const key of Object.keys(init)) {
                            this.append(key, init[key]);
                        }
                    }
                }
            }

            _normalizeKey(name) {
                return String(name).toLowerCase();
            }

            append(name, value) {
                const key = this._normalizeKey(name);
                const strValue = String(value);
                if (this._map.has(key)) {
                    this._map.set(key, this._map.get(key) + ', ' + strValue);
                } else {
                    this._map.set(key, strValue);
                }
            }

            delete(name) {
                this._map.delete(this._normalizeKey(name));
            }

            get(name) {
                const value = this._map.get(this._normalizeKey(name));
                return value !== undefined ? value : null;
            }

            has(name) {
                return this._map.has(this._normalizeKey(name));
            }

            set(name, value) {
                this._map.set(this._normalizeKey(name), String(value));
            }

            *entries() {
                yield* this._map.entries();
            }

            *keys() {
                yield* this._map.keys();
            }

            *values() {
                yield* this._map.values();
            }

            forEach(callback, thisArg) {
                for (const [key, value] of this._map) {
                    callback.call(thisArg, value, key, this);
                }
            }

            [Symbol.iterator]() {
                return this.entries();
            }

            getSetCookie() {
                const cookies = [];
                const value = this._map.get('set-cookie');
                if (value) {
                    cookies.push(value);
                }
                return cookies;
            }
        };
        "#,
    ))?;
    Ok(())
}

fn setup_request(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.Request = class Request {
            constructor(input, init) {
                init = init || {};

                if (input instanceof Request) {
                    this.url = input.url;
                    this.method = init.method || input.method;
                    this.headers = new Headers(init.headers || input.headers);
                    if (init.body !== undefined) {
                        this._initBody(init.body);
                    } else if (input.body && !input.bodyUsed) {
                        this._initBody(input.body);
                    } else {
                        this.body = null;
                    }
                } else {
                    this.url = String(input);
                    this.method = (init.method || 'GET').toUpperCase();
                    this.headers = new Headers(init.headers);
                    this._initBody(init.body);
                }

                this.bodyUsed = false;
                this.mode = init.mode || 'cors';
                this.credentials = init.credentials || 'same-origin';
                this.cache = init.cache || 'default';
                this.redirect = init.redirect || 'follow';
                this.referrer = init.referrer || 'about:client';
                this.integrity = init.integrity || '';
            }

            _initBody(body) {
                if (body instanceof ReadableStream) {
                    this.body = body;
                } else if (body instanceof Uint8Array || body instanceof ArrayBuffer) {
                    var _bytes = body instanceof Uint8Array ? body : new Uint8Array(body);
                    this.body = new ReadableStream({
                        start: function(c) { c.enqueue(_bytes); c.close(); }
                    });
                } else if (body === null || body === undefined) {
                    this.body = null;
                } else {
                    var _enc = new TextEncoder();
                    var _b = _enc.encode(String(body));
                    this.body = new ReadableStream({
                        start: function(c) { c.enqueue(_b); c.close(); }
                    });
                }
            }

            async text() {
                if (this.bodyUsed) {
                    throw new TypeError('Body has already been consumed');
                }
                this.bodyUsed = true;

                if (!this.body) return '';

                const reader = this.body.getReader();
                const chunks = [];

                try {
                    while (true) {
                        const { done, value } = await reader.read();
                        if (done) break;
                        chunks.push(value);
                    }
                } finally {
                    reader.releaseLock();
                }

                const totalLength = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
                const result = new Uint8Array(totalLength);
                let offset = 0;
                for (const chunk of chunks) {
                    result.set(chunk, offset);
                    offset += chunk.length;
                }

                return new TextDecoder().decode(result);
            }

            async json() {
                const text = await this.text();
                return JSON.parse(text);
            }

            async arrayBuffer() {
                if (this.bodyUsed) {
                    throw new TypeError('Body has already been consumed');
                }
                this.bodyUsed = true;

                if (!this.body) return new ArrayBuffer(0);

                const reader = this.body.getReader();
                const chunks = [];

                try {
                    while (true) {
                        const { done, value } = await reader.read();
                        if (done) break;
                        chunks.push(value);
                    }
                } finally {
                    reader.releaseLock();
                }

                const totalLength = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
                const result = new Uint8Array(totalLength);
                let offset = 0;
                for (const chunk of chunks) {
                    result.set(chunk, offset);
                    offset += chunk.length;
                }

                return result.buffer;
            }

            clone() {
                if (this.bodyUsed) {
                    throw new TypeError('Cannot clone a Request whose body has been consumed');
                }

                let clonedBody = null;
                if (this.body) {
                    const [stream1, stream2] = this.body.tee();
                    this.body = stream1;
                    clonedBody = stream2;
                }

                return new Request(this.url, {
                    method: this.method,
                    headers: new Headers(this.headers),
                    body: clonedBody,
                    mode: this.mode,
                    credentials: this.credentials,
                    cache: this.cache,
                    redirect: this.redirect,
                    referrer: this.referrer,
                    integrity: this.integrity
                });
            }
        };
        "#,
    ))?;
    Ok(())
}

fn setup_response(context: &mut Context) -> Result<(), boa_engine::JsError> {
    context.eval(boa_engine::Source::from_bytes(
        r#"
        globalThis.Response = class Response {
            constructor(body, init) {
                init = init || {};
                this.status = init.status ?? 200;
                this.statusText = init.statusText || '';
                this.ok = this.status >= 200 && this.status < 300;
                this.bodyUsed = false;

                this.url = init.url || '';
                this.type = init.type || 'default';
                this.redirected = init.redirected || false;

                if (init.headers instanceof Headers) {
                    this.headers = init.headers;
                } else {
                    this.headers = new Headers(init.headers);
                }

                if (body instanceof ReadableStream) {
                    this.body = body;
                } else if (body instanceof Uint8Array || body instanceof ArrayBuffer) {
                    var _bytes = body instanceof Uint8Array ? body : new Uint8Array(body);
                    this.body = new ReadableStream({
                        start: function(c) { c.enqueue(_bytes); c.close(); }
                    });
                } else if (body === null || body === undefined) {
                    this.body = null;
                } else {
                    var _enc = new TextEncoder();
                    var _b = _enc.encode(String(body));
                    this.body = new ReadableStream({
                        start: function(c) { c.enqueue(_b); c.close(); }
                    });
                }
            }

            async text() {
                if (this.bodyUsed) {
                    throw new TypeError('Body has already been consumed');
                }
                this.bodyUsed = true;

                if (!this.body) return '';

                const reader = this.body.getReader();
                const chunks = [];

                try {
                    while (true) {
                        const { done, value } = await reader.read();
                        if (done) break;
                        chunks.push(value);
                    }
                } finally {
                    reader.releaseLock();
                }

                const totalLength = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
                const result = new Uint8Array(totalLength);
                let offset = 0;
                for (const chunk of chunks) {
                    result.set(chunk, offset);
                    offset += chunk.length;
                }

                return new TextDecoder().decode(result);
            }

            async arrayBuffer() {
                if (this.bodyUsed) {
                    throw new TypeError('Body has already been consumed');
                }
                this.bodyUsed = true;

                if (!this.body) return new ArrayBuffer(0);

                const reader = this.body.getReader();
                const chunks = [];

                try {
                    while (true) {
                        const { done, value } = await reader.read();
                        if (done) break;
                        chunks.push(value);
                    }
                } finally {
                    reader.releaseLock();
                }

                const totalLength = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
                const result = new Uint8Array(totalLength);
                let offset = 0;
                for (const chunk of chunks) {
                    result.set(chunk, offset);
                    offset += chunk.length;
                }

                return result.buffer;
            }

            async json() {
                const text = await this.text();
                return JSON.parse(text);
            }

            clone() {
                if (this.bodyUsed) {
                    throw new TypeError('Cannot clone a Response whose body has been consumed');
                }

                let clonedBody = null;
                if (this.body) {
                    const [stream1, stream2] = this.body.tee();
                    this.body = stream1;
                    clonedBody = stream2;
                }

                return new Response(clonedBody, {
                    status: this.status,
                    statusText: this.statusText,
                    headers: new Headers(this.headers),
                    url: this.url,
                    type: this.type,
                    redirected: this.redirected
                });
            }

            static json(data, init) {
                init = init || {};
                const body = JSON.stringify(data);
                const headers = new Headers(init.headers);
                if (!headers.has('content-type')) {
                    headers.set('content-type', 'application/json');
                }
                return new Response(body, {
                    status: init.status || 200,
                    statusText: init.statusText || '',
                    headers: headers
                });
            }

            static redirect(url, status) {
                status = status || 302;
                if (![301, 302, 303, 307, 308].includes(status)) {
                    throw new RangeError('Invalid redirect status code');
                }
                const headers = new Headers({ 'Location': url });
                return new Response(null, {
                    status: status,
                    headers: headers
                });
            }

            static error() {
                return new Response(null, {
                    status: 0,
                    type: 'error'
                });
            }
        };
        "#,
    ))?;
    Ok(())
}
