use llrt_modules::module_builder::ModuleBuilder;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module};

/// Heap limit of the JS runtime; scripts that allocate more fail with an out-of-memory error.
const MEMORY_LIMIT: usize = 512 * 1024 * 1024;

/// Runs a JS module with LLRT's Node/WinterTC APIs (fetch, timers, fs, ...).
/// `path` `-` reads the source from stdin.
pub async fn run_js(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let source = if path == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(path)?
    };
    let runtime = AsyncRuntime::new()?;
    runtime.set_memory_limit(MEMORY_LIMIT).await;
    let (resolver, loader, globals) = ModuleBuilder::default().build();
    runtime.set_loader((resolver,), (loader,)).await;
    let context = AsyncContext::full(&runtime).await?;

    context
        .async_with(async |ctx| {
            globals
                .attach(&ctx)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            let promise = Module::evaluate(ctx.clone(), path, source)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            promise
                .into_future::<()>()
                .await
                .catch(&ctx)
                .map_err(|e| e.to_string())
        })
        .await?;
    // Wait for pending timers, fetches and other spawned jobs.
    runtime.idle().await;
    Ok(())
}
