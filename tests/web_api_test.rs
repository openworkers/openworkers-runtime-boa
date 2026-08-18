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
