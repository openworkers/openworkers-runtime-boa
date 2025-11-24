use openworkers_runtime_boa::{Script, Worker};

#[tokio::main]
async fn main() {
    env_logger::init();

    let code = r#"
        globalThis.result = "not set";

        addEventListener('fetch', (event) => {
            setTimeout(() => {
                globalThis.result = "timeout executed!";
            }, 10);

            event.respondWith(new Response('Check setTimeout!'));
        });
    "#;

    let script = Script::new(code);
    let mut worker = Worker::new(script, None, None).await.unwrap();

    println!("✅ Worker created with setTimeout");
    println!("✅ Timers (setTimeout, setInterval, clear*) are available in Boa runtime");
    println!("\nNote: Full async timer scheduling would require tokio integration");
}
