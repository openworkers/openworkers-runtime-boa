mod common;

use common::eval;

#[tokio::test]
async fn test_base64_round_trip() {
    let out = eval("return atob(btoa('hello world'));").await;
    assert_eq!(out, "hello world");
}

#[tokio::test]
async fn test_atob_rejects_bad_padding() {
    let out = eval("return atob('a');").await;
    assert!(out.starts_with("threw InvalidCharacterError"), "{}", out);
}

#[tokio::test]
async fn test_structured_clone_is_deep() {
    let out = eval(
        "const src = { list: [1, 2], when: new Date(0) };
         const copy = structuredClone(src);
         copy.list.push(3);
         return [src.list.length, copy.list.length, copy.when.getTime()].join('|');",
    )
    .await;

    assert_eq!(out, "2|3|0");
}

#[tokio::test]
async fn test_queue_microtask_runs_before_return() {
    let out = eval(
        "let seen = 'no';
         queueMicrotask(() => { seen = 'yes'; });
         await Promise.resolve();
         return seen;",
    )
    .await;

    assert_eq!(out, "yes");
}

#[tokio::test]
async fn test_response_error_keeps_its_status() {
    let out = eval(
        "const r = Response.error();
         return [r.status, r.ok, r.type].join('|');",
    )
    .await;

    assert_eq!(out, "0|false|error");
}

#[tokio::test]
async fn test_abort_controller_reaches_its_listeners() {
    let out = eval(
        "const controller = new AbortController();
         let seen = 'no';
         controller.signal.addEventListener('abort', () => { seen = 'yes'; });
         controller.abort();
         await new Promise((resolve) => setTimeout(resolve, 0));
         return [controller.signal.aborted, controller.signal.reason.name, seen].join('|');",
    )
    .await;

    assert_eq!(out, "true|AbortError|yes");
}

#[tokio::test]
async fn test_aborted_signal_rejects_fetch() {
    let out = eval(
        "const controller = new AbortController();
         controller.abort();

         try {
             await fetch('https://example.com/', { signal: controller.signal });
             return 'resolved';
         } catch (e) {
             return e.name;
         }",
    )
    .await;

    assert_eq!(out, "AbortError");
}

#[tokio::test]
async fn test_headers_join_repeats_but_not_set_cookie() {
    let out = eval(
        "const h = new Headers();
         h.append('link', '<a>');
         h.append('link', '<b>');
         h.append('set-cookie', 'a=1');
         h.append('set-cookie', 'b=2');
         return [h.get('link'), h.get('set-cookie'), JSON.stringify([...h])].join('|');",
    )
    .await;

    assert_eq!(
        out,
        "<a>, <b>|a=1, b=2|[[\"link\",\"<a>, <b>\"],[\"set-cookie\",\"a=1\"],[\"set-cookie\",\"b=2\"]]"
    );
}

#[tokio::test]
async fn test_headers_get_set_cookie_lists_every_cookie() {
    let out = eval(
        "const h = new Headers();
         h.append('set-cookie', 'a=1');
         h.append('set-cookie', 'b=2');
         const copy = new Headers(h);
         copy.set('set-cookie', 'c=3');
         return [JSON.stringify(h.getSetCookie()), JSON.stringify(copy.getSetCookie())].join('|');",
    )
    .await;

    assert_eq!(out, "[\"a=1\",\"b=2\"]|[\"c=3\"]");
}
